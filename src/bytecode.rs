//! Байт-код OMM — контракт между парсером и виртуальной машиной.
//!
//! ```text
//!  текст .omm ──parser──▶ Program (байт-код) ──vm──▶ данные для viewer
//! ```
//!
//! Парсер превращает текст в [`Program`] и **больше ничего не знает о сцене**:
//! он не создаёт ни объектов, ни геометрии, не применяет групповые
//! преобразования. Смысл инструкций определяет только VM (`crate::vm`).
//!
//! # Кодирование
//!
//! Инструкция = 1 байт кода операции + операнды (все числа little-endian):
//!
//! | операнд | размер |
//! |---------|--------|
//! | `u8`    | 1      |
//! | `u32` (индекс в пуле строк/блобов) | 4 |
//! | `f64`   | 8      |
//!
//! Читатель ([`Reader`]) проверяет границы на каждом шаге, поэтому
//! испорченный или враждебный байт-код не может вызвать панику.

/// Коды операций.
pub mod op {
    /// Конец программы.
    pub const END: u8 = 0x00;
    /// `u8 shape` — начать новый объект (предыдущий завершается).
    pub const SHAPE: u8 = 0x01;
    /// `f64 n` — делитель высоты (`cube3 : n`).
    pub const DIVISOR: u8 = 0x02;
    /// Начало блока `mono( … )`.
    pub const GROUP_BEGIN: u8 = 0x03;
    /// Конец блока `mono( … )`.
    pub const GROUP_END: u8 = 0x04;

    /// `f64` — `x(...)`.
    pub const X: u8 = 0x10;
    pub const Y: u8 = 0x11;
    pub const Z: u8 = 0x12;
    /// `f64` — `scale(...)` (множитель).
    pub const SCALE: u8 = 0x13;
    /// `f64` градусы — `rr(...)`.
    pub const ROT_Y: u8 = 0x14;
    /// `f64` градусы — `ru(...)`.
    pub const ROT_X: u8 = 0x15;
    /// `u8 u8 u8` — `color(r, g, b)`.
    pub const COLOR: u8 = 0x16;
    /// `u32 string` — `texture(...)`.
    pub const TEXTURE: u8 = 0x17;
    /// `u8 which, f64` — `ur ul ud uu ug um`, см. [`super::DEFORM_NAMES`].
    pub const DEFORM: u8 = 0x18;

    /// Начало списка ключевых кадров `animation(...)`.
    pub const ANIM_BEGIN: u8 = 0x20;
    /// `u8 mask`, затем по `f64` на каждый установленный бит маски
    /// (порядок: x, y, z, rr, ru), см. `KF_*`.
    pub const KEYFRAME: u8 = 0x21;
    pub const ANIM_END: u8 = 0x22;

    /// `u32 blob` — упакованный меш (`data(...)`, формат `Mesh::pack`).
    pub const MESH_DATA: u8 = 0x30;
    /// `u32 blob` — вершины: `f32 × 3` на вершину.
    pub const MESH_VERTS: u8 = 0x31;
    /// `u32 blob` — UV: `f32 × 2` на вершину.
    pub const MESH_UVS: u8 = 0x32;
    /// `u32 blob` — треугольники: `u32 × 3`.
    pub const MESH_TRIS: u8 = 0x33;
}

/// Ключевые слова фигур; индекс = операнд `SHAPE`.
pub const SHAPE_NAMES: [&str; 7] =
    ["cube3", "pyramid3", "triangle3", "sphere3", "cylinder3", "image3", "mesh3"];

/// Свойства растяжения; индекс = операнд `DEFORM`.
pub const DEFORM_NAMES: [&str; 6] = ["ur", "ul", "ud", "uu", "ug", "um"];

pub const KF_X: u8 = 1;
pub const KF_Y: u8 = 2;
pub const KF_Z: u8 = 4;
pub const KF_RR: u8 = 8;
pub const KF_RU: u8 = 16;

/// Номер фигуры по ключевому слову (`"cube3"` → `Some(0)`).
pub fn shape_id(keyword: &str) -> Option<u8> {
    SHAPE_NAMES.iter().position(|k| *k == keyword).map(|i| i as u8)
}

/// Номер растяжения по имени свойства (`"ur"` → `Some(0)`).
pub fn deform_id(name: &str) -> Option<u8> {
    DEFORM_NAMES.iter().position(|k| *k == name).map(|i| i as u8)
}

