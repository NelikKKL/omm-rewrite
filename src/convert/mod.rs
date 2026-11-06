//! Импорт сторонних 3D-форматов в синтаксис `.omm`.
//!
//! Каждый конвертер превращает исходный файл в один или несколько блоков
//! `mesh3`, геометрия которых упакована компактно в base64 (`data(...)`,
//! см. [`crate::mesh::Mesh::pack`]). Получившийся текст можно сразу
//! скормить `OmmEngine::parse`, либо сохранить как обычный `.omm`-файл.

pub mod gltf;
pub mod obj;

use crate::base64;
use crate::mesh::Mesh;

/// Один импортированный объект: геометрия + базовый цвет + (опционально)
/// текстура — внешняя ссылка или встроенный data-URI.
#[derive(Clone, Debug)]
pub struct Primitive {
    pub mesh: Mesh,
    pub color: [u8; 3],
    pub texture: Option<String>,
}

/// Максимум вершин в одном объединённом `mesh3`.
const MAX_MERGED_VERTS: usize = 4_000_000;

/// Объединяет примитивы с одинаковым материалом (цвет + текстура + наличие
/// UV) в один. Модель из десятков частей с общей текстурой получает один
/// `mesh3` и одну копию встроенной картинки вместо десятков копий.
/// Порядок — по первому появлению материала.
pub fn merge_by_material(prims: &[Primitive]) -> Vec<Primitive> {
    struct Group {
        color: [u8; 3],
        texture: Option<String>,
        has_uv: bool,
        v: Vec<[f32; 3]>,
        uv: Vec<[f32; 2]>,
        tris: Vec<[u32; 3]>,
    }

    let mut groups: Vec<Group> = Vec::new();
    for p in prims {
        let has_uv = p.mesh.uv.is_some();
        let found = groups.iter().position(|g| {
            g.color == p.color
                && g.has_uv == has_uv
                && g.texture == p.texture
                && g.v.len() + p.mesh.v.len() <= MAX_MERGED_VERTS
        });
        let gi = match found {
            Some(i) => i,
            None => {
                groups.push(Group {
                    color: p.color,
                    texture: p.texture.clone(),
                    has_uv,
                    v: Vec::new(),
                    uv: Vec::new(),
                    tris: Vec::new(),
                });
                groups.len() - 1
            }
        };
        let g = &mut groups[gi];
        let base = g.v.len() as u32;
        g.v.extend_from_slice(&p.mesh.v);
        if let Some(uv) = &p.mesh.uv {
            g.uv.extend_from_slice(uv);
        }
        g.tris
            .extend(p.mesh.tris.iter().map(|t| [t[0] + base, t[1] + base, t[2] + base]));
    }

    groups
        .into_iter()
        .map(|g| Primitive {
            mesh: Mesh::new(g.v, if g.has_uv { Some(g.uv) } else { None }, g.tris),
            color: g.color,
            texture: g.texture,
        })
        .collect()
}

/// Печатает примитивы как текст `.omm` (по блоку `mesh3` на материал).
pub fn primitives_to_omm(prims: &[Primitive]) -> String {
    let mut out = String::new();
    for p in &merge_by_material(prims) {
        out.push_str("mesh3\n");
        out.push_str(&format!("data({})\n", base64::encode(&p.mesh.pack())));
        out.push_str(&format!("color({}, {}, {})\n", p.color[0], p.color[1], p.color[2]));
        if let Some(tex) = &p.texture {
            out.push_str(&format!("texture({tex})\n"));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tri(offset: f32) -> Mesh {
        Mesh::new(
            vec![[offset, 0.0, 0.0], [offset + 1.0, 0.0, 0.0], [offset, 1.0, 0.0]],
            None,
            vec![[0, 1, 2]],
        )
    }

    fn prim(offset: f32, color: [u8; 3], tex: Option<&str>) -> Primitive {
        Primitive { mesh: tri(offset), color, texture: tex.map(str::to_string) }
    }

    #[test]
    fn same_material_primitives_are_merged_with_index_offsets() {
        let merged = merge_by_material(&[
            prim(0.0, [1, 2, 3], None),
            prim(10.0, [9, 9, 9], None),
            prim(20.0, [1, 2, 3], None),
        ]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].mesh.v.len(), 6);
        assert_eq!(merged[0].mesh.tris, vec![[0, 1, 2], [3, 4, 5]]);
        assert_eq!(merged[0].mesh.v[3], [20.0, 0.0, 0.0]);
        assert_eq!(merged[1].color, [9, 9, 9]);
    }

    #[test]
    fn different_texture_or_uv_presence_is_not_merged() {
        let mut with_uv = prim(0.0, [1, 1, 1], Some("a"));
        with_uv.mesh = Mesh::new(
            with_uv.mesh.v.clone(),
            Some(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            with_uv.mesh.tris.clone(),
        );
        let merged = merge_by_material(&[
            with_uv.clone(),
            prim(5.0, [1, 1, 1], Some("a")), // без UV
            prim(6.0, [1, 1, 1], Some("b")),
            with_uv,
        ]);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].mesh.v.len(), 6); // два с UV склеены
        assert!(merged[0].mesh.uv.as_ref().is_some_and(|u| u.len() == 6));
    }

    #[test]
    fn omm_text_has_the_v3_layout() {
        let text = primitives_to_omm(&[prim(0.0, [200, 100, 50], Some("t.png"))]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "mesh3");
        assert!(lines[1].starts_with("data(") && lines[1].ends_with(')'));
        assert_eq!(lines[2], "color(200, 100, 50)");
        assert_eq!(lines[3], "texture(t.png)");
        assert!(text.ends_with("\n\n"));
    }

    #[test]
    fn shared_embedded_texture_is_written_once() {
        let big = "data:image/png;base64,AAAA";
        let text = primitives_to_omm(&[
            prim(0.0, [1, 1, 1], Some(big)),
            prim(1.0, [1, 1, 1], Some(big)),
            prim(2.0, [1, 1, 1], Some(big)),
        ]);
        assert_eq!(text.matches("mesh3").count(), 1);
        assert_eq!(text.matches(big).count(), 1);
    }
}
