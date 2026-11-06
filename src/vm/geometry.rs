//! Геометрия процедурных фигур и её разбиение на треугольники для viewer.
//!
//! Порядок вершин и граней в точности совпадает с OMM 3.x — от него зависят
//! и закраска граней, и текстурирование. Здесь же закодирован «фирменный»
//! внешний вид OMM: каждая грань имеет собственную яркость (`face_light`),
//! плоские фигуры текстурируются целой картинкой на каждую грань.

use std::f64::consts::PI;

use super::world::Shape;
use crate::mesh::Mesh;
use crate::viewer::STRIDE;

/// Грань: 3 или 4 индекса вершин.
#[derive(Clone, Copy, Debug)]
pub struct Face {
    pub n: u8,
    pub i: [u16; 4],
}

impl Face {
    fn tri(a: usize, b: usize, c: usize) -> Face {
        Face { n: 3, i: [a as u16, b as u16, c as u16, 0] }
    }

    fn quad(a: usize, b: usize, c: usize, d: usize) -> Face {
        Face { n: 4, i: [a as u16, b as u16, c as u16, d as u16] }
    }
}

/// Нормализованная геометрия (координаты в диапазоне −1..1).
#[derive(Clone, Debug)]
pub struct Geometry {
    pub verts: Vec<[f64; 3]>,
    pub faces: Vec<Face>,
}

/// Геометрия процедурной фигуры; для `Shape::Mesh` её нет (она хранится
/// в самом объекте).
pub fn geometry(shape: Shape) -> Option<Geometry> {
    match shape {
        Shape::Cube => Some(cube()),
        Shape::Pyramid => Some(pyramid()),
        Shape::Triangle => Some(triangle()),
        Shape::Sphere => Some(sphere()),
        Shape::Cylinder => Some(cylinder()),
        Shape::Image => Some(image()),
        Shape::Mesh => None,
    }
}

fn cube() -> Geometry {
    Geometry {
        verts: vec![
            [-1.0, -1.0, -1.0],
            [1.0, -1.0, -1.0],
            [1.0, 1.0, -1.0],
            [-1.0, 1.0, -1.0],
            [-1.0, -1.0, 1.0],
            [1.0, -1.0, 1.0],
            [1.0, 1.0, 1.0],
            [-1.0, 1.0, 1.0],
        ],
        faces: vec![
            Face::quad(0, 1, 2, 3),
            Face::quad(4, 5, 6, 7),
            Face::quad(0, 1, 5, 4),
            Face::quad(2, 3, 7, 6),
            Face::quad(0, 3, 7, 4),
            Face::quad(1, 2, 6, 5),
        ],
    }
}

fn pyramid() -> Geometry {
    Geometry {
        verts: vec![
            [0.0, -1.0, 0.0],
            [-1.0, 1.0, -1.0],
            [1.0, 1.0, -1.0],
            [1.0, 1.0, 1.0],
            [-1.0, 1.0, 1.0],
        ],
        faces: vec![
            Face::tri(0, 2, 1),
            Face::tri(0, 3, 2),
            Face::tri(0, 4, 3),
            Face::tri(0, 1, 4),
            Face::quad(4, 3, 2, 1),
        ],
    }
}

fn triangle() -> Geometry {
    Geometry {
        verts: vec![[0.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0]],
        faces: vec![Face::tri(0, 1, 2)],
    }
}

