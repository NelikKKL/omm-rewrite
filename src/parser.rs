//! Парсер `.omm`: текст → байт-код ([`Program`]).
//!
//! Парсер **ничего не знает о сцене**: он не создаёт объектов, не считает
//! группы, не собирает меши и не знает про геометрию — он только читает
//! синтаксис и эмитит инструкции (см. [`crate::bytecode`]). Всё, что
//! инструкции *означают*, решает VM.
//!
//! Синтаксис не изменился по сравнению с OMM 3.x: имя фигуры (`cube3`,
//! `sphere3`, …, необязательно `: N` — делитель высоты), затем свойства
//! вида `имя(значение)` в любом количестве на строке. Комментарии —
//! строки, начинающиеся с `//` (а также хвост строки после `//`).
//! Блок `mono( … )` объединяет фигуры в группу.
//!
//! Сверх старого синтаксиса (всё — надмножество, старые файлы разбираются
//! так же): параметры `verts/uvs/tris/data/animation` можно переносить на
//! несколько строк (внутри списков допустимы `//`-комментарии), свойства
//! можно писать в одной строке с именем фигуры, `mono()` вкладываются.
//!
//! Парсер не паникует и не возвращает ошибок: всё подозрительное попадает
//! в [`Program::warnings`].

use crate::base64;
use crate::bytecode::{deform_id, op, shape_id, Program, KF_RR, KF_RU, KF_X, KF_Y, KF_Z};

/// Максимальная вложенность `mono()`.
pub const MAX_DEPTH: usize = 16;
const MAX_WARNINGS: usize = 100;

/// Компилирует текст `.omm` в байт-код. Никогда не паникует.
pub fn parse(src: &str) -> Program {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let mut p = Parser {
        src,
        b: src.as_bytes(),
        pos: 0,
        prog: Program::new(),
        depth: 0,
        stray: 0,
        line: 1,
        line_pos: 0,
        warnings_capped: false,
    };
    p.run();
    p.prog
}

fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

struct Parser<'a> {
    src: &'a str,
    b: &'a [u8],
    pos: usize,
    prog: Program,
    /// Сколько блоков `mono(` сейчас открыто.
    depth: usize,
    /// Сколько «посторонних» `(` внутри групп ещё ждут своей `)` (как и в
    /// OMM 3.x, скобки в группе считаются по глубине — поэтому пояснение
    /// вроде `← группа-трансформ (необязательно)` не закрывает блок).
    stray: usize,
    /// Номер строки в позиции `line_pos` (считается инкрементально).
    line: u32,
    line_pos: usize,
    warnings_capped: bool,
}

