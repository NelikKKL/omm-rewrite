//! Минимальный парсер Wavefront OBJ (`.obj`).
//!
//! Поддерживает: `v` (вершины), `vt` (UV), `f` (грани — треугольники и
//! n-угольники, триангулируются веером). Нормали (`vn`) игнорируются — OMM
//! сам считает плоское освещение по грани для `mesh3`. Материалы (`.mtl`)
//! не парсятся: результат получает нейтральный серый цвет, цвет/текстуру
//! можно дописать вручную в получившийся `.omm`-текст (`color(...)`,
//! `texture(...)`).

use std::collections::HashMap;

use crate::convert::Primitive;
use crate::mesh::Mesh;

fn number(s: &str) -> Option<f64> {
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

pub fn obj_to_mesh(text: &str) -> Result<Mesh, String> {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut texcoords: Vec<[f32; 2]> = Vec::new();

    // Дедупликация по паре (индекс позиции, индекс uv) — OBJ хранит их
    // раздельно, а нам нужен один индекс на (позиция+uv) для треугольников.
    let mut vert_map: HashMap<(usize, i64), u32> = HashMap::new();
    let mut out_v: Vec<[f32; 3]> = Vec::new();
    let mut out_uv: Vec<[f32; 2]> = Vec::new();
    let mut any_uv = false;
    let mut tris: Vec<[u32; 3]> = Vec::new();

    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let mut it = l.split_whitespace();
        let Some(tag) = it.next() else { continue };

        match tag {
            "v" => {
                let nums: Vec<f64> = it.filter_map(number).collect();
                if nums.len() >= 3 {
                    // glTF/OBJ обычно правосторонние с Y вверх, OMM использует
                    // Y как «вниз» (как координаты экрана) — инвертируем.
                    positions.push([nums[0] as f32, -nums[1] as f32, nums[2] as f32]);
                }
            }
            "vt" => {
                let nums: Vec<f32> = it.filter_map(number).map(|v| v as f32).collect();
                match nums.len() {
                    0 => {}
                    1 => texcoords.push([nums[0], 0.0]),
                    _ => texcoords.push([nums[0], nums[1]]),
                }
            }
            "f" => {
                let tokens: Vec<&str> = it.collect();
                if tokens.len() < 3 {
                    continue;
                }
                let mut idxs: Vec<u32> = Vec::with_capacity(tokens.len());

                for tok in &tokens {
                    let mut parts = tok.split('/');
                    let Some(Some(vi)) = parts.next().map(|s| s.parse::<i64>().ok()) else {
                        continue;
                    };
                    let vti = parts
                        .next()
                        .filter(|s| !s.is_empty())
                        .and_then(|s| s.parse::<i64>().ok());

                    let pos_index = resolve_index(vi, positions.len());
                    if pos_index >= positions.len() {
                        continue;
                    }

                    // индекс uv в допустимых пределах либо -1 («нет uv»)
                    let uv_index: i64 = match vti {
                        Some(t) => {
                            let i = resolve_index(t, texcoords.len());
                            if i < texcoords.len() {
                                i as i64
                            } else {
                                -1
                            }
                        }
                        None => -1,
                    };

                    let key = (pos_index, uv_index);
                    let merged = match vert_map.get(&key) {
                        Some(&id) => id,
                        None => {
                            let id = out_v.len() as u32;
                            out_v.push(positions[pos_index]);
                            if uv_index >= 0 {
                                out_uv.push(texcoords[uv_index as usize]);
                                any_uv = true;
                            } else {
                                out_uv.push([0.0, 0.0]);
                            }
                            vert_map.insert(key, id);
                            id
                        }
                    };
                    idxs.push(merged);
                }

                if idxs.len() < 3 {
                    continue;
                }
                // Веерная триангуляция n-угольника; порядок вершин развёрнут,
                // чтобы компенсировать инверсию оси Y выше (иначе грани
                // окажутся "вывернуты" для расчёта нормали/освещения).
                for k in 1..idxs.len() - 1 {
                    tris.push([idxs[0], idxs[k + 1], idxs[k]]);
                }
            }
            _ => {}
        }
    }

    if out_v.is_empty() || tris.is_empty() {
        return Err("в OBJ-файле не найдено геометрии (нужны строки 'v' и 'f')".into());
    }

    let uv = if any_uv { Some(out_uv) } else { None };
    Ok(Mesh::new(out_v, uv, tris))
}

/// Индекс OBJ (1-based, отрицательные — от конца) → индекс в массиве.
/// Недопустимые значения (`0`, выход за начало) дают `usize::MAX`.
fn resolve_index(raw: i64, len: usize) -> usize {
    if raw > 0 {
        usize::try_from(raw - 1).unwrap_or(usize::MAX)
    } else if raw < 0 {
        match (len as i64).checked_add(raw) {
            Some(i) if i >= 0 => i as usize,
            _ => usize::MAX,
        }
    } else {
        usize::MAX
    }
}

