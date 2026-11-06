//! Мир VM: объекты в том виде, в каком их строит виртуальная машина.

use std::rc::Rc;

use crate::mesh::Mesh;

/// Масштаб по умолчанию: половина ребра куба в пикселях.
pub const DEFAULT_SCALE: f64 = 50.0;
/// Цвет по умолчанию (нейтральный серый).
pub const DEFAULT_RGB: [u8; 3] = [200, 200, 200];
/// Скорость анимации по умолчанию, пикселей за один `step()`.
pub const DEFAULT_ANIM_SPEED: f64 = 2.0;

/// Тип фигуры. Порядок совпадает с `bytecode::SHAPE_NAMES`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Shape {
    Cube,
    Pyramid,
    Triangle,
    Sphere,
    Cylinder,
    Image,
    /// Произвольная геометрия (`data()` или `verts()/tris()/uvs()`).
    Mesh,
}

impl Shape {
    pub const ALL: [Shape; 7] = [
        Shape::Cube,
        Shape::Pyramid,
        Shape::Triangle,
        Shape::Sphere,
        Shape::Cylinder,
        Shape::Image,
        Shape::Mesh,
    ];

    /// Фигура по операнду инструкции `SHAPE`.
    pub fn from_id(id: u8) -> Option<Shape> {
        Shape::ALL.get(usize::from(id)).copied()
    }

    /// Ключевое слово фигуры в тексте `.omm`.
    pub fn keyword(self) -> &'static str {
        crate::bytecode::SHAPE_NAMES[self as usize]
    }
}

/// Один ключевой кадр `animation(...)`. Отсутствующая ось = «не менять».
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Keyframe {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub z: Option<f64>,
    /// Поворот вокруг вертикальной оси, градусы (`rr`).
    pub rr: Option<f64>,
    /// Наклон вверх/вниз, градусы (`ru`).
    pub ru: Option<f64>,
}

/// Асимметричное растяжение сторон (`ur ul ud uu ug um`).
///
/// Имя поля = знак оси, которую оно растягивает:
/// `ur`→`px`, `ul`→`nx`, `ud`→`py`, `uu`→`ny`, `ug`→`pz`, `um`→`nz`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Deform {
    pub px: f64,
    pub nx: f64,
    pub py: f64,
    pub ny: f64,
    pub pz: f64,
    pub nz: f64,
}

impl Deform {
    pub fn is_zero(&self) -> bool {
        self.px == 0.0
            && self.nx == 0.0
            && self.py == 0.0
            && self.ny == 0.0
            && self.pz == 0.0
            && self.nz == 0.0
    }
}

/// Состояние анимации объекта.
#[derive(Clone, Debug)]
pub struct Animation {
    pub frames: Vec<Keyframe>,
    /// Индекс кадра, к которому объект движется сейчас.
    pub index: usize,
    /// Скорость, пикселей за один `step()`.
    pub speed: f64,
}

/// Один объект мира (фигура со всеми свойствами).
#[derive(Clone, Debug)]
pub struct Object {
    pub shape: Shape,
    /// Позиция `x, y, z`.
    pub pos: [f64; 3],
    /// Масштаб (`scale()` перемножается; по умолчанию 50).
    pub scale: f64,
    /// Множитель высоты: `1 / N` для `cube3 : N`.
    pub squash: f64,
    /// Наклон (`ru`), радианы.
    pub rot_x: f64,
    /// Поворот (`rr`), радианы.
    pub rot_y: f64,
    pub rgb: [u8; 3],
    /// Индекс в `World::textures`.
    pub texture: Option<u32>,
    pub deform: Deform,
    pub anim: Option<Animation>,
    /// Геометрия для `Shape::Mesh`.
    pub mesh: Option<Rc<Mesh>>,
}

impl Object {
    pub fn new(shape: Shape) -> Object {
        Object {
            shape,
            pos: [0.0; 3],
            scale: DEFAULT_SCALE,
            squash: 1.0,
            rot_x: 0.0,
            rot_y: 0.0,
            rgb: DEFAULT_RGB,
            texture: None,
            deform: Deform::default(),
            anim: None,
            mesh: None,
        }
    }
}

/// Результат работы VM.
#[derive(Clone, Debug, Default)]
pub struct World {
    pub objects: Vec<Object>,
    /// Уникальные источники текстур (URL или data-URI) в порядке первого
    /// использования. `Object::texture` ссылается сюда по индексу.
    pub textures: Vec<String>,
    /// Есть ли хотя бы одна `animation(...)`.
    pub animated: bool,
}
