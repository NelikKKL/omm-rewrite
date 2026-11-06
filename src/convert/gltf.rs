//! Минимальный загрузчик бинарного glTF (`.glb`) → `mesh3`.
//!
//! Поддерживается самый частый случай экспорта (Blender и т.п.): один файл
//! `.glb` со встроенным бинарным чанком (буфер вершин/индексов) и, опционально,
//! встроенными PNG/JPEG-текстурами. Обходит полную иерархию узлов сцены и
//! «запекает» трансформации (translation/rotation/scale или matrix) прямо в
//! координаты вершин — поэтому результирующий `mesh3` не требует никаких
//! доп. трансформаций в OMM, кроме обычных `scale()`/`x()`/`y()`/`z()` по
//! желанию пользователя.
//!
//! Не поддерживается: отдельный `.gltf` + внешние `.bin`/`.jpg` (только
//! самодостаточный `.glb`), скелетная анимация, морф-таргеты, любые режимы
//! примитивов кроме TRIANGLES.
//!
//! # Что стало безопаснее по сравнению с OMM 3.x
//!
//! * граф узлов обходится с защитой от циклов, глубины и «взрыва» числа
//!   посещений (раньше циклический файл вызывал бесконечную рекурсию);
//! * все размеры акцессоров проверяются *до* выделения памяти, арифметика —
//!   с проверкой переполнения (раньше `count` из файла шёл прямо в
//!   `Vec::with_capacity`);
//! * индексы читаются как целые (`u32`), а не через `f32` — раньше модели с
//!   более чем 16 млн вершин получали испорченные индексы;
//! * без `scene` и `scenes` корнями считаются только узлы без родителя
//!   (раньше дочерние узлы попадали в результат дважды).

use std::collections::HashMap;

use crate::base64;
use crate::bytes::{f32_at, i16_at, u16_at, u32_at};
use crate::convert::Primitive;
use crate::json::Json;
use crate::mesh::Mesh;

const GLB_MAGIC: u32 = 0x4654_6C67; // "glTF" little-endian
const CHUNK_JSON: u32 = 0x4E4F_534A; // "JSON"
const CHUNK_BIN: u32 = 0x004E_4942; // "BIN\0"

const MAX_NODE_DEPTH: usize = 64;
const MAX_NODE_VISITS: usize = 100_000;
const MAX_TOTAL_VERTS: usize = 16_000_000;
const MAX_ACCESSOR_COUNT: usize = 20_000_000;

const DEFAULT_COLOR: [u8; 3] = [200, 200, 200];

type Mat4 = [f64; 16];

fn identity() -> Mat4 {
    let mut m = [0.0; 16];
    m[0] = 1.0;
    m[5] = 1.0;
    m[10] = 1.0;
    m[15] = 1.0;
    m
}

fn mat_mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut r = [0.0; 16];
    for c in 0..4 {
        for row in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += a[k * 4 + row] * b[c * 4 + k];
            }
            r[c * 4 + row] = s;
        }
    }
    r
}

fn mat_from_trs(t: [f64; 3], q: [f64; 4], s: [f64; 3]) -> Mat4 {
    let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
    let (x2, y2, z2) = (x + x, y + y, z + z);
    let (xx, xy, xz) = (x * x2, x * y2, x * z2);
    let (yy, yz, zz) = (y * y2, y * z2, z * z2);
    let (wx, wy, wz) = (w * x2, w * y2, w * z2);

    let mut m = identity();
    m[0] = (1.0 - (yy + zz)) * s[0];
    m[1] = (xy + wz) * s[0];
    m[2] = (xz - wy) * s[0];
    m[4] = (xy - wz) * s[1];
    m[5] = (1.0 - (xx + zz)) * s[1];
    m[6] = (yz + wx) * s[1];
    m[8] = (xz + wy) * s[2];
    m[9] = (yz - wx) * s[2];
    m[10] = (1.0 - (xx + yy)) * s[2];
    m[12] = t[0];
    m[13] = t[1];
    m[14] = t[2];
    m
}

fn num_or(a: &[Json], i: usize, default: f64) -> f64 {
    a.get(i).and_then(Json::as_f64).unwrap_or(default)
}