/// Скомпилированная программа.
#[derive(Clone, Debug, Default)]
pub struct Program {
    /// Инструкции.
    pub code: Vec<u8>,
    /// Пул строк (источники текстур).
    pub strings: Vec<String>,
    /// Пул двоичных блоков (данные мешей).
    pub blobs: Vec<Vec<u8>>,
    /// Замечания, найденные при разборе текста.
    pub warnings: Vec<String>,
    /// Таблица строк: пары (смещение в `code`, номер строки исходника) по
    /// возрастанию смещения. Нужна только для понятных сообщений VM.
    pub lines: Vec<(u32, u32)>,
}

impl Program {
    pub fn new() -> Program {
        Program::default()
    }

    // ── эмиссия ─────────────────────────────────────────────────────────────

    pub fn emit(&mut self, opcode: u8) {
        self.code.push(opcode);
    }

    pub fn emit_u8(&mut self, opcode: u8, a: u8) {
        self.code.push(opcode);
        self.code.push(a);
    }

    pub fn emit_f64(&mut self, opcode: u8, v: f64) {
        self.code.push(opcode);
        self.code.extend_from_slice(&v.to_le_bytes());
    }

    pub fn emit_u32(&mut self, opcode: u8, v: u32) {
        self.code.push(opcode);
        self.code.extend_from_slice(&v.to_le_bytes());
    }

    pub fn emit_color(&mut self, rgb: [u8; 3]) {
        self.code.push(op::COLOR);
        self.code.extend_from_slice(&rgb);
    }

    pub fn emit_deform(&mut self, which: u8, v: f64) {
        self.code.push(op::DEFORM);
        self.code.push(which);
        self.code.extend_from_slice(&v.to_le_bytes());
    }

    /// `vals` — значения в порядке битов маски (x, y, z, rr, ru).
    pub fn emit_keyframe(&mut self, mask: u8, vals: &[f64]) {
        self.code.push(op::KEYFRAME);
        self.code.push(mask);
        for v in vals {
            self.code.extend_from_slice(&v.to_le_bytes());
        }
    }

    /// Добавляет строку в пул (с дедупликацией) и возвращает её индекс.
    pub fn intern_string(&mut self, s: &str) -> u32 {
        if let Some(i) = self.strings.iter().position(|t| t.as_str() == s) {
            return i as u32;
        }
        self.strings.push(s.to_string());
        (self.strings.len() - 1) as u32
    }

    /// Добавляет блок данных в пул и возвращает его индекс.
    pub fn add_blob(&mut self, data: Vec<u8>) -> u32 {
        self.blobs.push(data);
        (self.blobs.len() - 1) as u32
    }

    /// Запоминает, что следующие инструкции относятся к строке `line`.
    pub fn mark_line(&mut self, line: u32) {
        let pc = self.code.len() as u32;
        match self.lines.last_mut() {
            Some(last) if last.1 == line => {}
            Some(last) if last.0 == pc => last.1 = line,
            _ => self.lines.push((pc, line)),
        }
    }

    /// Номер строки исходника для смещения `pc` в `code`.
    pub fn line_at(&self, pc: usize) -> Option<u32> {
        let n = self.lines.partition_point(|&(p, _)| (p as usize) <= pc);
        if n == 0 {
            None
        } else {
            Some(self.lines[n - 1].1)
        }
    }

    // ── контейнер (для кэширования/передачи) ────────────────────────────────

