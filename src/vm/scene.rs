//! Экспорт мира VM в данные для viewer.
//!
//! Здесь «понятия OMM» (фигуры, деформация, масштаб, `scale`/`: N`)
//! превращаются в то, что viewer умеет рисовать без каких-либо знаний о
//! формате: треугольные сетки, матрицы и цвета.
//!
//! * Процедурные фигуры без деформации используют **общую** сетку
//!   (куб, сфера… загружаются на GPU по одному разу), а масштаб и поворот
//!   задаются матрицей экземпляра.
//! * Фигуры с растяжением (`ur`, `ul`, …) получают собственную сетку в
//!   пикселях — деформация зависит от знака координаты и не выражается
//!   матрицей.
//! * `mesh3` получает собственную сетку (яркость уже посчитана заранее).

use super::geometry::{geometry, tessellate, tessellate_mesh};
use super::world::{Object, Shape, World};
use crate::viewer::{Instance, Mesh, Scene, IDENTITY};

/// Собирает сцену для viewer. `revision` должен меняться при каждой замене
/// мира — по нему viewer понимает, что нужно перезагрузить геометрию.
pub fn compose(world: &World, revision: u64) -> Scene {
    let mut scene = Scene {
        meshes: Vec::new(),
        instances: Vec::with_capacity(world.objects.len()),
        textures: world.textures.clone(),
        revision,
    };
    let mut shared: [Option<u32>; 7] = [None; 7];
    for obj in &world.objects {
        let inst = make_instance(&mut scene, &mut shared, obj);
        scene.instances.push(inst);
    }
    scene
}

/// Обновляет только матрицы экземпляров (после `anim::step`). Порядок
/// экземпляров совпадает с порядком объектов мира.
pub fn refresh(world: &World, scene: &mut Scene) {
    for (obj, inst) in world.objects.iter().zip(scene.instances.iter_mut()) {
        inst.model = model_matrix(obj, is_baked(obj));
    }
}

/// Масштаб «запечён» в сетку (фигура с растяжением).
fn is_baked(obj: &Object) -> bool {
    obj.shape != Shape::Mesh && !obj.deform.is_zero()
}

fn push_mesh(scene: &mut Scene, vertices: Vec<f32>) -> u32 {
    scene.meshes.push(Mesh { vertices });
    (scene.meshes.len() - 1) as u32
}

fn rgb_unit(rgb: [u8; 3]) -> [f32; 3] {
    [f32::from(rgb[0]) / 255.0, f32::from(rgb[1]) / 255.0, f32::from(rgb[2]) / 255.0]
}

fn make_instance(scene: &mut Scene, shared: &mut [Option<u32>; 7], obj: &Object) -> Instance {
    let mut inst = Instance {
        mesh: 0,
        model: IDENTITY,
        color: rgb_unit(obj.rgb),
        texture: None,
        texture_required: false,
        visible: false,
    };

    if obj.shape == Shape::Mesh {
        if let Some(mesh) = obj.mesh.as_ref() {
            if !mesh.tris.is_empty() {
                inst.mesh = push_mesh(scene, tessellate_mesh(mesh));
                inst.visible = true;
                // текстура без UV бессмысленна — рисуем цветом (как в OMM 3.x)
                if mesh.uv.is_some() {
                    inst.texture = obj.texture;
                }
            }
        }
    } else if let Some(geo) = geometry(obj.shape) {
        inst.texture = obj.texture;
        inst.texture_required = obj.shape == Shape::Image;
        inst.visible = true;
        if obj.deform.is_zero() {
            let slot = obj.shape as usize;
            inst.mesh = match shared[slot] {
                Some(id) => id,
                None => {
                    let id = push_mesh(scene, tessellate(obj.shape, &geo, &geo.verts));
                    shared[slot] = Some(id);
                    id
                }
            };
        } else {
            let warped = deform_vertices(&geo.verts, obj);
            inst.mesh = push_mesh(scene, tessellate(obj.shape, &geo, &warped));
        }
    }

    inst.model = model_matrix(obj, is_baked(obj));
    inst
}