fn node_matrix(n: &Json) -> Mat4 {
    if let Some(arr) = n.get("matrix").and_then(Json::as_arr) {
        if arr.len() == 16 {
            let mut m = [0.0; 16];
            for (i, slot) in m.iter_mut().enumerate() {
                *slot = num_or(arr, i, 0.0);
            }
            return m;
        }
    }
    let vec3 = |key: &str, default: [f64; 3]| -> [f64; 3] {
        n.get(key).and_then(Json::as_arr).map_or(default, |a| {
            [num_or(a, 0, default[0]), num_or(a, 1, default[1]), num_or(a, 2, default[2])]
        })
    };
    let t = vec3("translation", [0.0, 0.0, 0.0]);
    let s = vec3("scale", [1.0, 1.0, 1.0]);
    let q = n
        .get("rotation")
        .and_then(Json::as_arr)
        .map_or([0.0, 0.0, 0.0, 1.0], |a| {
            [num_or(a, 0, 0.0), num_or(a, 1, 0.0), num_or(a, 2, 0.0), num_or(a, 3, 1.0)]
        });
    mat_from_trs(t, q, s)
}

fn mat_apply(m: &Mat4, p: (f64, f64, f64)) -> (f64, f64, f64) {
    let (x, y, z) = p;
    (
        m[0] * x + m[4] * y + m[8] * z + m[12],
        m[1] * x + m[5] * y + m[9] * z + m[13],
        m[2] * x + m[6] * y + m[10] * z + m[14],
    )
}

/// Разбирает GLB-контейнер на JSON-чанк и бинарный чанк.
fn split_glb(bytes: &[u8]) -> Result<(&str, Option<&[u8]>), String> {
    if bytes.len() < 12 {
        return Err("файл слишком мал, чтобы быть GLB".into());
    }
    if u32_at(bytes, 0) != Some(GLB_MAGIC) {
        return Err("не GLB-контейнер (нет сигнатуры 'glTF')".into());
    }
    let total_len = u32_at(bytes, 8).unwrap_or(0) as usize;
    let limit = total_len.min(bytes.len());

    let mut off = 12usize;
    let mut json_chunk: Option<&[u8]> = None;
    let mut bin_chunk: Option<&[u8]> = None;

    while off.checked_add(8).is_some_and(|end| end <= limit) {
        let clen = u32_at(bytes, off).unwrap_or(0) as usize;
        let ctype = u32_at(bytes, off + 4).unwrap_or(0);
        let data_start = off + 8;
        let data_end = data_start.saturating_add(clen).min(bytes.len());
        let data = bytes.get(data_start..data_end).unwrap_or(&[]);
        match ctype {
            CHUNK_JSON => json_chunk = Some(data),
            CHUNK_BIN => bin_chunk = Some(data),
            _ => {}
        }
        off = data_end;
    }

    let json_bytes = json_chunk.ok_or("в GLB не найден JSON-чанк")?;
    let json_str =
        std::str::from_utf8(json_bytes).map_err(|_| "JSON-чанк не в UTF-8".to_string())?;
    Ok((json_str, bin_chunk))
}

fn arr<'a>(root: &'a Json, key: &str) -> &'a [Json] {
    root.get(key).and_then(Json::as_arr).unwrap_or(&[])
}

/// Раскладка акцессора в буфере (после проверок границ).
struct Layout<'b> {
    /// Данные буфера, начиная с первого элемента.
    data: &'b [u8],
    count: usize,
    ncomp: usize,
    comp_type: u32,
    comp_size: usize,
    stride: usize,
}

/// Разобранный документ glTF.
struct Doc<'a> {
    buffers: Vec<Vec<u8>>,
    views: &'a [Json],
    accessors: &'a [Json],
    materials: &'a [Json],
    images: &'a [Json],
    textures: &'a [Json],
    meshes: &'a [Json],
    nodes: &'a [Json],
    material_cache: HashMap<usize, ([u8; 3], Option<String>)>,
}

/// Лимиты обхода графа.
struct Budget {
    visits: usize,
    verts: usize,
}

