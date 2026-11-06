//! Произвольная (не процедурная) геометрия для фигуры `mesh3`.
//!
//! Данные приходят либо из текста (`verts()/tris()/uvs()`), либо из
//! компактного бинарного блока `data(base64…)`, который кодируют конвертеры
//! `obj_to_omm` / `glb_to_omm`. Все грани — треугольники.
//!
//! # Инварианты `Mesh`
//!
//! Конструктор [`Mesh::build`] гарантирует, что рендеру не нужно ничего
//! проверять: каждый индекс треугольника `< v.len()`, все вершины конечны,
//! `uv` (если есть) ровно по одному на вершину, а `shade.len() == tris.len()`.

use crate::bytes::{f32_at, u32_at};

/// Направление света в локальных координатах объекта (как в OMM 3.x).
const LIGHT: [f64; 3] = [0.4, -0.7, -0.6];

#[derive(Clone, Debug, Default)]
pub struct Mesh {
    /// Позиции вершин.
    pub v: Vec<[f32; 3]>,
    /// UV (параллельно `v`), нормализованные 0..1.
    pub uv: Option<Vec<[f32; 2]>>,
    /// Треугольники (индексы в `v`).
    pub tris: Vec<[u32; 3]>,
    /// Заранее посчитанное плоское освещение каждого треугольника, 0.55..=1.0.
    /// Свет закреплён в локальном пространстве объекта, поэтому значение
    /// не зависит от камеры и считается один раз, а не каждый кадр.
    pub shade: Vec<f32>,
}

/// Что пришлось исправить при построении меша (для предупреждений).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Repairs {
    /// Треугольники с индексами вне диапазона или нечисловыми вершинами.
    pub bad_tris: usize,
    /// UV отброшены, т.к. их число не совпало с числом вершин.
    pub uv_dropped: bool,
}

impl Mesh {
    /// Строит меш, отбрасывая всё, что могло бы привести к ошибке при
    /// рисовании, и считает освещение.
    pub fn build(
        v: Vec<[f32; 3]>,
        uv: Option<Vec<[f32; 2]>>,
        tris: Vec<[u32; 3]>,
    ) -> (Mesh, Repairs) {
        let mut rep = Repairs::default();

        let uv = match uv {
            Some(mut u) if u.len() == v.len() => {
                for t in u.iter_mut() {
                    if !t[0].is_finite() || !t[1].is_finite() {
                        *t = [0.0, 0.0];
                    }
                }
                Some(u)
            }
            Some(_) => {
                rep.uv_dropped = true;
                None
            }
            None => None,
        };

        let finite: Vec<bool> = v.iter().map(|p| p.iter().all(|c| c.is_finite())).collect();
        let n = v.len();
        let mut good: Vec<[u32; 3]> = Vec::with_capacity(tris.len());
        let mut shade: Vec<f32> = Vec::with_capacity(tris.len());

        for t in tris {
            let a = t[0] as usize;
            let b = t[1] as usize;
            let c = t[2] as usize;
            if a >= n || b >= n || c >= n || !finite[a] || !finite[b] || !finite[c] {
                rep.bad_tris += 1;
                continue;
            }
            shade.push(flat_shade(&v[a], &v[b], &v[c]));
            good.push(t);
        }

        (Mesh { v, uv, tris: good, shade }, rep)
    }

    /// То же, что [`Mesh::build`], но без отчёта.
    pub fn new(v: Vec<[f32; 3]>, uv: Option<Vec<[f32; 2]>>, tris: Vec<[u32; 3]>) -> Mesh {
        Mesh::build(v, uv, tris).0
    }

