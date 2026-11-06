//! Viewer: рисует то, что ему дали.
//!
//! Модуль **ничего не знает** ни о формате OMM, ни о байт-коде, ни о VM. На
//! входе — обычные данные: треугольные сетки, экземпляры с матрицами,
//! камера; на выходе — картинка. Поэтому бэкенд можно заменить (другой
//! API, тестовый, экспорт) без изменений остальной системы.
//!
//! # Формат вершины
//!
//! Сетка — плоский список треугольников (по 3 вершины на треугольник, без
//! индексов), каждая вершина — [`STRIDE`] чисел `f32`:
//!
//! ```text
//! [ x, y, z,  light,  u, v,  edge ]
//! ```
//!
//! * `light` — готовая яркость вершины (0..1), множится на цвет экземпляра;
//! * `u, v` — текстурные координаты, нормализованные 0..1 (`0,0` — левый
//!   верхний угол картинки);
//! * `edge` — битовая маска рёбер треугольника, которые рисуются тонкой
//!   контурной линией: бит 0 — ребро напротив вершины 0, бит 1 — напротив
//!   вершины 1, бит 2 — напротив вершины 2 (`7` — все три).
//!
//! # Порядок отрисовки
//!
//! Непрозрачные экземпляры рисуются с буфером глубины в любом порядке.
//! Экземпляры с готовой текстурой могут содержать прозрачные пиксели, поэтому
//! рисуются вторым проходом с blending, отсортированные от дальних к
//! ближним (см. [`make_plan`]).

pub mod shaders;

#[cfg(target_arch = "wasm32")]
pub mod gl;

/// Число `f32` на вершину.
pub const STRIDE: usize = 7;

/// Единичная матрица 4×4 (column-major).
pub const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];

/// Треугольная сетка (см. формат вершины в описании модуля).
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub vertices: Vec<f32>,
}

impl Mesh {
    pub fn vertex_count(&self) -> usize {
        self.vertices.len() / STRIDE
    }
}

/// Один рисуемый объект: сетка + преобразование + материал.
#[derive(Clone, Debug)]
pub struct Instance {
    /// Индекс в [`Scene::meshes`].
    pub mesh: u32,
    /// Матрица модели, column-major.
    pub model: [f32; 16],
    /// Цвет материала, 0..1.
    pub color: [f32; 3],
    /// Индекс в [`Scene::textures`]. Если картинка ещё не загружена,
    /// экземпляр рисуется цветом.
    pub texture: Option<u32>,
    /// Не рисовать, пока текстура не готова (иначе — рисовать цветом).
    pub texture_required: bool,
    pub visible: bool,
}

impl Instance {
    pub fn new(mesh: u32) -> Instance {
        Instance {
            mesh,
            model: IDENTITY,
            color: [1.0; 3],
            texture: None,
            texture_required: false,
            visible: true,
        }
    }
}

/// Всё, что нужно нарисовать.
#[derive(Clone, Debug, Default)]
pub struct Scene {
    pub meshes: Vec<Mesh>,
    pub instances: Vec<Instance>,
    /// Ключи текстур (для viewer — непрозрачные метки; картинки ему
    /// передаёт владелец по индексу).
    pub textures: Vec<String>,
    /// Растёт при каждой замене геометрии: viewer перезагружает буферы.
    pub revision: u64,
}

/// Камера и параметры кадра.
#[derive(Clone, Copy, Debug)]
pub struct View {
    /// Матрица «вид × проекция», column-major.
    pub view_proj: [f32; 16],
    pub width: u32,
    pub height: u32,
    /// Прозрачность контурных линий (0 — выключены).
    pub outline: f32,
}

/// План отрисовки: какие экземпляры и в каком порядке. Считается на CPU,
/// без обращения к графическому API (поэтому тестируется без браузера).
#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// Непрозрачные экземпляры (порядок не важен).
    pub opaque: Vec<u32>,
    /// Экземпляры с готовой текстурой — от дальних к ближним.
    pub blended: Vec<u32>,
    keys: Vec<(f32, u32)>,
}