impl<'a> Doc<'a> {
    fn layout(&self, acc_idx: usize) -> Option<Layout<'_>> {
        let acc = self.accessors.get(acc_idx)?;
        let bv = self.views.get(acc.get("bufferView")?.as_usize()?)?;
        let buf = self.buffers.get(bv.get("buffer")?.as_usize()?)?;
        let bv_off = bv.get("byteOffset").and_then(Json::as_usize).unwrap_or(0);
        let acc_off = acc.get("byteOffset").and_then(Json::as_usize).unwrap_or(0);
        let count = acc.get("count")?.as_usize()?;
        let comp_type = u32::try_from(acc.get("componentType")?.as_usize()?).ok()?;
        let ncomp = match acc.get("type")?.as_str()? {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            _ => return None,
        };
        let comp_size: usize = match comp_type {
            5120 | 5121 => 1,
            5122 | 5123 => 2,
            5125 | 5126 => 4,
            _ => return None,
        };
        let elem = comp_size * ncomp;
        let stride = bv.get("byteStride").and_then(Json::as_usize).unwrap_or(elem);
        if stride < elem || count > MAX_ACCESSOR_COUNT {
            return None;
        }
        let base = bv_off.checked_add(acc_off)?;
        let data = buf.get(base..)?;
        if count > 0 {
            // все элементы обязаны лежать внутри буфера — проверяем заранее
            let need = (count - 1).checked_mul(stride)?.checked_add(elem)?;
            if need > data.len() {
                return None;
            }
        }
        Some(Layout { data, count, ncomp, comp_type, comp_size, stride })
    }

    /// Числа акцессора как `f32` (позиции, UV).
    fn read_f32s(&self, idx: usize) -> Option<Vec<f32>> {
        let l = self.layout(idx)?;
        let mut out = Vec::with_capacity(l.count * l.ncomp);
        for i in 0..l.count {
            let elem = i * l.stride;
            for c in 0..l.ncomp {
                let o = elem + c * l.comp_size;
                let v = match l.comp_type {
                    5126 => f32_at(l.data, o)?,
                    5125 => u32_at(l.data, o)? as f32,
                    5123 => f32::from(u16_at(l.data, o)?),
                    5122 => f32::from(i16_at(l.data, o)?),
                    5121 => f32::from(*l.data.get(o)?),
                    5120 => f32::from(*l.data.get(o)? as i8),
                    _ => return None,
                };
                out.push(v);
            }
        }
        Some(out)
    }

    /// Индексы акцессора как `u32` без потери точности.
    fn read_indices(&self, idx: usize) -> Option<Vec<u32>> {
        let l = self.layout(idx)?;
        if l.ncomp != 1 {
            return None;
        }
        let mut out = Vec::with_capacity(l.count);
        for i in 0..l.count {
            let o = i * l.stride;
            let v = match l.comp_type {
                5125 => u32_at(l.data, o)?,
                5123 => u32::from(u16_at(l.data, o)?),
                5121 => u32::from(*l.data.get(o)?),
                _ => return None,
            };
            out.push(v);
        }
        Some(out)
    }

    fn material(&mut self, idx: Option<usize>) -> ([u8; 3], Option<String>) {
        let Some(mi) = idx else { return (DEFAULT_COLOR, None) };
        if let Some(hit) = self.material_cache.get(&mi) {
            return hit.clone();
        }
        let res = self.material_uncached(mi);
        self.material_cache.insert(mi, res.clone());
        res
    }

    fn material_uncached(&self, mi: usize) -> ([u8; 3], Option<String>) {
        let Some(mat) = self.materials.get(mi) else { return (DEFAULT_COLOR, None) };
        let pbr = mat.get("pbrMetallicRoughness");
        let mut rgb = DEFAULT_COLOR;
        if let Some(factor) = pbr.and_then(|p| p.get("baseColorFactor")).and_then(Json::as_arr) {
            if factor.len() >= 3 {
                let ch = |i: usize| (num_or(factor, i, 0.8) * 255.0).clamp(0.0, 255.0) as u8;
                rgb = [ch(0), ch(1), ch(2)];
            }
        }
        let tex = pbr
            .and_then(|p| p.get("baseColorTexture"))
            .and_then(|t| t.get("index"))
            .and_then(Json::as_usize)
            .and_then(|ti| self.textures.get(ti))
            .and_then(|t| t.get("source"))
            .and_then(Json::as_usize)
            .and_then(|ii| self.images.get(ii))
            .and_then(|img| self.image_source(img));
        (rgb, tex)
    }

    /// Внешняя ссылка/data-URI картинки либо встроенный в буфер файл,
    /// превращённый в data-URI.
    fn image_source(&self, img: &Json) -> Option<String> {
        if let Some(uri) = img.get("uri").and_then(Json::as_str) {
            return Some(uri.to_string());
        }
        let bv = self.views.get(img.get("bufferView")?.as_usize()?)?;
        let buf = self.buffers.get(bv.get("buffer")?.as_usize()?)?;
        let off = bv.get("byteOffset").and_then(Json::as_usize).unwrap_or(0);
        let len = bv.get("byteLength")?.as_usize()?;
        let bytes = buf.get(off..off.checked_add(len)?)?;
        let mime = img.get("mimeType").and_then(Json::as_str).unwrap_or("image/png");
        Some(format!("data:{mime};base64,{}", base64::encode(bytes)))
    }

    fn walk(
        &mut self,
        idx: usize,
        parent: &Mat4,
        path: &mut Vec<usize>,
        budget: &mut Budget,
        out: &mut Vec<Primitive>,
    ) -> Result<(), String> {
        let nodes: &'a [Json] = self.nodes;
        let meshes: &'a [Json] = self.meshes;
        let Some(node) = nodes.get(idx) else { return Ok(()) };
        if path.len() >= MAX_NODE_DEPTH || path.contains(&idx) {
            return Ok(()); // слишком глубоко или цикл в графе узлов
        }
        budget.visits += 1;
        if budget.visits > MAX_NODE_VISITS {
            return Err("слишком сложный граф узлов glTF".into());
        }
        let world = mat_mul(parent, &node_matrix(node));

        if let Some(mesh) = node
            .get("mesh")
            .and_then(Json::as_usize)
            .and_then(|mi| meshes.get(mi))
        {
            for p in mesh.get("primitives").and_then(Json::as_arr).unwrap_or(&[]) {
                self.add_primitive(p, &world, budget, out)?;
            }
        }

        if let Some(children) = node.get("children").and_then(Json::as_arr) {
            path.push(idx);
            for ci in children.iter().filter_map(Json::as_usize) {
                self.walk(ci, &world, path, budget, out)?;
            }
            path.pop();
        }
        Ok(())
    }

    fn add_primitive(
        &mut self,
        p: &Json,
        world: &Mat4,
        budget: &mut Budget,
        out: &mut Vec<Primitive>,
    ) -> Result<(), String> {
        let mode = p.get("mode").and_then(Json::as_usize).unwrap_or(4);
        if mode != 4 {
            return Ok(()); // только TRIANGLES
        }
        let Some(attrs) = p.get("attributes") else { return Ok(()) };
        let Some(pos_idx) = attrs.get("POSITION").and_then(Json::as_usize) else { return Ok(()) };
        let Some(positions) = self.read_f32s(pos_idx) else { return Ok(()) };
        let vcount = positions.len() / 3;
        if vcount == 0 {
            return Ok(());
        }
        budget.verts += vcount;
        if budget.verts > MAX_TOTAL_VERTS {
            return Err("модель слишком велика (более 16 млн вершин)".into());
        }

        let uv = attrs
            .get("TEXCOORD_0")
            .and_then(Json::as_usize)
            .and_then(|i| self.read_f32s(i))
            .filter(|u| u.len() / 2 == vcount);

        let indices: Vec<u32> = p
            .get("indices")
            .and_then(Json::as_usize)
            .and_then(|i| self.read_indices(i))
            .unwrap_or_else(|| (0..vcount as u32).collect());

        let verts: Vec<[f32; 3]> = positions
            .chunks_exact(3)
            .map(|c| {
                let (x, y, z) =
                    mat_apply(world, (f64::from(c[0]), f64::from(c[1]), f64::from(c[2])));
                // glTF — правая система координат, Y вверх; OMM использует
                // Y «вниз» (как координаты экрана).
                [x as f32, -y as f32, z as f32]
            })
            .collect();

        let uv = uv.map(|u| u.chunks_exact(2).map(|c| [c[0], c[1]]).collect::<Vec<[f32; 2]>>());

        // инверсия Y меняет ориентацию граней — компенсируем порядком вершин
        let tris: Vec<[u32; 3]> = indices.chunks_exact(3).map(|c| [c[0], c[2], c[1]]).collect();
        if tris.is_empty() {
            return Ok(());
        }

        let mat_idx = p.get("material").and_then(Json::as_usize);
        let (color, texture) = self.material(mat_idx);
        out.push(Primitive { mesh: Mesh::new(verts, uv, tris), color, texture });
        Ok(())
    }
}

