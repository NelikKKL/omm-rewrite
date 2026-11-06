//! Виртуальная машина OMM.
//!
//! VM исполняет байт-код ([`crate::bytecode::Program`]) и строит из него
//! [`World`]: создаёт объекты, применяет свойства, считает групповые
//! преобразования `mono()`, собирает меши. Именно здесь — единственное
//! место, где инструкции обретают смысл.
//!
//! Дальше мир превращается в простые данные для viewer ([`scene::compose`]),
//! а анимация двигает объекты ([`anim::step`]) и обновляет матрицы
//! ([`scene::refresh`]).
//!
//! VM устойчива к любому байт-коду: обрыв, неизвестные инструкции,
//! неверные индексы и лишние/недостающие границы групп дают предупреждения,
//! но не панику.

pub mod anim;
pub mod geometry;
pub mod scene;
pub mod world;

use std::f64::consts::PI;
use std::rc::Rc;

use crate::bytecode::{op, Program, Reader, KF_RR, KF_RU, KF_X, KF_Y, KF_Z};
use crate::mesh::{Mesh, Repairs};
use world::{Animation, Keyframe, Object, Shape, World, DEFAULT_ANIM_SPEED};

/// Максимум объектов в мире (защита от «текстовых бомб»).
pub const MAX_OBJECTS: usize = 50_000;
/// Максимальная вложенность групп.
pub const MAX_GROUP_DEPTH: usize = 16;
const MAX_WARNINGS: usize = 100;
const TRUNCATED: &str = "обрыв байт-кода";

/// Результат запуска программы.
#[derive(Clone, Debug, Default)]
pub struct Run {
    pub world: World,
    /// Замечания VM (без замечаний парсера — они лежат в `Program::warnings`).
    pub warnings: Vec<String>,
}

/// Исполняет программу и возвращает построенный мир. Не паникует.
pub fn run(program: &Program) -> Run {
    let mut m = Machine {
        prog: program,
        objects: Vec::new(),
        textures: Vec::new(),
        tex_map: Vec::new(),
        groups: Vec::new(),
        cur: None,
        frames: None,
        animated: false,
        warnings: Vec::new(),
        objects_capped: false,
        warnings_capped: false,
        pc: 0,
    };
    m.execute();
    Run {
        world: World { objects: m.objects, textures: m.textures, animated: m.animated },
        warnings: m.warnings,
    }
}

// ── внутреннее устройство ────────────────────────────────────────────────────

/// Объект в процессе сборки: свойства + черновики геометрии `mesh3`.
struct Draft {
    obj: Object,
    verts: Option<Vec<[f32; 3]>>,
    uvs: Option<Vec<[f32; 2]>>,
    tris: Option<Vec<[u32; 3]>>,
    /// Смещение инструкции `SHAPE` (для сообщений об ошибках).
    start_pc: usize,
}

/// Преобразование группы `mono( … )`.
#[derive(Clone, Copy)]
struct GroupXf {
    pos: [f64; 3],
    rx: f64,
    ry: f64,
    scale: f64,
}

impl GroupXf {
    fn identity() -> GroupXf {
        GroupXf { pos: [0.0; 3], rx: 0.0, ry: 0.0, scale: 1.0 }
    }

    fn is_identity(&self) -> bool {
        self.pos == [0.0; 3] && self.rx == 0.0 && self.ry == 0.0 && self.scale == 1.0
    }
}

struct Group {
    /// Индекс первого объекта группы в `objects`.
    start: usize,
    xf: GroupXf,
    /// Уже встретилась фигура (или вложенная группа): заголовок закончился.
    shape_seen: bool,
}

/// К чему относится свойство, пришедшее в данный момент.
enum Target {
    Object,
    Group,
    Nothing,
}

struct Machine<'p> {
    prog: &'p Program,
    objects: Vec<Object>,
    textures: Vec<String>,
    /// Индекс строки программы → индекс текстуры мира.
    tex_map: Vec<Option<u32>>,
    groups: Vec<Group>,
    cur: Option<Draft>,
    /// Ключевые кадры между `ANIM_BEGIN` и `ANIM_END`.
    frames: Option<Vec<Keyframe>>,
    animated: bool,
    warnings: Vec<String>,
    objects_capped: bool,
    warnings_capped: bool,
    /// Смещение текущей инструкции.
    pc: usize,
}