/// Строит план отрисовки. `texture_ready(i)` сообщает, загружена ли
/// текстура `i`. Буферы плана переиспользуются между кадрами.
pub fn make_plan(
    scene: &Scene,
    view: &View,
    texture_ready: &dyn Fn(u32) -> bool,
    plan: &mut Plan,
) {
    plan.opaque.clear();
    plan.blended.clear();
    plan.keys.clear();

    for (i, inst) in scene.instances.iter().enumerate() {
        if !inst.visible {
            continue;
        }
        match scene.meshes.get(inst.mesh as usize) {
            Some(m) if m.vertex_count() > 0 => {}
            _ => continue,
        }
        let ready = inst.texture.is_some_and(|t| texture_ready(t));
        if inst.texture_required && !ready {
            continue;
        }
        if ready {
            // w-координата начала модели в clip-пространстве = расстояние
            // вдоль взгляда
            let m = &inst.model;
            let v = &view.view_proj;
            let w = v[3] * m[12] + v[7] * m[13] + v[11] * m[14] + v[15];
            plan.keys.push((w, i as u32));
        } else {
            plan.opaque.push(i as u32);
        }
    }

    // дальние первыми; при равенстве — исходный порядок (стабильно)
    plan.keys.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    plan.blended.extend(plan.keys.iter().map(|k| k.1));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_with_depth_from_z() -> View {
        // w = z модели
        let mut vp = [0.0f32; 16];
        vp[11] = 1.0;
        View { view_proj: vp, width: 100, height: 100, outline: 0.15 }
    }

    fn scene_with(instances: Vec<Instance>) -> Scene {
        Scene {
            meshes: vec![Mesh { vertices: vec![0.0; STRIDE * 3] }],
            instances,
            textures: vec!["a".to_string(), "b".to_string()],
            revision: 1,
        }
    }

    fn inst_at(z: f32, tex: Option<u32>) -> Instance {
        let mut i = Instance::new(0);
        i.model[14] = z;
        i.texture = tex;
        i
    }

    fn plan_for(scene: &Scene, ready: &dyn Fn(u32) -> bool) -> Plan {
        let mut p = Plan::default();
        make_plan(scene, &view_with_depth_from_z(), ready, &mut p);
        p
    }

    #[test]
    fn splits_opaque_and_textured() {
        let s = scene_with(vec![inst_at(1.0, None), inst_at(2.0, Some(0)), inst_at(3.0, None)]);
        let p = plan_for(&s, &|_| true);
        assert_eq!(p.opaque, vec![0, 2]);
        assert_eq!(p.blended, vec![1]);
    }

    #[test]
    fn texture_that_is_not_ready_falls_back_to_color() {
        let s = scene_with(vec![inst_at(1.0, Some(0))]);
        let p = plan_for(&s, &|_| false);
        assert_eq!(p.opaque, vec![0]);
        assert!(p.blended.is_empty());
    }

    #[test]
    fn texture_required_hides_until_ready() {
        let mut i = inst_at(1.0, Some(0));
        i.texture_required = true;
        let s = scene_with(vec![i]);
        let hidden = plan_for(&s, &|_| false);
        assert!(hidden.opaque.is_empty() && hidden.blended.is_empty());
        let shown = plan_for(&s, &|t| t == 0);
        assert_eq!(shown.blended, vec![0]);
    }

    #[test]
    fn blended_are_sorted_far_to_near_and_stable() {
        let s = scene_with(vec![
            inst_at(5.0, Some(0)),
            inst_at(50.0, Some(0)),
            inst_at(5.0, Some(1)),
            inst_at(20.0, Some(0)),
        ]);
        let p = plan_for(&s, &|_| true);
        assert_eq!(p.blended, vec![1, 3, 0, 2]);
    }

    #[test]
    fn invisible_and_broken_instances_are_skipped() {
        let mut hidden = inst_at(1.0, None);
        hidden.visible = false;
        let mut bad_mesh = inst_at(1.0, None);
        bad_mesh.mesh = 9;
        let mut s = scene_with(vec![hidden, bad_mesh, inst_at(1.0, None)]);
        s.meshes.push(Mesh::default()); // пустая сетка
        let mut empty = inst_at(1.0, None);
        empty.mesh = 1;
        s.instances.push(empty);
        let p = plan_for(&s, &|_| true);
        assert_eq!(p.opaque, vec![2]);
    }

    #[test]
    fn plan_buffers_are_reused() {
        let s = scene_with(vec![inst_at(1.0, None)]);
        let mut p = Plan::default();
        let v = view_with_depth_from_z();
        make_plan(&s, &v, &|_| true, &mut p);
        make_plan(&s, &v, &|_| true, &mut p);
        assert_eq!(p.opaque, vec![0]); // не накапливается
    }

    #[test]
    fn nan_depth_does_not_panic() {
        let mut a = inst_at(f32::NAN, Some(0));
        a.model[14] = f32::NAN;
        let s = scene_with(vec![a, inst_at(1.0, Some(0))]);
        let p = plan_for(&s, &|_| true);
        assert_eq!(p.blended.len(), 2);
    }
}