impl<'a> Parser<'a> {
    // ── низкоуровневые помощники ────────────────────────────────────────────

    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    /// Подстрока `[a, b)`; при попадании внутрь многобайтного символа
    /// (чего не должно быть) — пустая строка вместо паники.
    fn slice(&self, a: usize, b: usize) -> &'a str {
        self.src.get(a..b).unwrap_or("")
    }

    /// Номер строки для позиции `pos` (позиции запрашиваются по возрастанию).
    fn line_of(&mut self, pos: usize) -> u32 {
        let pos = pos.min(self.b.len());
        if pos > self.line_pos {
            let n = self.b[self.line_pos..pos].iter().filter(|&&c| c == b'\n').count();
            self.line = self.line.saturating_add(n as u32);
            self.line_pos = pos;
        }
        self.line
    }

    fn warn(&mut self, pos: usize, msg: &str) {
        if self.prog.warnings.len() >= MAX_WARNINGS {
            if !self.warnings_capped {
                self.warnings_capped = true;
                self.prog
                    .warnings
                    .push("слишком много предупреждений, остальные скрыты".to_string());
            }
            return;
        }
        let line = self.line_of(pos);
        self.prog.warnings.push(format!("строка {line}: {msg}"));
    }

    /// Пропускает пробелы, переводы строк и комментарии `// …`.
    fn skip_blank(&mut self) {
        while let Some(c) = self.peek() {
            match c {
                b' ' | b'\t' | b'\r' | b'\n' => self.pos += 1,
                b'/' if self.b.get(self.pos + 1) == Some(&b'/') => {
                    while self.peek().is_some_and(|c| c != b'\n') {
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn skip_inline_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r')) {
            self.pos += 1;
        }
    }

    fn read_ident(&mut self) -> &'a str {
        let start = self.pos;
        while self.peek().is_some_and(is_ident_char) {
            self.pos += 1;
        }
        self.slice(start, self.pos)
    }

    /// Позиция закрывающей `)` для значения, которое начинается с `from`.
    /// Обычные свойства живут в пределах одной строки (как в старой версии),
    /// списки — могут занимать несколько.
    fn find_close(&self, from: usize, multiline: bool) -> Option<usize> {
        let mut i = from;
        while i < self.b.len() {
            match self.b[i] {
                b')' => return Some(i),
                b'\n' if !multiline => return None,
                _ => i += 1,
            }
        }
        None
    }

    /// Пропускает всё до парной `)` (скобку `(` уже съели).
    fn skip_balanced(&mut self) {
        let mut depth = 1usize;
        while let Some(c) = self.peek() {
            self.pos += 1;
            match c {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    // ── основной цикл ───────────────────────────────────────────────────────

    fn run(&mut self) {
        loop {
            self.skip_blank();
            let Some(c) = self.peek() else { break };

            if c == b')' {
                self.pos += 1;
                if self.stray > 0 {
                    self.stray -= 1; // закрывает посторонний `(`, а не группу
                } else if self.depth > 0 {
                    self.prog.emit(op::GROUP_END);
                    self.depth -= 1;
                }
                continue;
            }
            if !(c.is_ascii_alphabetic() || c == b'_') {
                if c == b'(' && self.depth > 0 {
                    self.stray = self.stray.saturating_add(1);
                }
                self.pos += 1; // посторонний символ — пропускаем
                continue;
            }

            let word_pos = self.pos;
            let word = self.read_ident();
            let line = self.line_of(word_pos);
            self.prog.mark_line(line);

            // mono( … ) — группа
            if word == "mono" {
                let save = self.pos;
                self.skip_inline_ws();
                if self.peek() == Some(b'(') {
                    self.pos += 1;
                    if self.depth + 1 >= MAX_DEPTH {
                        self.warn(word_pos, "слишком глубокая вложенность mono(), блок пропущен");
                        self.skip_balanced();
                    } else {
                        if self.depth == 0 {
                            self.stray = 0;
                        }
                        self.prog.emit(op::GROUP_BEGIN);
                        self.depth += 1;
                    }
                    continue;
                }
                self.pos = save;
            }

            // имя фигуры (если сразу за ним нет `(` — иначе это вызов свойства)
            if self.peek() != Some(b'(') {
                if let Some(id) = shape_id(word) {
                    self.prog.emit_u8(op::SHAPE, id);
                    self.read_divisor(word_pos);
                    continue;
                }
            }

            // свойство `имя(значение)`
            self.skip_inline_ws();
            if self.peek() != Some(b'(') {
                self.warn(
                    word_pos,
                    &format!("неизвестное слово `{word}` (ожидалась фигура или свойство)"),
                );
                continue;
            }
            self.pos += 1;
            let args_start = self.pos;
            let multiline = matches!(word, "verts" | "uvs" | "tris" | "data" | "animation");
            let Some(close) = self.find_close(args_start, multiline) else {
                self.warn(word_pos, &format!("не закрыта скобка в `{word}(`"));
                while self.peek().is_some_and(|c| c != b'\n') {
                    self.pos += 1;
                }
                continue;
            };
            let raw = self.slice(args_start, close);
            self.pos = close + 1;
            if self.depth > 0 {
                // вложенные `(` внутри значения дождутся своих `)` позже
                let nested = raw.bytes().filter(|&b| b == b'(').count();
                self.stray = self.stray.saturating_add(nested);
            }
            self.property(word, raw, word_pos);
        }

        // незакрытые mono( … ) закрываются в конце файла (как и раньше:
        // блок «съедал» весь остаток текста)
        for _ in 0..self.depth {
            self.prog.emit(op::GROUP_END);
        }
        self.depth = 0;
        self.prog.emit(op::END);
    }

    /// После имени фигуры: необязательное `: N` (высота уменьшается в N раз).
    fn read_divisor(&mut self, at: usize) {
        self.skip_inline_ws();
        if self.peek() != Some(b':') {
            return;
        }
        self.pos += 1;
        self.skip_inline_ws();
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|c| !c.is_ascii_whitespace() && c != b'(' && c != b')')
        {
            self.pos += 1;
        }
        match self.slice(start, self.pos).parse::<f64>() {
            Ok(n) if n.is_finite() => self.prog.emit_f64(op::DIVISOR, n),
            _ => self.warn(at, "после `:` ожидалось число (делитель высоты)"),
        }
    }

    // ── свойства → инструкции ───────────────────────────────────────────────

    fn num(&mut self, raw: &str, key: &str, at: usize) -> f64 {
        match raw.trim().parse::<f64>() {
            Ok(n) if n.is_finite() => n,
            _ => {
                self.warn(at, &format!("{key}(): `{}` — не число, использовано 0", raw.trim()));
                0.0
            }
        }
    }

    fn numbers(&mut self, raw: &str, key: &str, at: usize) -> Vec<f64> {
        let mut out = Vec::new();
        let mut bad = 0usize;
        for line in raw.lines() {
            // внутри многострочных списков разрешены комментарии `// …`
            let line = line.split("//").next().unwrap_or("");
            for tok in line
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|t| !t.is_empty())
            {
                match tok.parse::<f64>() {
                    Ok(n) if n.is_finite() => out.push(n),
                    _ => bad += 1,
                }
            }
        }
        if bad > 0 {
            self.warn(at, &format!("{key}(): пропущено нечисловых значений: {bad}"));
        }
        out
    }

    fn property(&mut self, key: &str, raw: &str, at: usize) {
        match key {
            "x" => {
                let v = self.num(raw, key, at);
                self.prog.emit_f64(op::X, v);
            }
            "y" => {
                let v = self.num(raw, key, at);
                self.prog.emit_f64(op::Y, v);
            }
            "z" => {
                let v = self.num(raw, key, at);
                self.prog.emit_f64(op::Z, v);
            }
            "scale" => {
                let v = self.num(raw, key, at);
                self.prog.emit_f64(op::SCALE, v);
            }
            "rr" => {
                let v = self.num(raw, key, at);
                self.prog.emit_f64(op::ROT_Y, v);
            }
            "ru" => {
                let v = self.num(raw, key, at);
                self.prog.emit_f64(op::ROT_X, v);
            }
            "ur" | "ul" | "ud" | "uu" | "ug" | "um" => {
                let v = self.num(raw, key, at);
                if let Some(id) = deform_id(key) {
                    self.prog.emit_deform(id, v);
                }
            }
            "color" => match parse_color(raw) {
                Some(c) => self.prog.emit_color(c),
                None => self.warn(at, "color(): ожидается три числа R, G, B"),
            },
            "texture" => {
                let t = raw.trim();
                if t.is_empty() {
                    self.warn(at, "texture(): пустое значение");
                } else {
                    let id = self.prog.intern_string(t);
                    self.prog.emit_u32(op::TEXTURE, id);
                }
            }
            "animation" => {
                self.prog.emit(op::ANIM_BEGIN);
                for seg in strip_line_comments(raw).split(',') {
                    let (mask, vals) = parse_keyframe(seg);
                    self.prog.emit_keyframe(mask, &vals);
                }
                self.prog.emit(op::ANIM_END);
            }
            "data" => match base64::decode(raw.trim()) {
                Some(bytes) => {
                    let id = self.prog.add_blob(bytes);
                    self.prog.emit_u32(op::MESH_DATA, id);
                }
                None => self.warn(at, "data(): некорректный base64"),
            },
            "verts" => {
                let nums = self.numbers(raw, key, at);
                if nums.len() % 3 != 0 {
                    self.warn(at, "verts(): число значений не кратно 3, хвост отброшен");
                }
                let mut blob = Vec::with_capacity(nums.len() / 3 * 12);
                for tri in nums.chunks_exact(3) {
                    for v in tri {
                        blob.extend_from_slice(&(*v as f32).to_le_bytes());
                    }
                }
                let id = self.prog.add_blob(blob);
                self.prog.emit_u32(op::MESH_VERTS, id);
            }
            "uvs" => {
                let nums = self.numbers(raw, key, at);
                if nums.len() % 2 != 0 {
                    self.warn(at, "uvs(): число значений не кратно 2, хвост отброшен");
                }
                let mut blob = Vec::with_capacity(nums.len() / 2 * 8);
                for pair in nums.chunks_exact(2) {
                    for v in pair {
                        blob.extend_from_slice(&(*v as f32).to_le_bytes());
                    }
                }
                let id = self.prog.add_blob(blob);
                self.prog.emit_u32(op::MESH_UVS, id);
            }
            "tris" => {
                let nums = self.numbers(raw, key, at);
                if nums.len() % 3 != 0 {
                    self.warn(at, "tris(): число значений не кратно 3, хвост отброшен");
                }
                let mut blob = Vec::with_capacity(nums.len() / 3 * 12);
                for tri in nums.chunks_exact(3) {
                    for v in tri {
                        blob.extend_from_slice(&index(*v).to_le_bytes());
                    }
                }
                let id = self.prog.add_blob(blob);
                self.prog.emit_u32(op::MESH_TRIS, id);
            }
            _ => self.warn(at, &format!("неизвестное свойство `{key}`")),
        }
    }
}

/// Индекс вершины из числа; всё некорректное превращается в «заведомо
/// недопустимый» индекс, который затем отбросит VM.
fn index(n: f64) -> u32 {
    if n >= 0.0 && n <= f64::from(u32::MAX) {
        n as u32
    } else {
        u32::MAX
    }
}

fn strip_line_comments(raw: &str) -> String {
    raw.lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `color(R, G, B)`: числа через запятую (как раньше) либо через пробел.
fn parse_color(raw: &str) -> Option<[u8; 3]> {
    let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    let parts: Vec<&str> = compact.split(',').collect();
    if parts.len() >= 3 {
        return Some([channel(parts[0]), channel(parts[1]), channel(parts[2])]);
    }
    let spaced: Vec<&str> = raw.split_whitespace().collect();
    if spaced.len() >= 3 {
        return Some([channel(spaced[0]), channel(spaced[1]), channel(spaced[2])]);
    }
    None
}

fn channel(s: &str) -> u8 {
    match s.parse::<f64>() {
        Ok(v) if v.is_finite() => v.round().clamp(0.0, 255.0) as u8,
        _ => 0,
    }
}

/// Один ключевой кадр: пары «ключ+число» (`x10`, `y-50`, `rr90`, `ru15`).
/// Неизвестные ключи и ключи без числа игнорируются. Возвращает маску
/// (`KF_*`) и значения в порядке битов (x, y, z, rr, ru).
fn parse_keyframe(seg: &str) -> (u8, Vec<f64>) {
    let b = seg.as_bytes();
    let mut slots: [Option<f64>; 5] = [None; 5];
    let mut i = 0usize;
    while i < b.len() {
        if !b[i].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let key_start = i;
        while i < b.len() && b[i].is_ascii_alphabetic() {
            i += 1;
        }
        let key = seg.get(key_start..i).unwrap_or("");

        let mut j = i;
        while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
            j += 1;
        }
        let val_start = j;
        if j < b.len() && (b[j] == b'-' || b[j] == b'+') {
            j += 1;
        }
        while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.') {
            j += 1;
        }
        if j == val_start {
            continue; // у ключа нет числа
        }
        let v = match seg.get(val_start..j).unwrap_or("").parse::<f64>() {
            Ok(n) if n.is_finite() => n,
            _ => 0.0,
        };
        i = j;
        match key {
            "x" => slots[0] = Some(v),
            "y" => slots[1] = Some(v),
            "z" => slots[2] = Some(v),
            "rr" => slots[3] = Some(v),
            "ru" => slots[4] = Some(v),
            _ => {}
        }
    }

    let mut mask = 0u8;
    let mut vals = Vec::new();
    for (slot, bit) in slots.iter().zip([KF_X, KF_Y, KF_Z, KF_RR, KF_RU]) {
        if let Some(v) = slot {
            mask |= bit;
            vals.push(*v);
        }
    }
    (mask, vals)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dis(src: &str) -> String {
        parse(src).disassemble()
    }

    fn count(listing: &str, word: &str) -> usize {
        listing.lines().filter(|l| l.starts_with(word)).count()
    }

    #[test]
    fn basic_shape_and_properties() {
        let p = parse("cube3\nx(10) y(-20) z(30)\nscale(2)\ncolor(80, 140, 255)\nrr(90) ru(45)\n");
        assert_eq!(
            p.disassemble(),
            "SHAPE cube3\nX 10\nY -20\nZ 30\nSCALE 2\nCOLOR 80 140 255\nROT_Y 90\nROT_X 45\nEND"
        );
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    }

    #[test]
    fn every_shape_keyword_is_recognised() {
        let listing = dis("cube3\npyramid3\ntriangle3\nsphere3\ncylinder3\nimage3\nmesh3\n");
        assert_eq!(
            listing,
            "SHAPE cube3\nSHAPE pyramid3\nSHAPE triangle3\nSHAPE sphere3\nSHAPE cylinder3\n\
             SHAPE image3\nSHAPE mesh3\nEND"
        );
    }

    #[test]
    fn height_divisor_is_passed_through() {
        let p = parse("cube3 : 4\ncube3 : 0\ncube3:abc\ncube3 : -2\ncube3: 8 // thin\n");
        assert_eq!(
            p.disassemble(),
            "SHAPE cube3\nDIVISOR 4\nSHAPE cube3\nDIVISOR 0\nSHAPE cube3\nSHAPE cube3\nDIVISOR -2\n\
             SHAPE cube3\nDIVISOR 8\nEND"
        );
        assert_eq!(p.warnings.len(), 1); // `abc`
    }

    #[test]
    fn comments_are_skipped_and_hide_properties() {
        assert_eq!(
            dis("// cube3\n   // x(5)\ncube3\n// color(1,2,3)\ny(4) // tail\n"),
            "SHAPE cube3\nY 4\nEND"
        );
    }

    #[test]
    fn shape_name_may_follow_properties_on_the_same_line() {
        assert_eq!(
            dis("cube3\ncolor(1,2,3) sphere3 x(4)\n"),
            "SHAPE cube3\nCOLOR 1 2 3\nSHAPE sphere3\nX 4\nEND"
        );
    }

    #[test]
    fn properties_may_share_the_shape_line() {
        assert_eq!(dis("sphere3 x(5) color(9,8,7)"), "SHAPE sphere3\nX 5\nCOLOR 9 8 7\nEND");
    }

    #[test]
    fn shape_name_with_call_syntax_is_not_a_shape() {
        let p = parse("cube3(5)\ncube3x\n");
        assert_eq!(p.disassemble(), "END");
        assert_eq!(p.warnings.len(), 2);
    }

    #[test]
    fn color_forms() {
        let p = parse("cube3\ncolor(300, -5, 12.6)\nsphere3\ncolor(255 0 128)\npyramid3\ncolor(1,2)");
        assert_eq!(
            p.disassemble(),
            "SHAPE cube3\nCOLOR 255 0 13\nSHAPE sphere3\nCOLOR 255 0 128\nSHAPE pyramid3\nEND"
        );
        assert_eq!(p.warnings.len(), 1);
    }

    #[test]
    fn deform_properties() {
        assert_eq!(
            dis("cube3\nur(1) ul(2) ud(3) uu(4) ug(5) um(6)"),
            "SHAPE cube3\nDEFORM ur 1\nDEFORM ul 2\nDEFORM ud 3\nDEFORM uu 4\nDEFORM ug 5\nDEFORM um 6\nEND"
        );
    }

    #[test]
    fn bad_numbers_become_zero_with_warning() {
        let p = parse("cube3\nx(abc) y(1e999) z(nan)");
        assert_eq!(p.disassemble(), "SHAPE cube3\nX 0\nY 0\nZ 0\nEND");
        assert_eq!(p.warnings.len(), 3);
    }

    #[test]
    fn animation_keyframes() {
        assert_eq!(
            dis("cube3\nanimation(x0 y0 z0, x0 y200 z0, rr90 ru-15, y+5)"),
            "SHAPE cube3\nANIM_BEGIN\nKEYFRAME x=0 y=0 z=0\nKEYFRAME x=0 y=200 z=0\n\
             KEYFRAME rr=90 ru=-15\nKEYFRAME y=5\nANIM_END\nEND"
        );
    }

    #[test]
    fn keyframe_edge_cases() {
        assert_eq!(parse_keyframe(""), (0, vec![]));
        // ключ без числа и неизвестные ключи игнорируются; `z-` даёт 0
        assert_eq!(parse_keyframe("x y5 foo9 z-"), (KF_Y | KF_Z, vec![5.0, 0.0]));
        assert_eq!(parse_keyframe("ru15 rr-30"), (KF_RR | KF_RU, vec![-30.0, 15.0]));
        // пустой кадр между запятыми остаётся кадром
        let listing = dis("cube3\nanimation(y0,,y100)");
        assert_eq!(count(&listing, "KEYFRAME"), 3);
    }

    #[test]
    fn multiline_animation_with_comments() {
        let listing = dis("cube3\nanimation(\n  y0,   // вниз\n  y100 // вверх\n)\n");
        assert_eq!(count(&listing, "KEYFRAME"), 2);
    }

    #[test]
    fn textures_are_interned_and_trimmed() {
        let p = parse("cube3\ntexture( a.png )\nsphere3\ntexture(b.png)\nimage3\ntexture(a.png)");
        assert_eq!(p.strings, vec!["a.png".to_string(), "b.png".to_string()]);
        let refs: Vec<String> = p
            .disassemble()
            .lines()
            .filter(|l| l.starts_with("TEXTURE"))
            .map(|l| l.split(' ').nth(1).unwrap_or("").to_string())
            .collect();
        assert_eq!(refs, vec!["#0", "#1", "#0"]);
    }

    #[test]
    fn texture_urls_with_double_slash_are_not_comments() {
        let p = parse("image3\ntexture(https://example.com/a.png)\n");
        assert_eq!(p.strings, vec!["https://example.com/a.png".to_string()]);
    }

    #[test]
    fn data_block_becomes_a_blob() {
        let raw = vec![1u8, 2, 3, 4, 5, 6, 7];
        let p = parse(&format!("mesh3\ndata({})\n", base64::encode(&raw)));
        assert_eq!(p.blobs, vec![raw]);
        assert_eq!(p.disassemble(), "SHAPE mesh3\nMESH_DATA #0 (7 bytes)\nEND");
    }

    #[test]
    fn data_block_may_be_wrapped_over_lines() {
        let raw: Vec<u8> = (0..60).collect();
        let enc = base64::encode(&raw);
        let (a, b) = enc.split_at(enc.len() / 2);
        let p = parse(&format!("mesh3\ndata(\n{a}\n{b}\n)\n"));
        assert_eq!(p.blobs, vec![raw]);
    }

    #[test]
    fn bad_data_gives_warning_and_no_instruction() {
        let p = parse("mesh3\ndata(!!!)\n");
        assert_eq!(p.disassemble(), "SHAPE mesh3\nEND");
        assert_eq!(p.warnings.len(), 1);
    }

    #[test]
    fn hand_authored_mesh_lists_become_typed_blobs() {
        let src = "mesh3\n\
                   verts(\n  0 -50 0,   // верх\n  50 50 0,\n  -50 50 0\n)\n\
                   uvs(0.5 0, 1 1, 0 1)\n\
                   tris(0 1 2, -1 0 1)\n";
        let p = parse(src);
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        assert_eq!(
            p.disassemble(),
            "SHAPE mesh3\nMESH_VERTS #0 (36 bytes)\nMESH_UVS #1 (24 bytes)\nMESH_TRIS #2 (24 bytes)\nEND"
        );
        let f = |i: usize| f32::from_le_bytes([p.blobs[0][i], p.blobs[0][i + 1], p.blobs[0][i + 2], p.blobs[0][i + 3]]);
        assert_eq!((f(0), f(4), f(8), f(12)), (0.0, -50.0, 0.0, 50.0));
        // отрицательный индекс превращается в заведомо недопустимый
        let tri2_first = u32::from_le_bytes([p.blobs[2][12], p.blobs[2][13], p.blobs[2][14], p.blobs[2][15]]);
        assert_eq!(tri2_first, u32::MAX);
    }

    #[test]
    fn list_tails_are_dropped_with_warning() {
        let p = parse("mesh3\nverts(1 2 3 4)\n");
        assert_eq!(p.disassemble(), "SHAPE mesh3\nMESH_VERTS #0 (12 bytes)\nEND");
        assert_eq!(p.warnings.len(), 1);
    }

    #[test]
    fn mono_block_is_just_markers() {
        assert_eq!(
            dis("mono(\n  x(100) scale(2)\n  cube3\n  x(5)\n)\n"),
            "GROUP_BEGIN\nX 100\nSCALE 2\nSHAPE cube3\nX 5\nGROUP_END\nEND"
        );
    }

    #[test]
    fn mono_on_one_line() {
        assert_eq!(
            dis("mono( x(5) cube3 color(1,2,3) sphere3 )"),
            "GROUP_BEGIN\nX 5\nSHAPE cube3\nCOLOR 1 2 3\nSHAPE sphere3\nGROUP_END\nEND"
        );
    }

    #[test]
    fn mono_blocks_nest() {
        assert_eq!(
            dis("mono(\n x(1)\n mono(\n  cube3\n )\n)\n"),
            "GROUP_BEGIN\nX 1\nGROUP_BEGIN\nSHAPE cube3\nGROUP_END\nGROUP_END\nEND"
        );
    }

    #[test]
    fn unclosed_mono_is_closed_at_eof_and_stray_paren_is_ignored() {
        assert_eq!(dis("mono(\ncube3\n"), "GROUP_BEGIN\nSHAPE cube3\nGROUP_END\nEND");
        assert_eq!(dis("cube3\n)\n)\n"), "SHAPE cube3\nEND");
    }

    #[test]
    fn parenthesised_remarks_inside_a_group_do_not_close_it() {
        // пример из README 3.x вместе со стрелкой-пояснением
        let src = "mono(\n  x(0) y(0) scale(1)   ← группа-трансформ (необязательно)\n\n  cube3\n  color(150, 150, 150)\n  y(200)\n\n  sphere3\n  color(200, 100, 50)\n)\n";
        assert_eq!(
            dis(src),
            "GROUP_BEGIN\nX 0\nY 0\nSCALE 1\nSHAPE cube3\nCOLOR 150 150 150\nY 200\n\
             SHAPE sphere3\nCOLOR 200 100 50\nGROUP_END\nEND"
        );
        // вложенные скобки в значении тоже учитываются по глубине
        let nested = dis("mono(\n texture(foo(1).png)\n cube3\n)\ncube3");
        assert!(nested.ends_with("GROUP_END\nSHAPE cube3\nEND"), "{nested}");
        // а на верхнем уровне посторонние скобки группам не мешают
        assert_eq!(dis("( ( cube3\nmono(\n sphere3\n)\n"), "SHAPE cube3\nGROUP_BEGIN\nSHAPE sphere3\nGROUP_END\nEND");
    }

    #[test]
    fn mono_keyword_needs_parens() {
        let p = parse("monolith(5)\nmono\ncube3");
        assert_eq!(p.disassemble(), "SHAPE cube3\nEND");
        assert_eq!(p.warnings.len(), 2);
    }

    #[test]
    fn crlf_and_bom() {
        let p = parse("\u{feff}cube3\r\nx(5)\r\ncolor(1,2,3)\r\n");
        assert_eq!(p.disassemble(), "SHAPE cube3\nX 5\nCOLOR 1 2 3\nEND");
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    }

    #[test]
    fn unclosed_paren_does_not_swallow_following_lines() {
        let p = parse("cube3\ncolor(1,2,3\ny(5)\n");
        assert_eq!(p.disassemble(), "SHAPE cube3\nY 5\nEND");
        assert_eq!(p.warnings.len(), 1);
    }

    #[test]
    fn non_ascii_text_is_safe() {
        let p = parse("привет\ncube3 ✓ x(5)\n// комментарий\nsphere3 color(1,2,3) 日本語\nimage3\ntexture(картинка.png)\n");
        assert_eq!(
            p.disassemble(),
            "SHAPE cube3\nX 5\nSHAPE sphere3\nCOLOR 1 2 3\nSHAPE image3\nTEXTURE #0 \"картинка.png\"\nEND"
        );
    }

    #[test]
    fn empty_and_whitespace_input() {
        for src in ["", "   ", "\n\n\n", "// только комментарий"] {
            let p = parse(src);
            assert_eq!(p.disassemble(), "END");
            assert!(p.warnings.is_empty());
        }
    }

    #[test]
    fn line_numbers_reach_the_line_table() {
        let p = parse("cube3\n\nx(5)\n");
        assert_eq!(p.line_at(0), Some(1));
        assert_eq!(p.line_at(2), Some(3));
        let w = parse("cube3\n\nbogus(1)\n").warnings;
        assert!(w[0].starts_with("строка 3:"), "{w:?}");
    }

    #[test]
    fn deep_nesting_does_not_overflow_and_stays_balanced() {
        let src = "mono(".repeat(5000) + "cube3" + &")".repeat(5000);
        let p = parse(&src);
        assert!(!p.warnings.is_empty());
        let listing = p.disassemble();
        assert_eq!(count(&listing, "GROUP_BEGIN"), count(&listing, "GROUP_END"));
        assert!(count(&listing, "GROUP_BEGIN") < MAX_DEPTH);
    }

    #[test]
    fn warnings_are_capped() {
        let src = "cube3\n".to_string() + &"bogus(1)\n".repeat(1000);
        assert!(parse(&src).warnings.len() <= MAX_WARNINGS + 1);
    }

    #[test]
    fn random_garbage_never_panics_and_stays_balanced() {
        const TOKENS: [&str; 30] = [
            "cube3", "sphere3", "mesh3", "image3", "mono(", ")", "(", "x(", "y(1)", "color(",
            "1,2,3", "verts(", "tris(0 1 2)", "data(", "AAAA", "\n", " ", ":", "4", "//",
            "animation(", "x0 y1,", "-", ".", "é", "texture(", "a.png", "scale(2)", "\r\n", "uvs(",
        ];
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..400 {
            let mut src = String::new();
            let n = (next() % 60) as usize;
            for _ in 0..n {
                src.push_str(TOKENS[(next() % TOKENS.len() as u64) as usize]);
            }
            let listing = parse(&src).disassemble();
            assert!(listing.ends_with("END"), "{src:?}\n{listing}");
            assert_eq!(count(&listing, "GROUP_BEGIN"), count(&listing, "GROUP_END"), "{src:?}");
        }
    }
}