    /// Упаковывает программу в самодостаточный двоичный контейнер `OMMB`.
    /// Предупреждения и таблица строк в контейнер не входят.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + self.code.len());
        out.extend_from_slice(b"OMMB");
        out.push(1); // версия
        out.extend_from_slice(&(self.code.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.code);
        out.extend_from_slice(&(self.strings.len() as u32).to_le_bytes());
        for s in &self.strings {
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        out.extend_from_slice(&(self.blobs.len() as u32).to_le_bytes());
        for b in &self.blobs {
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(b);
        }
        out
    }

    /// Обратная операция к [`Program::to_bytes`]. Любое несоответствие
    /// размеров → `None` (память заранее не выделяется по заявленным счётчикам).
    pub fn from_bytes(bytes: &[u8]) -> Option<Program> {
        if bytes.get(0..4)? != b"OMMB" || *bytes.get(4)? != 1 {
            return None;
        }
        let mut r = Reader::new(bytes);
        r.pc = 5;
        let mut prog = Program::new();
        prog.code = r.chunk()?.to_vec();
        let n_strings = r.u32()? as usize;
        for _ in 0..n_strings {
            let s = std::str::from_utf8(r.chunk()?).ok()?;
            prog.strings.push(s.to_string());
        }
        let n_blobs = r.u32()? as usize;
        for _ in 0..n_blobs {
            prog.blobs.push(r.chunk()?.to_vec());
        }
        Some(prog)
    }

    // ── отладка ─────────────────────────────────────────────────────────────

    /// Человекочитаемый листинг (по инструкции на строку).
    pub fn disassemble(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        let mut r = Reader::new(&self.code);
        while let Some(opcode) = r.u8() {
            match self.disassemble_one(&mut r, opcode) {
                Some(text) => lines.push(text),
                None => {
                    lines.push("<обрыв или неизвестная инструкция>".to_string());
                    break;
                }
            }
            if opcode == op::END {
                break;
            }
        }
        lines.join("\n")
    }

    fn disassemble_one(&self, r: &mut Reader, opcode: u8) -> Option<String> {
        Some(match opcode {
            op::END => "END".to_string(),
            op::SHAPE => {
                let id = r.u8()?;
                format!("SHAPE {}", SHAPE_NAMES.get(usize::from(id)).copied().unwrap_or("?"))
            }
            op::DIVISOR => format!("DIVISOR {}", r.f64()?),
            op::GROUP_BEGIN => "GROUP_BEGIN".to_string(),
            op::GROUP_END => "GROUP_END".to_string(),
            op::X => format!("X {}", r.f64()?),
            op::Y => format!("Y {}", r.f64()?),
            op::Z => format!("Z {}", r.f64()?),
            op::SCALE => format!("SCALE {}", r.f64()?),
            op::ROT_Y => format!("ROT_Y {}", r.f64()?),
            op::ROT_X => format!("ROT_X {}", r.f64()?),
            op::COLOR => format!("COLOR {} {} {}", r.u8()?, r.u8()?, r.u8()?),
            op::TEXTURE => {
                let i = r.u32()?;
                let s = self.strings.get(i as usize).map_or("?", |s| s.as_str());
                format!("TEXTURE #{i} {s:?}")
            }
            op::DEFORM => {
                let which = r.u8()?;
                let name = DEFORM_NAMES.get(usize::from(which)).copied().unwrap_or("?");
                format!("DEFORM {name} {}", r.f64()?)
            }
            op::ANIM_BEGIN => "ANIM_BEGIN".to_string(),
            op::KEYFRAME => {
                let mask = r.u8()?;
                let mut text = String::from("KEYFRAME");
                for (bit, name) in [(KF_X, "x"), (KF_Y, "y"), (KF_Z, "z"), (KF_RR, "rr"), (KF_RU, "ru")] {
                    if mask & bit != 0 {
                        text.push_str(&format!(" {name}={}", r.f64()?));
                    }
                }
                text
            }
            op::ANIM_END => "ANIM_END".to_string(),
            op::MESH_DATA | op::MESH_VERTS | op::MESH_UVS | op::MESH_TRIS => {
                let name = match opcode {
                    op::MESH_DATA => "MESH_DATA",
                    op::MESH_VERTS => "MESH_VERTS",
                    op::MESH_UVS => "MESH_UVS",
                    _ => "MESH_TRIS",
                };
                let i = r.u32()?;
                let len = self.blobs.get(i as usize).map_or(0, Vec::len);
                format!("{name} #{i} ({len} bytes)")
            }
            _ => return None,
        })
    }
}

/// Безопасный читатель байт-кода: любой выход за границы → `None`.
pub struct Reader<'a> {
    code: &'a [u8],
    /// Смещение следующего байта.
    pub pc: usize,
}