pub fn glb_to_primitives(bytes: &[u8]) -> Result<Vec<Primitive>, String> {
    let (json_str, bin) = split_glb(bytes)?;
    let root = crate::json::parse(json_str).ok_or("не удалось разобрать glTF JSON")?;

    // ── Буферы ────────────────────────────────────────────────────────────
    let mut buffers: Vec<Vec<u8>> = Vec::new();
    for (i, b) in arr(&root, "buffers").iter().enumerate() {
        let data = match b.get("uri").and_then(Json::as_str) {
            Some(uri) => match uri.find(";base64,") {
                Some(at) => uri
                    .get(at + 8..)
                    .and_then(base64::decode)
                    .unwrap_or_default(),
                None => Vec::new(), // внешний файл — не поддерживается
            },
            None if i == 0 => bin.map(<[u8]>::to_vec).unwrap_or_default(),
            None => Vec::new(),
        };
        buffers.push(data);
    }

    let nodes = arr(&root, "nodes");
    let mut doc = Doc {
        buffers,
        views: arr(&root, "bufferViews"),
        accessors: arr(&root, "accessors"),
        materials: arr(&root, "materials"),
        images: arr(&root, "images"),
        textures: arr(&root, "textures"),
        meshes: arr(&root, "meshes"),
        nodes,
        material_cache: HashMap::new(),
    };

    // ── Корневые узлы ─────────────────────────────────────────────────────
    let scene_idx = root.get("scene").and_then(Json::as_usize).unwrap_or(0);
    let roots: Vec<usize> = match arr(&root, "scenes")
        .get(scene_idx)
        .and_then(|s| s.get("nodes"))
        .and_then(Json::as_arr)
    {
        Some(list) => list.iter().filter_map(Json::as_usize).collect(),
        None => {
            // сцены нет — корни это узлы без родителя
            let mut is_child = vec![false; nodes.len()];
            for n in nodes {
                for ci in n
                    .get("children")
                    .and_then(Json::as_arr)
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(Json::as_usize)
                {
                    if let Some(slot) = is_child.get_mut(ci) {
                        *slot = true;
                    }
                }
            }
            (0..nodes.len()).filter(|&i| !is_child[i]).collect()
        }
    };

    let mut primitives: Vec<Primitive> = Vec::new();
    let ident = identity();
    let mut budget = Budget { visits: 0, verts: 0 };
    let mut path: Vec<usize> = Vec::new();
    for ni in roots {
        doc.walk(ni, &ident, &mut path, &mut budget, &mut primitives)?;
    }

    if primitives.is_empty() {
        return Err(
            "в glTF/GLB не найдено треугольных мешей (поддерживается только режим TRIANGLES)"
                .into(),
        );
    }
    Ok(primitives)
}