/// Конвертирует текст `.obj` целиком в готовый `.omm`-текст (один блок `mesh3`).
pub fn obj_to_omm(text: &str) -> Result<String, String> {
    let mesh = obj_to_mesh(text)?;
    let prims = vec![Primitive { mesh, color: [200, 200, 200], texture: None }];
    Ok(crate::convert::primitives_to_omm(&prims))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUBE_FACE: &str = "\
# квадрат с UV
v 0 0 0
v 1 0 0
v 1 1 0
v 0 1 0
vt 0 0
vt 1 0
vt 1 1
vt 0 1
vn 0 0 1
f 1/1/1 2/2/1 3/3/1 4/4/1
";

    #[test]
    fn quad_is_fan_triangulated_with_flipped_y_and_winding() {
        let m = obj_to_mesh(CUBE_FACE).unwrap();
        assert_eq!(m.v.len(), 4);
        // Y инвертирован
        assert_eq!(m.v[2], [1.0, -1.0, 0.0]);
        // [0, k+1, k] для k = 1, 2
        assert_eq!(m.tris, vec![[0, 2, 1], [0, 3, 2]]);
        assert_eq!(m.uv.as_ref().unwrap()[2], [1.0, 1.0]);
        assert_eq!(m.shade.len(), 2);
    }

    #[test]
    fn vertices_are_deduplicated_by_position_and_uv() {
        let src = "v 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nvt 1 1\nf 1/1 2/1 3/1\nf 1/2 2/2 3/2\n";
        let m = obj_to_mesh(src).unwrap();
        assert_eq!(m.v.len(), 6); // одна и та же позиция с разным uv — разные вершины
        let same = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\nf 3 2 1\n";
        assert_eq!(obj_to_mesh(same).unwrap().v.len(), 3);
    }

    #[test]
    fn negative_indices_count_from_the_end() {
        let m = obj_to_mesh("v 0 0 0\nv 1 0 0\nv 0 1 0\nf -3 -2 -1\n").unwrap();
        assert_eq!(m.tris, vec![[0, 2, 1]]);
    }

    #[test]
    fn invalid_references_are_skipped_not_fatal() {
        // индекс 0 и 99 недопустимы; грань остаётся из двух вершин → пропускается
        let m = obj_to_mesh("v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\nf 0 1 2\nf 1 2 99\n").unwrap();
        assert_eq!(m.tris.len(), 1);
    }

    #[test]
    fn vertex_extras_and_garbage_tokens_are_tolerated() {
        let src = "v 0 0 0 1 0 0\nv 1 0 0 nan\nv 0 1 inf 5\nv 0 1 0\nf 1 2 4 abc\no thing\ns off\nusemtl x\n";
        let m = obj_to_mesh(src).unwrap();
        // третья строка «v 0 1 inf 5» → числа 0, 1, 5 (inf отброшен) — это валидная вершина
        assert!(m.v.len() >= 3);
        assert!(!m.tris.is_empty());
    }

    #[test]
    fn single_component_vt_defaults_v_to_zero() {
        let m = obj_to_mesh("v 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0.5\nf 1/1 2/1 3/1\n").unwrap();
        assert_eq!(m.uv.as_ref().unwrap()[0], [0.5, 0.0]);
    }

    #[test]
    fn no_geometry_is_an_error_with_the_v3_message() {
        let e = obj_to_mesh("# пусто\nvt 0 0\n").unwrap_err();
        assert_eq!(e, "в OBJ-файле не найдено геометрии (нужны строки 'v' и 'f')");
        assert!(obj_to_mesh("").is_err());
        assert!(obj_to_mesh("v 0 0 0\nv 1 0 0\nv 0 1 0\n").is_err()); // нет граней
    }

    #[test]
    fn output_text_roundtrips_through_parser_and_vm() {
        let text = obj_to_omm(CUBE_FACE).unwrap();
        assert!(text.starts_with("mesh3\ndata("));
        assert!(text.contains("color(200, 200, 200)"));
        let run = crate::vm::run(&crate::parser::parse(&text));
        assert!(run.warnings.is_empty(), "{:?}", run.warnings);
        let m = run.world.objects[0].mesh.as_ref().unwrap();
        assert_eq!(m.tris.len(), 2);
        assert_eq!(m.v.len(), 4);
    }

    #[test]
    fn huge_and_hostile_indices_do_not_panic() {
        let src = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 9223372036854775807 1 2\nf -9223372036854775808 1 2\nf 1 2 3\n";
        let m = obj_to_mesh(src).unwrap();
        assert_eq!(m.tris.len(), 1);
    }
}