impl<'a> Reader<'a> {
    pub fn new(code: &'a [u8]) -> Reader<'a> {
        Reader { code, pc: 0 }
    }

    pub fn u8(&mut self) -> Option<u8> {
        let b = *self.code.get(self.pc)?;
        self.pc += 1;
        Some(b)
    }

    pub fn u32(&mut self) -> Option<u32> {
        let v = crate::bytes::u32_at(self.code, self.pc)?;
        self.pc += 4;
        Some(v)
    }

    pub fn f64(&mut self) -> Option<f64> {
        let v = crate::bytes::f64_at(self.code, self.pc)?;
        self.pc += 8;
        Some(v)
    }

    /// Блок «u32 длина + данные» (для контейнера).
    fn chunk(&mut self) -> Option<&'a [u8]> {
        let len = self.u32()? as usize;
        let end = self.pc.checked_add(len)?;
        let data = self.code.get(self.pc..end)?;
        self.pc = end;
        Some(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_and_disassemble() {
        let mut p = Program::new();
        p.emit_u8(op::SHAPE, shape_id("sphere3").unwrap());
        p.emit_f64(op::DIVISOR, 4.0);
        p.emit_f64(op::X, -20.5);
        p.emit_color([80, 140, 255]);
        let t = p.intern_string("a.png");
        p.emit_u32(op::TEXTURE, t);
        p.emit_deform(deform_id("ud").unwrap(), 3.0);
        p.emit(op::ANIM_BEGIN);
        p.emit_keyframe(KF_Y | KF_RR, &[200.0, 90.0]);
        p.emit(op::ANIM_END);
        let b = p.add_blob(vec![1, 2, 3]);
        p.emit_u32(op::MESH_DATA, b);
        p.emit(op::END);
        assert_eq!(
            p.disassemble(),
            "SHAPE sphere3\nDIVISOR 4\nX -20.5\nCOLOR 80 140 255\nTEXTURE #0 \"a.png\"\n\
             DEFORM ud 3\nANIM_BEGIN\nKEYFRAME y=200 rr=90\nANIM_END\nMESH_DATA #0 (3 bytes)\nEND"
        );
    }

    #[test]
    fn pools_deduplicate_strings_only() {
        let mut p = Program::new();
        assert_eq!(p.intern_string("a"), 0);
        assert_eq!(p.intern_string("b"), 1);
        assert_eq!(p.intern_string("a"), 0);
        assert_eq!(p.add_blob(vec![]), 0);
        assert_eq!(p.add_blob(vec![]), 1);
    }

    #[test]
    fn reader_is_bounds_checked() {
        let mut r = Reader::new(&[1, 2]);
        assert_eq!(r.u8(), Some(1));
        assert_eq!(r.u32(), None);
        assert_eq!(r.f64(), None);
        assert_eq!(r.u8(), Some(2));
        assert_eq!(r.u8(), None);
    }

    #[test]
    fn truncated_code_disassembles_without_panic() {
        let mut p = Program::new();
        p.emit_f64(op::X, 1.0);
        p.code.truncate(4);
        assert!(p.disassemble().contains("обрыв"));
        p.code = vec![0xEE];
        assert!(p.disassemble().contains("неизвестная"));
    }

    #[test]
    fn line_table() {
        let mut p = Program::new();
        p.mark_line(1);
        p.emit(op::GROUP_BEGIN); // pc 0
        p.mark_line(1); // та же строка — не дублируется
        p.mark_line(3);
        p.emit(op::GROUP_END); // pc 1
        assert_eq!(p.lines, vec![(0, 1), (1, 3)]);
        assert_eq!(p.line_at(0), Some(1));
        assert_eq!(p.line_at(1), Some(3));
        assert_eq!(p.line_at(99), Some(3));
        assert_eq!(Program::new().line_at(0), None);
    }

    #[test]
    fn container_roundtrip_and_hostile_input() {
        let mut p = Program::new();
        p.emit_f64(op::X, 1.0);
        p.intern_string("тест.png");
        p.add_blob(vec![9; 40]);
        p.emit(op::END);
        let bytes = p.to_bytes();
        let back = Program::from_bytes(&bytes).expect("roundtrip");
        assert_eq!(back.code, p.code);
        assert_eq!(back.strings, p.strings);
        assert_eq!(back.blobs, p.blobs);

        // любое усечение — None, а не паника
        for cut in 0..bytes.len() {
            assert!(Program::from_bytes(&bytes[..cut]).is_none(), "cut={cut}");
        }
        // заявленный огромный размер блока
        let mut evil = b"OMMB\x01".to_vec();
        evil.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(Program::from_bytes(&evil).is_none());
        assert!(Program::from_bytes(b"XXXX\x01").is_none());
    }
}