/// Растяжение сторон (OMM 3.x): сторона, на которую смотрит вершина,
/// получает собственную добавку к масштабу. Результат — в пикселях.
fn deform_vertices(verts: &[[f64; 3]], obj: &Object) -> Vec<[f64; 3]> {
    let s = obj.scale;
    let sy = s * obj.squash;
    let d = obj.deform;
    verts
        .iter()
        .map(|rv| {
            let mut vx = rv[0] * s;
            let mut vy = rv[1] * sy;
            let mut vz = rv[2] * s;
            if vx > 0.0 {
                vx = rv[0] * (s + d.px);
            } else if vx < 0.0 {
                vx = rv[0] * (s + d.nx);
            }
            if sy != 0.0 {
                if vy > 0.0 {
                    vy = rv[1] * (sy + d.py);
                } else if vy < 0.0 {
                    vy = rv[1] * (sy + d.ny);
                }
            }
            if vz > 0.0 {
                vz = rv[2] * (s + d.pz);
            } else if vz < 0.0 {
                vz = rv[2] * (s + d.nz);
            }
            [vx, vy, vz]
        })
        .collect()
}

/// Матрица модели (column-major): `T(pos) · Ry · Rx · S`.
///
/// Порядок поворотов — как в OMM 3.x (сначала наклон `ru` вокруг X, затем
/// поворот `rr` вокруг Y). Если масштаб уже запечён в сетку (`baked`),
/// матрица масштаб не содержит.
pub fn model_matrix(obj: &Object, baked: bool) -> [f32; 16] {
    let (sx, sy, sz) = if baked {
        (1.0, 1.0, 1.0)
    } else {
        (obj.scale, obj.scale * obj.squash, obj.scale)
    };
    let (s_rx, c_rx) = obj.rot_x.sin_cos();
    let (s_ry, c_ry) = obj.rot_y.sin_cos();

    // столбцы R = Ry·Rx
    let c0 = [c_ry, 0.0, -s_ry];
    let c1 = [s_ry * s_rx, c_rx, c_ry * s_rx];
    let c2 = [s_ry * c_rx, -s_rx, c_ry * c_rx];

    [
        (c0[0] * sx) as f32,
        (c0[1] * sx) as f32,
        (c0[2] * sx) as f32,
        0.0,
        (c1[0] * sy) as f32,
        (c1[1] * sy) as f32,
        (c1[2] * sy) as f32,
        0.0,
        (c2[0] * sz) as f32,
        (c2[1] * sz) as f32,
        (c2[2] * sz) as f32,
        0.0,
        obj.pos[0] as f32,
        obj.pos[1] as f32,
        obj.pos[2] as f32,
        1.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::{Camera, FOCAL};
    use crate::parser::parse;
    use crate::viewer::STRIDE;
    use crate::vm::{anim, run};

    fn compose_text(src: &str) -> (World, Scene) {
        let w = run(&parse(src)).world;
        let s = compose(&w, 1);
        (w, s)
    }

    fn one_object(setup: impl FnOnce(&mut Object)) -> World {
        let mut o = Object::new(Shape::Cube);
        setup(&mut o);
        World { objects: vec![o], textures: vec![], animated: false }
    }

    /// Трансформация OMM 3.x по стадиям (поворот вокруг X, затем вокруг Y).
    fn staged_world(p: [f64; 3], o: &Object) -> [f64; 3] {
        let (srx, crx) = o.rot_x.sin_cos();
        let (sry, cry) = o.rot_y.sin_cos();
        let ty = p[1] * crx - p[2] * srx;
        let tz = p[1] * srx + p[2] * crx;
        let tx = p[0] * cry + tz * sry;
        let tz2 = -p[0] * sry + tz * cry;
        [tx + o.pos[0], ty + o.pos[1], tz2 + o.pos[2]]
    }

    fn mul(m: &[f32; 16], v: [f32; 3]) -> [f32; 4] {
        [0usize, 1, 2, 3].map(|r| m[r] * v[0] + m[4 + r] * v[1] + m[8 + r] * v[2] + m[12 + r])
    }

    fn positions(scene: &Scene, mesh: u32) -> Vec<[f32; 3]> {
        scene.meshes[mesh as usize]
            .vertices
            .chunks_exact(STRIDE)
            .map(|v| [v[0], v[1], v[2]])
            .collect()
    }

    #[test]
    fn shared_primitives_are_uploaded_once() {
        let (_, s) = compose_text("cube3\ncube3\nsphere3\ncube3");
        assert_eq!(s.instances.len(), 4);
        assert_eq!(s.meshes.len(), 2);
        assert_eq!(s.instances[0].mesh, s.instances[1].mesh);
        assert_eq!(s.instances[0].mesh, s.instances[3].mesh);
        assert_ne!(s.instances[0].mesh, s.instances[2].mesh);
        assert!(s.instances.iter().all(|i| i.visible));
        assert_eq!(s.revision, 1);
    }

    #[test]
    fn deformed_objects_get_their_own_mesh_in_pixels() {
        let (_, s) = compose_text("cube3\ncube3\nur(50)");
        assert_eq!(s.meshes.len(), 2);
        assert_ne!(s.instances[0].mesh, s.instances[1].mesh);

        // у деформированного — масштаб запечён: в матрице нет множителя 50
        assert!((s.instances[1].model[0] - 1.0).abs() < 1e-6);
        assert!((s.instances[0].model[0] - 50.0).abs() < 1e-6);

        let xs: Vec<f32> = positions(&s, s.instances[1].mesh).iter().map(|p| p[0]).collect();
        let max = xs.iter().copied().fold(f32::MIN, f32::max);
        let min = xs.iter().copied().fold(f32::MAX, f32::min);
        assert_eq!((min, max), (-50.0, 100.0)); // правая сторона: 50 + ur(50)
    }

    #[test]
    fn model_matrix_equals_the_v3_staged_transform() {
        let w = one_object(|o| {
            o.pos = [10.0, -20.0, 30.0];
            o.scale = 70.0;
            o.squash = 0.5;
            o.rot_x = 0.3;
            o.rot_y = -0.7;
        });
        let s = compose(&w, 1);
        let o = &w.objects[0];
        let geo = geometry(Shape::Cube).unwrap();
        for rv in &geo.verts {
            let p = [rv[0] * o.scale, rv[1] * o.scale * o.squash, rv[2] * o.scale];
            let want = staged_world(p, o);
            let got = mul(&s.instances[0].model, [rv[0] as f32, rv[1] as f32, rv[2] as f32]);
            for k in 0..3 {
                assert!((f64::from(got[k]) - want[k]).abs() < 1e-3, "k={k} {got:?} vs {want:?}");
            }
            assert_eq!(got[3], 1.0);
        }
    }

    #[test]
    fn end_to_end_screen_position_matches_v3() {
        let w = one_object(|o| {
            o.pos = [15.0, -25.0, 40.0];
            o.scale = 60.0;
            o.rot_x = 0.2;
            o.rot_y = 0.9;
        });
        let s = compose(&w, 1);
        let o = &w.objects[0];
        let cam = Camera { z: 700.0, rx: 0.35, ry: -0.8, cx: 10.0, cy: -4.0 };
        let (cw, ch) = (800u32, 600u32);
        let vp = cam.view_proj(cw, ch);

        for v in positions(&s, s.instances[0].mesh) {
            // вершина куба → мир (через матрицу экземпляра) → clip → экран
            let world = mul(&s.instances[0].model, v);
            let clip = mul(&vp, [world[0], world[1], world[2]]);
            let sx = (clip[0] / clip[3] * 0.5 + 0.5) * cw as f32;
            let sy = (0.5 - clip[1] / clip[3] * 0.5) * ch as f32;

            // эталон: OMM 3.x
            let p = [f64::from(v[0]) * o.scale, f64::from(v[1]) * o.scale, f64::from(v[2]) * o.scale];
            let wp = staged_world(p, o);
            let (srx, crx) = cam.rx.sin_cos();
            let (sry, cry) = cam.ry.sin_cos();
            let cy = wp[1] * crx - wp[2] * srx;
            let cz = wp[1] * srx + wp[2] * crx;
            let cx = wp[0] * cry + cz * sry;
            let cz = -wp[0] * sry + cz * cry;
            let sc = FOCAL / (FOCAL + cz + cam.z);
            let want = [cx * sc + f64::from(cw) / 2.0 + cam.cx, cy * sc + f64::from(ch) / 2.0 + cam.cy];

            assert!((f64::from(sx) - want[0]).abs() < 0.05, "x {sx} vs {}", want[0]);
            assert!((f64::from(sy) - want[1]).abs() < 0.05, "y {sy} vs {}", want[1]);
        }
    }

    #[test]
    fn instances_follow_animation_after_refresh() {
        let (mut w, mut s) = compose_text("cube3\nanimation(x100)");
        assert_eq!(s.instances[0].model[12], 0.0);
        for _ in 0..10 {
            anim::step(&mut w.objects);
        }
        refresh(&w, &mut s);
        assert!((s.instances[0].model[12] - 20.0).abs() < 1e-4); // 10 тиков × 2 px
        assert_eq!(s.meshes.len(), 1, "refresh не трогает геометрию");
    }

    #[test]
    fn colors_are_normalised() {
        let (_, s) = compose_text("cube3\ncolor(255, 0, 51)");
        assert_eq!(s.instances[0].color, [1.0, 0.0, 0.2]);
    }

    #[test]
    fn texture_rules() {
        // image3 обязан иметь текстуру; куб без текстуры — цветом
        let (_, s) = compose_text("image3\ntexture(a.png)\ncube3\ncylinder3\ntexture(a.png)");
        assert_eq!(s.textures, vec!["a.png".to_string()]);
        assert!(s.instances[0].texture_required && s.instances[0].texture == Some(0));
        assert!(!s.instances[1].texture_required && s.instances[1].texture.is_none());
        assert!(!s.instances[2].texture_required && s.instances[2].texture == Some(0));

        // mesh3: текстура учитывается только при наличии UV
        let with_uv = "mesh3\nverts(0 0 0, 1 0 0, 0 1 0)\nuvs(0 0, 1 0, 0 1)\ntris(0 1 2)\ntexture(t.png)\n";
        let without_uv = "mesh3\nverts(0 0 0, 1 0 0, 0 1 0)\ntris(0 1 2)\ntexture(t.png)\n";
        assert_eq!(compose_text(with_uv).1.instances[0].texture, Some(0));
        assert_eq!(compose_text(without_uv).1.instances[0].texture, None);
    }

    #[test]
    fn mesh3_without_geometry_is_invisible_but_keeps_its_slot() {
        let (w, s) = compose_text("cube3\nmesh3\nsphere3");
        assert_eq!(w.objects.len(), 3);
        assert_eq!(s.instances.len(), 3);
        assert!(s.instances[0].visible && !s.instances[1].visible && s.instances[2].visible);
    }

    #[test]
    fn mesh3_gets_its_own_mesh_with_precomputed_light() {
        let (w, s) = compose_text("mesh3\nverts(0 0 0, 1 0 0, 1 1 0)\ntris(0 1 2)\n");
        let m = w.objects[0].mesh.as_ref().unwrap();
        let v = &s.meshes[s.instances[0].mesh as usize].vertices;
        assert_eq!(v.len(), 3 * STRIDE);
        assert_eq!(v[3], m.shade[0]);
    }

    #[test]
    fn composing_an_empty_world_is_fine() {
        let s = compose(&World::default(), 7);
        assert!(s.meshes.is_empty() && s.instances.is_empty());
        assert_eq!(s.revision, 7);
    }
}