/// Конвертирует байты `.glb` целиком в готовый `.omm`-текст.
pub fn glb_to_omm(bytes: &[u8]) -> Result<String, String> {
    let prims = glb_to_primitives(bytes)?;
    Ok(crate::convert::primitives_to_omm(&prims))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Собирает GLB из JSON и (необязательного) бинарного чанка.
    fn make_glb(json: &str, bin: &[u8]) -> Vec<u8> {
        fn pad(v: &mut Vec<u8>, byte: u8) {
            while v.len() % 4 != 0 {
                v.push(byte);
            }
        }
        let mut j = json.as_bytes().to_vec();
        pad(&mut j, b' ');
        let mut b = bin.to_vec();
        pad(&mut b, 0);
        let total = 12 + 8 + j.len() + if bin.is_empty() { 0 } else { 8 + b.len() };
        let mut out = Vec::new();
        out.extend_from_slice(&GLB_MAGIC.to_le_bytes());
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(j.len() as u32).to_le_bytes());
        out.extend_from_slice(&CHUNK_JSON.to_le_bytes());
        out.extend_from_slice(&j);
        if !bin.is_empty() {
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(&CHUNK_BIN.to_le_bytes());
            out.extend_from_slice(&b);
        }
        out
    }

    /// 3 вершины (0,0,0) (1,0,0) (0,1,0) + индексы u16 0,1,2.
    fn triangle_bin() -> Vec<u8> {
        let mut bin = Vec::new();
        for v in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            bin.extend_from_slice(&v.to_le_bytes());
        }
        for i in [0u16, 1, 2] {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        bin
    }

    const TRIANGLE_JSON: &str = r#"{
      "asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],
      "nodes":[{"mesh":0,"translation":[10,20,30]}],
      "meshes":[{"primitives":[{"attributes":{"POSITION":0},"indices":1,"material":0}]}],
      "materials":[{"pbrMetallicRoughness":{"baseColorFactor":[1,0.5,0,1]}}],
      "buffers":[{"byteLength":42}],
      "bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":36},{"buffer":0,"byteOffset":36,"byteLength":6}],
      "accessors":[
        {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"},
        {"bufferView":1,"componentType":5123,"count":3,"type":"SCALAR"}]
    }"#;

    #[test]
    fn triangle_with_node_translation_material_and_flips() {
        let glb = make_glb(TRIANGLE_JSON, &triangle_bin());
        let prims = glb_to_primitives(&glb).unwrap();
        assert_eq!(prims.len(), 1);
        let p = &prims[0];
        // узел сдвигает на (10,20,30); Y затем инвертируется
        assert_eq!(p.mesh.v, vec![[10.0, -20.0, 30.0], [11.0, -20.0, 30.0], [10.0, -21.0, 30.0]]);
        // порядок вершин развёрнут: [c0, c2, c1]
        assert_eq!(p.mesh.tris, vec![[0, 2, 1]]);
        assert_eq!(p.color, [255, 127, 0]);
        assert!(p.texture.is_none());
    }

    #[test]
    fn full_pipeline_through_omm_text() {
        let text = glb_to_omm(&make_glb(TRIANGLE_JSON, &triangle_bin())).unwrap();
        assert!(text.contains("color(255, 127, 0)"));
        let run = crate::vm::run(&crate::parser::parse(&text));
        assert!(run.warnings.is_empty(), "{:?}", run.warnings);
        let m = run.world.objects[0].mesh.as_ref().unwrap();
        assert_eq!(m.v[1], [11.0, -20.0, 30.0]);
        assert_eq!(m.tris, vec![[0, 2, 1]]);
    }

    #[test]
    fn container_errors_have_the_v3_messages() {
        assert_eq!(glb_to_primitives(&[1, 2, 3]).unwrap_err(), "файл слишком мал, чтобы быть GLB");
        assert_eq!(
            glb_to_primitives(&[0u8; 20]).unwrap_err(),
            "не GLB-контейнер (нет сигнатуры 'glTF')"
        );
        // заголовок без чанков
        let mut hdr = GLB_MAGIC.to_le_bytes().to_vec();
        hdr.extend_from_slice(&2u32.to_le_bytes());
        hdr.extend_from_slice(&12u32.to_le_bytes());
        assert_eq!(glb_to_primitives(&hdr).unwrap_err(), "в GLB не найден JSON-чанк");
        let bad = make_glb("{ это не json", &[]);
        assert_eq!(glb_to_primitives(&bad).unwrap_err(), "не удалось разобрать glTF JSON");
        let empty = make_glb(r#"{"asset":{"version":"2.0"}}"#, &[]);
        assert!(glb_to_primitives(&empty).unwrap_err().contains("не найдено треугольных мешей"));
    }

    #[test]
    fn non_triangle_modes_are_skipped() {
        let json = TRIANGLE_JSON.replace(r#""attributes""#, r#""mode":1,"attributes""#);
        assert!(glb_to_primitives(&make_glb(&json, &triangle_bin())).is_err());
    }

    #[test]
    fn missing_indices_mean_sequential_triangles() {
        let json = TRIANGLE_JSON.replace(r#""indices":1,"#, "");
        let prims = glb_to_primitives(&make_glb(&json, &triangle_bin())).unwrap();
        assert_eq!(prims[0].mesh.tris, vec![[0, 2, 1]]);
    }

    #[test]
    fn node_cycles_do_not_recurse_forever() {
        // узел 0 — потомок самого себя; а также 1 ↔ 2
        let json = r#"{
          "scene":0,"scenes":[{"nodes":[0,1]}],
          "nodes":[{"mesh":0,"children":[0]},{"children":[2]},{"mesh":0,"children":[1]}],
          "meshes":[{"primitives":[{"attributes":{"POSITION":0},"indices":1}]}],
          "buffers":[{"byteLength":42}],
          "bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":36},{"buffer":0,"byteOffset":36,"byteLength":6}],
          "accessors":[
            {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"},
            {"bufferView":1,"componentType":5123,"count":3,"type":"SCALAR"}]
        }"#;
        let prims = glb_to_primitives(&make_glb(json, &triangle_bin())).unwrap();
        // узел 0 даёт 1 меш; узел 1 → 2 даёт ещё 1 (возврат к 1 отсекается как цикл)
        assert_eq!(prims.len(), 2);
    }

    #[test]
    fn deep_chains_are_cut_off() {
        let n = 5000;
        let mut nodes = String::new();
        for i in 0..n {
            if i > 0 {
                nodes.push(',');
            }
            nodes.push_str(&format!(r#"{{"children":[{}]}}"#, i + 1));
        }
        nodes.push_str(r#",{"mesh":0}"#);
        let json = format!(
            r#"{{"scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{nodes}],
              "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}}}}]}}],
              "buffers":[{{"byteLength":36}}],
              "bufferViews":[{{"buffer":0,"byteLength":36}}],
              "accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}}]}}"#
        );
        // мешу на глубине 5000 до него не добраться — это ошибка «нет мешей», а не переполнение стека
        assert!(glb_to_primitives(&make_glb(&json, &triangle_bin()[..36])).is_err());
    }

    #[test]
    fn without_scenes_only_parentless_nodes_are_roots() {
        let json = r#"{
          "nodes":[{"children":[1]},{"mesh":0}],
          "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
          "buffers":[{"byteLength":36}],
          "bufferViews":[{"buffer":0,"byteLength":36}],
          "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}]
        }"#;
        let prims = glb_to_primitives(&make_glb(json, &triangle_bin()[..36])).unwrap();
        assert_eq!(prims.len(), 1, "дочерний узел не должен попасть в результат дважды");
    }

    #[test]
    fn hostile_accessors_are_rejected_without_allocating() {
        for count in ["4000000000", "20000001", "100"] {
            let json = TRIANGLE_JSON.replace(r#""count":3,"type":"VEC3""#, &format!(r#""count":{count},"type":"VEC3""#));
            assert!(glb_to_primitives(&make_glb(&json, &triangle_bin())).is_err(), "count={count}");
        }
        // stride меньше размера элемента
        let json = TRIANGLE_JSON.replace(r#""byteLength":36}"#, r#""byteLength":36,"byteStride":4}"#);
        assert!(glb_to_primitives(&make_glb(&json, &triangle_bin())).is_err());
        // смещение за пределами буфера
        let json = TRIANGLE_JSON.replace(r#""byteOffset":0,"byteLength":36"#, r#""byteOffset":99999,"byteLength":36"#);
        assert!(glb_to_primitives(&make_glb(&json, &triangle_bin())).is_err());
        // переполнение при сложении смещений
        let json = TRIANGLE_JSON.replace(r#""byteOffset":0,"byteLength":36"#, r#""byteOffset":9000000000000000,"byteLength":36"#);
        assert!(glb_to_primitives(&make_glb(&json, &triangle_bin())).is_err());
    }

    #[test]
    fn indices_above_2_pow_24_keep_their_precision() {
        // индекс 16_777_217 нельзя представить в f32 — раньше он портился
        let mut bin = Vec::new();
        for v in [0.0f32; 9] {
            bin.extend_from_slice(&v.to_le_bytes());
        }
        for i in [0u32, 1, 16_777_217] {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        let json = TRIANGLE_JSON
            .replace(r#""byteLength":42"#, r#""byteLength":48"#)
            .replace(r#""byteLength":6}"#, r#""byteLength":12}"#)
            .replace(r#""componentType":5123,"count":3"#, r#""componentType":5125,"count":3"#);
        let glb = make_glb(&json, &bin);
        let (json_str, bin_chunk) = split_glb(&glb).unwrap();
        let root = crate::json::parse(json_str).unwrap();
        let doc = Doc {
            buffers: vec![bin_chunk.unwrap().to_vec()],
            views: arr(&root, "bufferViews"),
            accessors: arr(&root, "accessors"),
            materials: &[],
            images: &[],
            textures: &[],
            meshes: &[],
            nodes: &[],
            material_cache: HashMap::new(),
        };
        assert_eq!(doc.read_indices(1).unwrap(), vec![0, 1, 16_777_217]);
    }

    #[test]
    fn embedded_texture_becomes_a_data_uri_and_uv_is_read() {
        let mut bin = triangle_bin(); // 42 байт позиций+индексов, добиваем до 44
        bin.extend_from_slice(&[0, 0]);
        for v in [0.0f32, 0.0, 1.0, 0.0, 0.0, 1.0] {
            bin.extend_from_slice(&v.to_le_bytes());
        }
        bin.extend_from_slice(&[0x89, b'P', b'N', b'G']); // «картинка»
        let json = r#"{
          "scene":0,"scenes":[{"nodes":[0]}],
          "nodes":[{"mesh":0}],
          "meshes":[{"primitives":[{"attributes":{"POSITION":0,"TEXCOORD_0":2},"indices":1,"material":0}]}],
          "materials":[{"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}],
          "textures":[{"source":0}],
          "images":[{"bufferView":3,"mimeType":"image/png"}],
          "buffers":[{"byteLength":72}],
          "bufferViews":[
            {"buffer":0,"byteOffset":0,"byteLength":36},
            {"buffer":0,"byteOffset":36,"byteLength":6},
            {"buffer":0,"byteOffset":44,"byteLength":24},
            {"buffer":0,"byteOffset":68,"byteLength":4}],
          "accessors":[
            {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"},
            {"bufferView":1,"componentType":5123,"count":3,"type":"SCALAR"},
            {"bufferView":2,"componentType":5126,"count":3,"type":"VEC2"}]
        }"#;
        let prims = glb_to_primitives(&make_glb(json, &bin)).unwrap();
        let p = &prims[0];
        assert_eq!(p.mesh.uv.as_ref().unwrap()[1], [1.0, 0.0]);
        let want = format!("data:image/png;base64,{}", base64::encode(&[0x89, b'P', b'N', b'G']));
        assert_eq!(p.texture.as_deref(), Some(want.as_str()));
        // без baseColorFactor цвет по умолчанию
        assert_eq!(p.color, [200, 200, 200]);
    }

    #[test]
    fn node_matrix_and_trs_compose_like_gltf() {
        // поворот на 90° вокруг Z (кватернион) и масштаб 2: (1,0,0) → (0,2,0)
        let n = crate::json::parse(r#"{"rotation":[0,0,0.7071067811865476,0.7071067811865476],"scale":[2,2,2]}"#).unwrap();
        let (x, y, z) = mat_apply(&node_matrix(&n), (1.0, 0.0, 0.0));
        assert!(x.abs() < 1e-9 && (y - 2.0).abs() < 1e-9 && z.abs() < 1e-9, "{x} {y} {z}");
        let m = crate::json::parse(r#"{"matrix":[1,0,0,0, 0,1,0,0, 0,0,1,0, 5,6,7,1]}"#).unwrap();
        assert_eq!(mat_apply(&node_matrix(&m), (1.0, 1.0, 1.0)), (6.0, 7.0, 8.0));
    }

    #[test]
    fn truncated_files_never_panic() {
        let glb = make_glb(TRIANGLE_JSON, &triangle_bin());
        for cut in 0..glb.len() {
            let _ = glb_to_primitives(&glb[..cut]);
        }
    }

    #[test]
    fn data_uri_buffers_are_supported() {
        let b64 = base64::encode(&triangle_bin());
        let json = TRIANGLE_JSON.replace(
            r#""buffers":[{"byteLength":42}]"#,
            &format!(r#""buffers":[{{"byteLength":42,"uri":"data:application/octet-stream;base64,{b64}"}}]"#),
        );
        let prims = glb_to_primitives(&make_glb(&json, &[])).unwrap();
        assert_eq!(prims.len(), 1);
    }
}