    /// Бинарный формат (little-endian), который несёт `data(base64…)`:
    ///
    /// ```text
    /// u32  vertex_count
    /// u32  tri_count
    /// u8   has_uv (0/1)
    /// [vertex_count * 3] f32 — позиции (x, y, z)
    /// если has_uv: [vertex_count * 2] f32 — uv (u, v)
    /// [tri_count * 3] u32 — индексы треугольников
    /// ```
    ///
    /// Формат полностью совместим с OMM 3.x.
    pub fn pack(&self) -> Vec<u8> {
        let has_uv = self.uv.is_some();
        let cap = 9
            + self.v.len() * 12
            + if has_uv { self.v.len() * 8 } else { 0 }
            + self.tris.len() * 12;
        let mut out = Vec::with_capacity(cap);
        out.extend_from_slice(&(self.v.len() as u32).to_le_bytes());
        out.extend_from_slice(&(self.tris.len() as u32).to_le_bytes());
        out.push(u8::from(has_uv));

        for p in &self.v {
            for c in p {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
        if let Some(uvs) = &self.uv {
            for t in uvs {
                out.extend_from_slice(&t[0].to_le_bytes());
                out.extend_from_slice(&t[1].to_le_bytes());
            }
        }
        for t in &self.tris {
            for i in t {
                out.extend_from_slice(&i.to_le_bytes());
            }
        }
        out
    }

    /// Разбирает бинарный блок. Размеры проверяются *до* выделения памяти,
    /// арифметика — в `u64`, поэтому испорченный заголовок не может ни
    /// вызвать панику, ни заставить выделить гигабайты.
    pub fn unpack(bytes: &[u8]) -> Option<(Mesh, Repairs)> {
        let vc = u32_at(bytes, 0)? as usize;
        let tc = u32_at(bytes, 4)? as usize;
        let has_uv = *bytes.get(8)? != 0;

        let uv_part = if has_uv { vc as u64 * 8 } else { 0 };
        let need = 9u64 + vc as u64 * 12 + uv_part + tc as u64 * 12;
        if (bytes.len() as u64) < need {
            return None;
        }

        let mut pos = 9usize;
        let mut v: Vec<[f32; 3]> = Vec::with_capacity(vc);
        for _ in 0..vc {
            v.push([f32_at(bytes, pos)?, f32_at(bytes, pos + 4)?, f32_at(bytes, pos + 8)?]);
            pos += 12;
        }

        let uv = if has_uv {
            let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(vc);
            for _ in 0..vc {
                uvs.push([f32_at(bytes, pos)?, f32_at(bytes, pos + 4)?]);
                pos += 8;
            }
            Some(uvs)
        } else {
            None
        };

        let mut tris: Vec<[u32; 3]> = Vec::with_capacity(tc);
        for _ in 0..tc {
            tris.push([u32_at(bytes, pos)?, u32_at(bytes, pos + 4)?, u32_at(bytes, pos + 8)?]);
            pos += 12;
        }

        Some(Mesh::build(v, uv, tris))
    }
}

/// Плоское освещение по нормали грани: ambient 0.55 + направленный свет.
fn flat_shade(a: &[f32; 3], b: &[f32; 3], c: &[f32; 3]) -> f32 {
    let e1 = [
        f64::from(b[0]) - f64::from(a[0]),
        f64::from(b[1]) - f64::from(a[1]),
        f64::from(b[2]) - f64::from(a[2]),
    ];
    let e2 = [
        f64::from(c[0]) - f64::from(a[0]),
        f64::from(c[1]) - f64::from(a[1]),
        f64::from(c[2]) - f64::from(a[2]),
    ];
    let nx = e1[1] * e2[2] - e1[2] * e2[1];
    let ny = e1[2] * e2[0] - e1[0] * e2[2];
    let nz = e1[0] * e2[1] - e1[1] * e2[0];
    let len = (nx * nx + ny * ny + nz * nz).sqrt().max(1e-9);
    let dot = (nx * LIGHT[0] + ny * LIGHT[1] + nz * LIGHT[2]) / len;
    (0.55 + dot.max(0.0) * 0.45).min(1.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad() -> Mesh {
        Mesh::new(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]],
            Some(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]),
            vec![[0, 1, 2], [0, 2, 3]],
        )
    }

    #[test]
    fn pack_unpack_roundtrip() {
        let m = quad();
        let bytes = m.pack();
        assert_eq!(bytes.len(), 9 + 4 * 12 + 4 * 8 + 2 * 12);
        let (back, rep) = Mesh::unpack(&bytes).expect("unpack");
        assert_eq!(rep, Repairs::default());
        assert_eq!(back.v, m.v);
        assert_eq!(back.uv, m.uv);
        assert_eq!(back.tris, m.tris);
    }

    #[test]
    fn header_layout_is_compatible_with_v3() {
        // 1 вершина, 0 треугольников, без UV.
        let mut b = vec![1, 0, 0, 0, 0, 0, 0, 0, 0];
        b.extend_from_slice(&1.0f32.to_le_bytes());
        b.extend_from_slice(&2.0f32.to_le_bytes());
        b.extend_from_slice(&3.0f32.to_le_bytes());
        let (m, _) = Mesh::unpack(&b).unwrap();
        assert_eq!(m.v, vec![[1.0, 2.0, 3.0]]);
        assert!(m.uv.is_none());
        assert!(m.tris.is_empty());
    }

    #[test]
    fn hostile_headers_do_not_panic_or_allocate() {
        // огромные счётчики при крошечном теле
        let mut b = vec![0xFF; 9];
        b[8] = 1;
        assert!(Mesh::unpack(&b).is_none());
        assert!(Mesh::unpack(&[]).is_none());
        assert!(Mesh::unpack(&[1, 2, 3]).is_none());
    }

    #[test]
    fn invalid_triangles_are_dropped() {
        let (m, rep) = Mesh::build(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, f32::NAN]],
            Some(vec![[0.0, 0.0]]), // неверное число UV
            vec![[0, 1, 2], [0, 1, 7], [0, 1, 1]],
        );
        assert_eq!(rep.bad_tris, 2); // NaN-вершина и индекс 7
        assert!(rep.uv_dropped);
        assert!(m.uv.is_none());
        assert_eq!(m.tris, vec![[0, 1, 1]]);
        assert_eq!(m.shade.len(), m.tris.len());
    }

    #[test]
    fn shade_is_in_range_and_matches_formula() {
        let m = quad();
        for s in &m.shade {
            assert!((0.55..=1.0).contains(s));
        }
        // Нормаль (0,0,1): dot = -0.6 → освещения нет → ambient 0.55.
        assert!((m.shade[0] - 0.55).abs() < 1e-6);
    }
}