fn finite(v: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

impl<'p> Machine<'p> {
    fn warn_at(&mut self, pc: usize, msg: &str) {
        if self.warnings.len() >= MAX_WARNINGS {
            if !self.warnings_capped {
                self.warnings_capped = true;
                self.warnings.push("слишком много предупреждений, остальные скрыты".to_string());
            }
            return;
        }
        let text = match self.prog.line_at(pc) {
            Some(line) => format!("строка {line}: {msg}"),
            None => msg.to_string(),
        };
        self.warnings.push(text);
    }

    fn warn(&mut self, msg: &str) {
        let pc = self.pc;
        self.warn_at(pc, msg);
    }

    fn execute(&mut self) {
        let prog: &'p Program = self.prog;
        let mut r = Reader::new(&prog.code);
        loop {
            self.pc = r.pc;
            let Some(opcode) = r.u8() else { break };
            match self.step(&mut r, opcode) {
                Ok(true) => {}
                Ok(false) => break,
                Err(msg) => {
                    self.warn(&msg);
                    break;
                }
            }
        }
        // всё, что осталось открытым, закрываем (внешний байт-код мог
        // обойтись без GROUP_END)
        self.finish_draft();
        while let Some(g) = self.groups.pop() {
            apply_group(&mut self.objects, g.start, &g.xf);
        }
    }

    /// Выполняет одну инструкцию. `Ok(false)` — конец программы.
    fn step(&mut self, r: &mut Reader, opcode: u8) -> Result<bool, String> {
        match opcode {
            op::END => return Ok(false),
            op::SHAPE => {
                let id = r.u8().ok_or(TRUNCATED)?;
                self.begin_shape(id);
            }
            op::DIVISOR => {
                let n = r.f64().ok_or(TRUNCATED)?;
                self.divisor(n);
            }
            op::GROUP_BEGIN => self.group_begin()?,
            op::GROUP_END => self.group_end(),
            op::X | op::Y | op::Z | op::SCALE | op::ROT_Y | op::ROT_X => {
                let v = r.f64().ok_or(TRUNCATED)?;
                self.set_num(opcode, v);
            }
            op::COLOR => {
                let c = [
                    r.u8().ok_or(TRUNCATED)?,
                    r.u8().ok_or(TRUNCATED)?,
                    r.u8().ok_or(TRUNCATED)?,
                ];
                self.color(c);
            }
            op::TEXTURE => {
                let i = r.u32().ok_or(TRUNCATED)?;
                self.texture(i);
            }
            op::DEFORM => {
                let which = r.u8().ok_or(TRUNCATED)?;
                let v = r.f64().ok_or(TRUNCATED)?;
                self.deform(which, v);
            }
            op::ANIM_BEGIN => self.frames = Some(Vec::new()),
            op::KEYFRAME => {
                let mask = r.u8().ok_or(TRUNCATED)?;
                let mut kf = Keyframe::default();
                if mask & KF_X != 0 {
                    kf.x = Some(finite(r.f64().ok_or(TRUNCATED)?));
                }
                if mask & KF_Y != 0 {
                    kf.y = Some(finite(r.f64().ok_or(TRUNCATED)?));
                }
                if mask & KF_Z != 0 {
                    kf.z = Some(finite(r.f64().ok_or(TRUNCATED)?));
                }
                if mask & KF_RR != 0 {
                    kf.rr = Some(finite(r.f64().ok_or(TRUNCATED)?));
                }
                if mask & KF_RU != 0 {
                    kf.ru = Some(finite(r.f64().ok_or(TRUNCATED)?));
                }
                if let Some(frames) = self.frames.as_mut() {
                    frames.push(kf);
                }
            }
            op::ANIM_END => self.anim_end(),
            op::MESH_DATA | op::MESH_VERTS | op::MESH_UVS | op::MESH_TRIS => {
                let i = r.u32().ok_or(TRUNCATED)?;
                self.mesh_op(opcode, i);
            }
            _ => return Err(format!("неизвестная инструкция 0x{opcode:02X}")),
        }
        Ok(true)
    }

    // ── объекты ─────────────────────────────────────────────────────────────

    fn begin_shape(&mut self, id: u8) {
        self.finish_draft();
        self.frames = None; // незавершённая анимация прежнего объекта отбрасывается
        match Shape::from_id(id) {
            Some(shape) => {
                self.cur = Some(Draft {
                    obj: Object::new(shape),
                    verts: None,
                    uvs: None,
                    tris: None,
                    start_pc: self.pc,
                });
                if let Some(g) = self.groups.last_mut() {
                    g.shape_seen = true;
                }
            }
            None => self.warn("неизвестный тип фигуры"),
        }
    }

    /// Завершает текущий объект: собирает меш и кладёт объект в мир.
    fn finish_draft(&mut self) {
        let Some(d) = self.cur.take() else { return };
        if self.objects.len() >= MAX_OBJECTS {
            if !self.objects_capped {
                self.objects_capped = true;
                self.warn_at(
                    d.start_pc,
                    &format!("превышен лимит {MAX_OBJECTS} объектов, остальные пропущены"),
                );
            }
            return;
        }
        let at = d.start_pc;
        let mut obj = d.obj;
        if obj.shape == Shape::Mesh && obj.mesh.is_none() {
            if let (Some(v), Some(t)) = (d.verts, d.tris) {
                let (mesh, rep) = Mesh::build(v, d.uvs, t);
                self.report_repairs(rep, at);
                obj.mesh = Some(Rc::new(mesh));
            }
        }
        if obj.shape == Shape::Mesh && obj.mesh.is_none() {
            self.warn_at(at, "mesh3 без геометрии (нужны data(...) либо verts(...) и tris(...))");
        }
        self.objects.push(obj);
    }

    fn report_repairs(&mut self, rep: Repairs, at: usize) {
        if rep.bad_tris > 0 {
            self.warn_at(
                at,
                &format!(
                    "отброшено треугольников: {} (индекс вне диапазона или нечисловая вершина)",
                    rep.bad_tris
                ),
            );
        }
        if rep.uv_dropped {
            self.warn_at(at, "число uv не совпадает с числом вершин — uv отброшены");
        }
    }

    // ── группы ──────────────────────────────────────────────────────────────

    fn group_begin(&mut self) -> Result<(), String> {
        self.finish_draft();
        if self.groups.len() >= MAX_GROUP_DEPTH {
            return Err("слишком глубокая вложенность групп".to_string());
        }
        if let Some(g) = self.groups.last_mut() {
            g.shape_seen = true;
        }
        let start = self.objects.len();
        self.groups.push(Group { start, xf: GroupXf::identity(), shape_seen: false });
        Ok(())
    }

    fn group_end(&mut self) {
        self.finish_draft();
        match self.groups.pop() {
            Some(g) => apply_group(&mut self.objects, g.start, &g.xf),
            None => self.warn("лишний конец группы проигнорирован"),
        }
    }

    // ── свойства ────────────────────────────────────────────────────────────

    fn target(&self) -> Target {
        if self.cur.is_some() {
            Target::Object
        } else {
            match self.groups.last() {
                Some(g) if !g.shape_seen => Target::Group,
                _ => Target::Nothing,
            }
        }
    }

    fn set_num(&mut self, opcode: u8, v: f64) {
        let v = finite(v);
        match self.target() {
            Target::Object => {
                if let Some(d) = self.cur.as_mut() {
                    let o = &mut d.obj;
                    match opcode {
                        op::X => o.pos[0] = v,
                        op::Y => o.pos[1] = v,
                        op::Z => o.pos[2] = v,
                        op::SCALE => o.scale *= v,
                        op::ROT_Y => o.rot_y = v * PI / 180.0,
                        op::ROT_X => o.rot_x = v * PI / 180.0,
                        _ => {}
                    }
                }
            }
            Target::Group => {
                if let Some(g) = self.groups.last_mut() {
                    match opcode {
                        op::X => g.xf.pos[0] = v,
                        op::Y => g.xf.pos[1] = v,
                        op::Z => g.xf.pos[2] = v,
                        op::SCALE => g.xf.scale = v,
                        op::ROT_Y => g.xf.ry = v * PI / 180.0,
                        op::ROT_X => g.xf.rx = v * PI / 180.0,
                        _ => {}
                    }
                }
            }
            Target::Nothing => self.warn("свойство вне фигуры проигнорировано"),
        }
    }

    fn divisor(&mut self, n: f64) {
        let applied = if let Some(d) = self.cur.as_mut() {
            d.obj.squash = if n.is_finite() && n != 0.0 && (1.0 / n).is_finite() {
                1.0 / n
            } else {
                1.0
            };
            true
        } else {
            false
        };
        if !applied {
            self.warn("делитель высоты вне фигуры проигнорирован");
        }
    }

    fn color(&mut self, c: [u8; 3]) {
        match self.target() {
            Target::Object => {
                if let Some(d) = self.cur.as_mut() {
                    d.obj.rgb = c;
                }
            }
            Target::Group => self.warn("в заголовке mono() допустимы только x, y, z, scale, rr, ru (color пропущен)"),
            Target::Nothing => self.warn("color() вне фигуры проигнорирован"),
        }
    }

    fn deform(&mut self, which: u8, v: f64) {
        let v = finite(v);
        match self.target() {
            Target::Object => {
                if let Some(d) = self.cur.as_mut() {
                    let f = &mut d.obj.deform;
                    match which {
                        0 => f.px = v,
                        1 => f.nx = v,
                        2 => f.py = v,
                        3 => f.ny = v,
                        4 => f.pz = v,
                        5 => f.nz = v,
                        _ => {}
                    }
                }
            }
            Target::Group => self.warn("в заголовке mono() допустимы только x, y, z, scale, rr, ru (растяжение пропущено)"),
            Target::Nothing => self.warn("растяжение вне фигуры проигнорировано"),
        }
    }

    fn texture(&mut self, idx: u32) {
        if self.cur.is_none() {
            self.warn("texture() вне фигуры проигнорирован");
            return;
        }
        let prog: &'p Program = self.prog;
        let idx = idx as usize;
        let Some(src) = prog.strings.get(idx) else {
            self.warn("texture(): неверный индекс строки");
            return;
        };
        let id = match self.tex_map.get(idx).copied().flatten() {
            Some(id) => id,
            None => {
                self.textures.push(src.clone());
                let id = (self.textures.len() - 1) as u32;
                if self.tex_map.len() <= idx {
                    self.tex_map.resize(idx + 1, None);
                }
                self.tex_map[idx] = Some(id);
                id
            }
        };
        if let Some(d) = self.cur.as_mut() {
            d.obj.texture = Some(id);
        }
    }

    fn anim_end(&mut self) {
        let frames = self.frames.take();
        let attached = match (self.cur.as_mut(), frames) {
            (Some(d), Some(frames)) => {
                d.obj.anim = Some(Animation { frames, index: 0, speed: DEFAULT_ANIM_SPEED });
                true
            }
            _ => false,
        };
        if attached {
            self.animated = true;
        } else {
            self.warn("animation() вне фигуры проигнорирована");
        }
    }

    fn mesh_op(&mut self, opcode: u8, blob_idx: u32) {
        if self.cur.is_none() {
            self.warn("данные меша вне фигуры проигнорированы");
            return;
        }
        let prog: &'p Program = self.prog;
        let Some(blob) = prog.blobs.get(blob_idx as usize) else {
            self.warn("неверный индекс блока данных");
            return;
        };
        match opcode {
            op::MESH_DATA => match Mesh::unpack(blob) {
                Some((mesh, rep)) => {
                    let at = self.pc;
                    self.report_repairs(rep, at);
                    if let Some(d) = self.cur.as_mut() {
                        d.obj.mesh = Some(Rc::new(mesh));
                    }
                }
                None => self.warn("data(): бинарный блок повреждён"),
            },
            op::MESH_VERTS => {
                if let Some(d) = self.cur.as_mut() {
                    d.verts = Some(read_vec3(blob));
                }
            }
            op::MESH_UVS => {
                if let Some(d) = self.cur.as_mut() {
                    d.uvs = Some(read_vec2(blob));
                }
            }
            _ => {
                if let Some(d) = self.cur.as_mut() {
                    d.tris = Some(read_tris(blob));
                }
            }
        }
    }
}

fn read_vec3(blob: &[u8]) -> Vec<[f32; 3]> {
    blob.chunks_exact(12)
        .map(|c| {
            [
                f32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                f32::from_le_bytes([c[4], c[5], c[6], c[7]]),
                f32::from_le_bytes([c[8], c[9], c[10], c[11]]),
            ]
        })
        .collect()
}

fn read_vec2(blob: &[u8]) -> Vec<[f32; 2]> {
    blob.chunks_exact(8)
        .map(|c| {
            [
                f32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                f32::from_le_bytes([c[4], c[5], c[6], c[7]]),
            ]
        })
        .collect()
}

fn read_tris(blob: &[u8]) -> Vec<[u32; 3]> {
    blob.chunks_exact(12)
        .map(|c| {
            [
                u32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                u32::from_le_bytes([c[4], c[5], c[6], c[7]]),
                u32::from_le_bytes([c[8], c[9], c[10], c[11]]),
            ]
        })
        .collect()
}

/// Применяет преобразование группы ко всем объектам `objects[start..]`.
/// Формулы — в точности как в OMM 3.x (в т.ч. сложение углов членов
/// группы с углами группы), поэтому старые сцены выглядят так же.
fn apply_group(objects: &mut [Object], start: usize, g: &GroupXf) {
    if g.is_identity() {
        return;
    }
    let (srx, crx) = g.rx.sin_cos();
    let (sry, cry) = g.ry.sin_cos();
    let Some(members) = objects.get_mut(start..) else { return };
    for o in members {
        o.scale *= g.scale;
        let [lx, ly, lz] = o.pos;
        let rx1 = lx * cry + lz * sry;
        let rz1 = -lx * sry + lz * cry;
        let ry1 = ly * crx - rz1 * srx;
        let rz2 = ly * srx + rz1 * crx;
        o.pos = [
            rx1 * g.scale + g.pos[0],
            ry1 * g.scale + g.pos[1],
            rz2 * g.scale + g.pos[2],
        ];
        o.rot_x += g.rx;
        o.rot_y += g.ry;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base64;
    use crate::bytecode::{deform_id, shape_id};
    use crate::parser::parse;

    fn world(src: &str) -> World {
        run(&parse(src)).world
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn basic_object_from_text() {
        let r = run(&parse("cube3\nx(10) y(-20) z(30)\nscale(2)\ncolor(80, 140, 255)\nrr(90) ru(45)\n"));
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(r.world.objects.len(), 1);
        let o = &r.world.objects[0];
        assert_eq!(o.shape, Shape::Cube);
        assert_eq!(o.pos, [10.0, -20.0, 30.0]);
        assert_eq!(o.scale, 100.0);
        assert_eq!(o.rgb, [80, 140, 255]);
        assert!(approx(o.rot_y, PI / 2.0));
        assert!(approx(o.rot_x, PI / 4.0));
    }

    #[test]
    fn defaults_match_v3() {
        let w = world("sphere3");
        let o = &w.objects[0];
        assert_eq!(o.shape, Shape::Sphere);
        assert_eq!(o.pos, [0.0; 3]);
        assert_eq!(o.scale, 50.0);
        assert_eq!(o.squash, 1.0);
        assert_eq!(o.rgb, [200, 200, 200]);
        assert!(o.texture.is_none() && o.anim.is_none() && o.mesh.is_none());
        assert!(o.deform.is_zero());
        assert!(!w.animated);
    }

    #[test]
    fn every_shape_keyword_makes_its_shape() {
        let w = world("cube3\npyramid3\ntriangle3\nsphere3\ncylinder3\nimage3\nmesh3\n");
        let shapes: Vec<Shape> = w.objects.iter().map(|o| o.shape).collect();
        assert_eq!(shapes, Shape::ALL.to_vec());
    }

    #[test]
    fn scale_is_multiplicative_and_last_assignment_wins() {
        let w = world("cube3\nscale(2)\nscale(3)\nx(1)\nx(7)");
        assert_eq!(w.objects[0].scale, 300.0);
        assert_eq!(w.objects[0].pos[0], 7.0);
    }

    #[test]
    fn height_divisor() {
        let w = world("cube3 : 4\ncube3 : 0\ncube3:abc\ncube3 : -2\ncube3: 8 // thin\n");
        let sq: Vec<f64> = w.objects.iter().map(|o| o.squash).collect();
        assert_eq!(sq, vec![0.25, 1.0, 1.0, -0.5, 0.125]);
    }

    #[test]
    fn deform_axes_mapping() {
        let w = world("cube3\nur(1) ul(2) ud(3) uu(4) ug(5) um(6)");
        let d = w.objects[0].deform;
        assert_eq!((d.px, d.nx, d.py, d.ny, d.pz, d.nz), (1.0, 2.0, 3.0, 4.0, 5.0, 6.0));
    }

    #[test]
    fn properties_outside_a_shape_warn_with_line_numbers() {
        let r = run(&parse("x(5)\ncube3"));
        assert_eq!(r.world.objects[0].pos, [0.0; 3]);
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].starts_with("строка 1:"), "{:?}", r.warnings);
    }

    #[test]
    fn animation_is_built_from_keyframes() {
        let w = world("cube3\nanimation(x0 y0 z0, x0 y200 z0, rr90 ru-15)");
        assert!(w.animated);
        let a = w.objects[0].anim.as_ref().expect("animation");
        assert_eq!(a.frames.len(), 3);
        assert_eq!(a.speed, 2.0);
        assert_eq!(a.frames[1].y, Some(200.0));
        assert_eq!((a.frames[2].rr, a.frames[2].ru), (Some(90.0), Some(-15.0)));
        assert_eq!(a.frames[2].x, None);
    }

    #[test]
    fn last_animation_wins_and_animation_needs_a_shape() {
        let w = world("cube3\nanimation(y1)\nanimation(y1,y2,y3)");
        assert_eq!(w.objects[0].anim.as_ref().unwrap().frames.len(), 3);
        let r = run(&parse("animation(y1)\ncube3"));
        assert!(!r.world.animated);
        assert!(!r.warnings.is_empty());
    }

    #[test]
    fn textures_are_deduplicated_into_world_indices() {
        let w = world("cube3\ntexture( a.png )\nsphere3\ntexture(b.png)\nimage3\ntexture(a.png)");
        assert_eq!(w.textures, vec!["a.png".to_string(), "b.png".to_string()]);
        let ids: Vec<Option<u32>> = w.objects.iter().map(|o| o.texture).collect();
        assert_eq!(ids, vec![Some(0), Some(1), Some(0)]);
    }

    #[test]
    fn unused_texture_strings_do_not_leak_into_the_world() {
        let w = world("texture(orphan.png)\ncube3");
        assert!(w.textures.is_empty());
    }

    #[test]
    fn mesh_data_block_roundtrip() {
        let m = Mesh::new(
            vec![[0.0, -50.0, 0.0], [50.0, 50.0, 0.0], [-50.0, 50.0, 0.0]],
            Some(vec![[0.5, 0.0], [1.0, 1.0], [0.0, 1.0]]),
            vec![[0, 1, 2]],
        );
        let r = run(&parse(&format!("mesh3\ndata({})\ncolor(1,2,3)\n", base64::encode(&m.pack()))));
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let got = r.world.objects[0].mesh.as_ref().expect("mesh");
        assert_eq!(got.v, m.v);
        assert_eq!(got.uv, m.uv);
        assert_eq!(got.tris, m.tris);
        assert_eq!(r.world.objects[0].rgb, [1, 2, 3]);
    }

    #[test]
    fn hand_authored_mesh_may_span_lines() {
        let src = "mesh3\n\
                   verts(\n  0 -50 0,   // верх\n  50 50 0,\n  -50 50 0\n)\n\
                   uvs(0.5 0, 1 1, 0 1)\n\
                   tris(0 1 2)\n";
        let r = run(&parse(src));
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let m = r.world.objects[0].mesh.as_ref().expect("mesh");
        assert_eq!(m.v[0], [0.0, -50.0, 0.0]);
        assert_eq!(m.tris, vec![[0, 1, 2]]);
        assert_eq!(m.uv.as_ref().map(Vec::len), Some(3));
    }

    #[test]
    fn data_wins_over_verts_when_valid_and_broken_data_falls_back() {
        let m = Mesh::new(vec![[9.0, 9.0, 9.0]], None, vec![]);
        let src = format!(
            "mesh3\nverts(0 0 0, 1 0 0, 0 1 0)\ntris(0 1 2)\ndata({})\n",
            base64::encode(&m.pack())
        );
        assert_eq!(world(&src).objects[0].mesh.as_ref().unwrap().v.len(), 1);

        let broken = run(&parse("mesh3\ndata(!!!)\nverts(0 0 0, 1 0 0, 0 1 0)\ntris(0 1 2)\n"));
        assert_eq!(broken.world.objects[0].mesh.as_ref().unwrap().tris.len(), 1);
        assert_eq!(broken.warnings.len(), 0);
        assert_eq!(parse("mesh3\ndata(!!!)\n").warnings.len(), 1);
    }

    #[test]
    fn mesh_problems_are_reported() {
        let r = run(&parse("mesh3"));
        assert!(r.world.objects[0].mesh.is_none());
        assert_eq!(r.warnings.len(), 1);

        let r = run(&parse("mesh3\nverts(0 0 0, 1 0 0, 0 1 0)\ntris(0 1 2, 0 1 9, -1 0 1)\n"));
        assert_eq!(r.world.objects[0].mesh.as_ref().unwrap().tris.len(), 1);
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn mono_matches_v3_formula() {
        let r = run(&parse("mono(\n  x(10) y(20) z(30) scale(2)\n  cube3\n  x(5) y(0) z(0)\n)\n"));
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let o = &r.world.objects[0];
        assert_eq!(o.pos, [20.0, 20.0, 30.0]);
        assert_eq!(o.scale, 100.0);
    }

    #[test]
    fn mono_rotation_rotates_positions_and_adds_angles() {
        let w = world("mono(\n rr(90)\n cube3\n x(10)\n)");
        let o = &w.objects[0];
        assert!(approx(o.pos[0], 0.0) && approx(o.pos[1], 0.0) && approx(o.pos[2], -10.0), "{:?}", o.pos);
        assert!(approx(o.rot_y, PI / 2.0));
    }

    #[test]
    fn mono_tilt_rotates_around_x() {
        // ru(90): (0, 10, 0) → rz1 = 0, ry1 = 10*cos90 - 0 = 0, rz2 = 10*sin90 = 10
        let w = world("mono(\n ru(90)\n cube3\n y(10)\n)");
        let o = &w.objects[0];
        assert!(approx(o.pos[1], 0.0) && approx(o.pos[2], 10.0), "{:?}", o.pos);
        assert!(approx(o.rot_x, PI / 2.0));
    }

    #[test]
    fn mono_on_one_line_applies_to_every_member() {
        let w = world("mono( x(5) cube3 color(1,2,3) sphere3 )");
        assert_eq!(w.objects.len(), 2);
        assert_eq!(w.objects[0].rgb, [1, 2, 3]);
        assert_eq!(w.objects[0].pos[0], 5.0);
        assert_eq!(w.objects[1].pos[0], 5.0);
    }

    #[test]
    fn mono_blocks_nest() {
        let w = world("mono(\n x(100)\n mono(\n  x(10)\n  cube3\n  x(1)\n )\n)\n");
        assert_eq!(w.objects[0].pos[0], 111.0);
    }

    #[test]
    fn mono_does_not_leak_into_siblings() {
        let r = run(&parse("cube3\nx(1)\nmono(\n x(50)\n sphere3\n)\nx(999)\ncube3\nx(2)\n"));
        let xs: Vec<f64> = r.world.objects.iter().map(|o| o.pos[0]).collect();
        assert_eq!(xs, vec![1.0, 50.0, 2.0]);
        assert!(!r.warnings.is_empty(), "x(999) после блока должен быть замечен");
    }

    #[test]
    fn group_header_rejects_non_transform_properties() {
        let r = run(&parse("mono(\n color(1,2,3)\n cube3\n)\n"));
        assert_eq!(r.world.objects[0].rgb, [200, 200, 200]);
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn empty_and_identity_groups_are_harmless() {
        let w = world("mono()\nmono(\n cube3\n)\n");
        assert_eq!(w.objects.len(), 1);
        assert_eq!(w.objects[0].pos, [0.0; 3]);
        assert_eq!(w.objects[0].scale, 50.0);
    }

    #[test]
    fn object_count_is_capped() {
        let r = run(&parse(&"cube3\n".repeat(MAX_OBJECTS + 10)));
        assert_eq!(r.world.objects.len(), MAX_OBJECTS);
        assert!(r.warnings.iter().any(|w| w.contains("лимит")));
    }

    // ── устойчивость к враждебному байт-коду ────────────────────────────────

    #[test]
    fn truncated_bytecode_stops_with_a_warning() {
        let mut p = parse("cube3\nx(10)\ny(20)\n");
        let keep = p.code.len() - 5; // обрываем посреди последнего f64
        p.code.truncate(keep);
        let r = run(&p);
        assert_eq!(r.world.objects.len(), 1);
        assert_eq!(r.world.objects[0].pos[0], 10.0);
        assert!(r.warnings.iter().any(|w| w.contains("обрыв")), "{:?}", r.warnings);
    }

    #[test]
    fn unknown_opcode_stops_execution() {
        let mut p = Program::new();
        p.emit_u8(op::SHAPE, shape_id("cube3").unwrap());
        p.emit(0xEE);
        p.emit_u8(op::SHAPE, shape_id("sphere3").unwrap());
        let r = run(&p);
        assert_eq!(r.world.objects.len(), 1);
        assert!(r.warnings[0].contains("0xEE"));
    }

    #[test]
    fn bad_indices_and_ids_are_reported_not_fatal() {
        let mut p = Program::new();
        p.emit_u8(op::SHAPE, 99); // нет такой фигуры
        p.emit_u8(op::SHAPE, shape_id("mesh3").unwrap());
        p.emit_u32(op::TEXTURE, 5); // нет такой строки
        p.emit_u32(op::MESH_DATA, 7); // нет такого блока
        p.emit_deform(deform_id("ur").unwrap(), f64::NAN);
        p.emit_f64(op::X, f64::INFINITY);
        p.emit(op::GROUP_END); // лишний конец группы
        p.emit(op::END);
        let r = run(&p);
        assert_eq!(r.world.objects.len(), 1);
        assert_eq!(r.world.objects[0].pos, [0.0; 3]); // inf → 0
        assert!(r.warnings.len() >= 4, "{:?}", r.warnings);
    }

    #[test]
    fn missing_group_end_is_closed_automatically() {
        let mut p = Program::new();
        p.emit(op::GROUP_BEGIN);
        p.emit_f64(op::X, 100.0);
        p.emit_u8(op::SHAPE, shape_id("cube3").unwrap());
        // ни GROUP_END, ни END
        let r = run(&p);
        assert_eq!(r.world.objects[0].pos[0], 100.0);
    }

    #[test]
    fn group_depth_is_limited() {
        let mut p = Program::new();
        for _ in 0..1000 {
            p.emit(op::GROUP_BEGIN);
        }
        p.emit_u8(op::SHAPE, shape_id("cube3").unwrap());
        let r = run(&p);
        assert!(r.warnings.iter().any(|w| w.contains("вложенность")));
    }

    #[test]
    fn random_bytecode_never_panics() {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..500 {
            let mut p = Program::new();
            p.strings.push("t.png".to_string());
            p.blobs.push((0..32).map(|i| (i * 7) as u8).collect());
            let n = (next() % 80) as usize;
            for _ in 0..n {
                p.code.push((next() % 0x40) as u8);
            }
            let r = run(&p);
            assert!(r.world.objects.len() <= MAX_OBJECTS);
        }
    }
}