/// `image3` — плоский квадрат; рисуется только когда текстура загружена.
fn image() -> Geometry {
    Geometry {
        verts: vec![
            [-1.0, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [1.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0],
        ],
        faces: vec![Face::quad(0, 1, 2, 3)],
    }
}

fn sphere() -> Geometry {
    const LAT: usize = 8;
    const LON: usize = 8;
    let mut verts = Vec::with_capacity((LAT + 1) * (LON + 1));
    let mut faces = Vec::with_capacity(LAT * LON * 2);

    for la in 0..=LAT {
        let theta = (la as f64 * PI) / LAT as f64;
        let (sin_t, cos_t) = theta.sin_cos();
        for lo in 0..=LON {
            let phi = (lo as f64 * 2.0 * PI) / LON as f64;
            verts.push([phi.cos() * sin_t, cos_t, phi.sin() * sin_t]);
        }
    }
    for la in 0..LAT {
        for lo in 0..LON {
            let f = la * (LON + 1) + lo;
            let s2 = f + LON + 1;
            faces.push(Face::tri(f, s2, f + 1));
            faces.push(Face::tri(s2, s2 + 1, f + 1));
        }
    }
    Geometry { verts, faces }
}

fn cylinder() -> Geometry {
    const SEG: usize = 10;
    let mut verts = Vec::with_capacity(SEG * 2 + 2);
    let mut faces = Vec::with_capacity(SEG * 3);

    for i in 0..SEG {
        let theta = (i as f64 * PI * 2.0) / SEG as f64;
        let (s, c) = theta.sin_cos();
        verts.push([c, -1.0, s]); // чётные — верхнее кольцо (y = −1)
        verts.push([c, 1.0, s]); //  нечётные — нижнее кольцо (y = +1)
    }
    let tc = verts.len();
    verts.push([0.0, -1.0, 0.0]);
    let bc = verts.len();
    verts.push([0.0, 1.0, 0.0]);

    for i in 0..SEG {
        let ct = i * 2;
        let cb = ct + 1;
        let nt = ((i + 1) % SEG) * 2;
        let nb = nt + 1;
        faces.push(Face::quad(ct, nt, nb, cb));
        faces.push(Face::tri(tc, ct, nt));
        faces.push(Face::tri(bc, nb, cb));
    }
    Geometry { verts, faces }
}

/// Яркость грани `fi` из `nf` (правила OMM 3.x): каждая грань чуть светлее
/// предыдущей, по кругу.
pub fn face_light(shape: Shape, fi: usize, nf: usize) -> f64 {
    match shape {
        Shape::Sphere => (0.8 + (fi % 8) as f64 * 0.05).min(1.0),
        Shape::Cylinder => (0.85 + (fi % 10) as f64 * 0.04).min(1.0),
        _ => (0.85 + (fi % nf.max(1)) as f64 * 0.05).min(1.0),
    }
}

fn push_tri(
    out: &mut Vec<f32>,
    verts: &[[f64; 3]],
    idx: [usize; 3],
    light: f32,
    uv: [[f32; 2]; 3],
    edge: u8,
) {
    for k in 0..3 {
        let p = verts.get(idx[k]).copied().unwrap_or([0.0; 3]);
        out.extend_from_slice(&[
            p[0] as f32,
            p[1] as f32,
            p[2] as f32,
            light,
            uv[k][0],
            uv[k][1],
            f32::from(edge),
        ]);
    }
}

/// Разбивает грани на треугольники в формате viewer (`STRIDE` чисел на
/// вершину). `verts` — позиции вершин, параллельные `geo.verts` (так можно
/// подставить деформированные).
///
/// * Четырёхугольник → два треугольника; диагональ в контур не входит.
/// * UV: каждая грань целиком покрывается текстурой (у квадрата — весь
///   кадр, у треугольника — «домик» с вершиной вверху по центру).
pub fn tessellate(shape: Shape, geo: &Geometry, verts: &[[f64; 3]]) -> Vec<f32> {
    let nf = geo.faces.len();
    let mut out = Vec::with_capacity(nf * 6 * STRIDE);
    for (fi, face) in geo.faces.iter().enumerate() {
        let light = face_light(shape, fi, nf) as f32;
        let i = face.i.map(usize::from);
        if face.n == 4 {
            // диагональ i0–i2: в первом треугольнике она напротив вершины 1
            // (бит 1), во втором — напротив вершины 2 (бит 2)
            push_tri(
                &mut out,
                verts,
                [i[0], i[1], i[2]],
                light,
                [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
                0b101,
            );
            push_tri(
                &mut out,
                verts,
                [i[0], i[2], i[3]],
                light,
                [[0.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
                0b011,
            );
        } else {
            push_tri(
                &mut out,
                verts,
                [i[0], i[1], i[2]],
                light,
                [[0.5, 0.0], [1.0, 1.0], [0.0, 1.0]],
                0b111,
            );
        }
    }
    out
}

/// Разбивает `mesh3` на треугольники в формате viewer. Яркость берётся из
/// заранее посчитанного `Mesh::shade`, UV — из меша (если есть).
pub fn tessellate_mesh(mesh: &Mesh) -> Vec<f32> {
    let mut out = Vec::with_capacity(mesh.tris.len() * 3 * STRIDE);
    for (ti, tri) in mesh.tris.iter().enumerate() {
        let light = mesh.shade.get(ti).copied().unwrap_or(1.0);
        for &vi in tri {
            let vi = vi as usize;
            let p = mesh.v.get(vi).copied().unwrap_or([0.0; 3]);
            let uv = mesh
                .uv
                .as_ref()
                .and_then(|u| u.get(vi))
                .copied()
                .unwrap_or([0.0, 0.0]);
            out.extend_from_slice(&[p[0], p[1], p[2], light, uv[0], uv[1], 7.0]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_match_v3() {
        let counts = |s: Shape| {
            let g = geometry(s).unwrap();
            (g.verts.len(), g.faces.len())
        };
        assert_eq!(counts(Shape::Cube), (8, 6));
        assert_eq!(counts(Shape::Pyramid), (5, 5));
        assert_eq!(counts(Shape::Triangle), (3, 1));
        assert_eq!(counts(Shape::Sphere), (81, 128));
        assert_eq!(counts(Shape::Cylinder), (22, 30));
        assert_eq!(counts(Shape::Image), (4, 1));
        assert!(geometry(Shape::Mesh).is_none());
    }

    #[test]
    fn every_face_index_is_in_range_and_finite() {
        for s in Shape::ALL {
            let Some(g) = geometry(s) else { continue };
            for f in &g.faces {
                assert!(f.n == 3 || f.n == 4);
                for k in 0..usize::from(f.n) {
                    assert!(usize::from(f.i[k]) < g.verts.len(), "{s:?}");
                }
            }
            for v in &g.verts {
                assert!(v.iter().all(|c| c.is_finite() && c.abs() <= 1.0 + 1e-12));
            }
        }
    }

    #[test]
    fn sphere_points_lie_on_unit_sphere() {
        for v in &geometry(Shape::Sphere).unwrap().verts {
            let r = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            assert!((r - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn triangle_counts_after_tessellation() {
        let tri_count = |s: Shape| {
            let g = geometry(s).unwrap();
            tessellate(s, &g, &g.verts).len() / STRIDE / 3
        };
        assert_eq!(tri_count(Shape::Cube), 12); // 6 квадратов × 2
        assert_eq!(tri_count(Shape::Pyramid), 4 + 2);
        assert_eq!(tri_count(Shape::Triangle), 1);
        assert_eq!(tri_count(Shape::Sphere), 128);
        assert_eq!(tri_count(Shape::Cylinder), 10 * (2 + 1 + 1));
        assert_eq!(tri_count(Shape::Image), 2);
    }

    #[test]
    fn face_light_rules_match_v3() {
        // куб: 6 граней → 0.85, 0.9, 0.95, 1, 1, 1
        let cube: Vec<f64> = (0..6).map(|i| face_light(Shape::Cube, i, 6)).collect();
        let want = [0.85, 0.90, 0.95, 1.0, 1.0, 1.0];
        for (a, b) in cube.iter().zip(want) {
            assert!((a - b).abs() < 1e-12);
        }
        assert!((face_light(Shape::Sphere, 0, 128) - 0.8).abs() < 1e-12);
        assert!((face_light(Shape::Sphere, 8, 128) - 0.8).abs() < 1e-12); // период 8
        assert_eq!(face_light(Shape::Sphere, 7, 128), 1.0);
        assert!((face_light(Shape::Cylinder, 1, 30) - 0.89).abs() < 1e-12);
        assert!((face_light(Shape::Triangle, 0, 1) - 0.85).abs() < 1e-12);
    }

    #[test]
    fn quad_is_split_with_hidden_diagonal_and_full_uv() {
        let g = geometry(Shape::Image).unwrap();
        let v = tessellate(Shape::Image, &g, &g.verts);
        assert_eq!(v.len(), 2 * 3 * STRIDE);
        let vert = |k: usize| &v[k * STRIDE..(k + 1) * STRIDE];
        // UV первого треугольника: (0,0) (1,0) (1,1); второго: (0,0) (1,1) (0,1)
        assert_eq!([vert(0)[4], vert(0)[5]], [0.0, 0.0]);
        assert_eq!([vert(1)[4], vert(1)[5]], [1.0, 0.0]);
        assert_eq!([vert(2)[4], vert(2)[5]], [1.0, 1.0]);
        assert_eq!([vert(3)[4], vert(3)[5]], [0.0, 0.0]);
        assert_eq!([vert(4)[4], vert(4)[5]], [1.0, 1.0]);
        assert_eq!([vert(5)[4], vert(5)[5]], [0.0, 1.0]);
        // маски рёбер: диагональ скрыта (бит 1 у первого, бит 2 у второго)
        assert_eq!(vert(0)[6], 5.0);
        assert_eq!(vert(3)[6], 3.0);
    }

    #[test]
    fn triangle_face_uv_is_a_house() {
        let g = geometry(Shape::Triangle).unwrap();
        let v = tessellate(Shape::Triangle, &g, &g.verts);
        let uv: Vec<[f32; 2]> = (0..3).map(|k| [v[k * STRIDE + 4], v[k * STRIDE + 5]]).collect();
        assert_eq!(uv, vec![[0.5, 0.0], [1.0, 1.0], [0.0, 1.0]]);
        assert_eq!(v[6], 7.0);
    }

    #[test]
    fn mesh_tessellation_carries_shade_and_uv() {
        let m = Mesh::new(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]],
            Some(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]),
            vec![[0, 1, 2]],
        );
        let v = tessellate_mesh(&m);
        assert_eq!(v.len(), 3 * STRIDE);
        assert_eq!(v[3], m.shade[0]);
        assert_eq!([v[STRIDE + 4], v[STRIDE + 5]], [1.0, 0.0]);
        assert_eq!(v[6], 7.0);
        // без UV — нули
        let flat = Mesh::new(m.v.clone(), None, vec![[0, 1, 2]]);
        let v = tessellate_mesh(&flat);
        assert_eq!([v[4], v[5]], [0.0, 0.0]);
    }
}
