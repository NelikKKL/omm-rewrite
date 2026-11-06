//! Минимальный JSON-парсер (достаточно для чтения glTF).
//!
//! В отличие от наивной рекурсии, глубина вложенности ограничена
//! ([`MAX_DEPTH`]) — злонамеренный файл с тысячами `[` подряд не сможет
//! переполнить стек. Паники исключены: любой изъян даёт `None`.

/// Максимальная глубина вложенности массивов/объектов.
pub const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    /// Пары «ключ → значение» в порядке появления.
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// Значение по ключу (при дубликатах побеждает последнее).
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(items) => items
                .iter()
                .rev()
                .find(|item| item.0.as_str() == key)
                .map(|item| &item.1),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a.as_slice()),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// Неотрицательное целое (для индексов, размеров и смещений).
    /// Отрицательные, дробные-мусорные и огромные значения → `None`.
    pub fn as_usize(&self) -> Option<usize> {
        match self {
            Json::Num(n) if n.is_finite() && *n >= 0.0 && *n <= 9.0e15 => Some(*n as usize),
            _ => None,
        }
    }
}

/// Разбирает JSON-документ. Хвост после первого значения игнорируется.
pub fn parse(src: &str) -> Option<Json> {
    let mut p = Parser { b: src.as_bytes(), i: 0 };
    p.value(0)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn lit(&mut self, word: &[u8]) -> bool {
        let hit = self.b.get(self.i..).is_some_and(|rest| rest.starts_with(word));
        if hit {
            self.i += word.len();
        }
        hit
    }

    fn value(&mut self, depth: usize) -> Option<Json> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.ws();
        match self.peek()? {
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'"' => self.string().map(Json::Str),
            b't' => self.lit(b"true").then_some(Json::Bool(true)),
            b'f' => self.lit(b"false").then_some(Json::Bool(false)),
            b'n' => self.lit(b"null").then_some(Json::Null),
            _ => self.number(),
        }
    }

    fn object(&mut self, depth: usize) -> Option<Json> {
        self.i += 1; // '{'
        let mut items: Vec<(String, Json)> = Vec::new();
        self.ws();
        if self.peek()? == b'}' {
            self.i += 1;
            return Some(Json::Obj(items));
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            if self.peek()? != b':' {
                return None;
            }
            self.i += 1;
            let val = self.value(depth + 1)?;
            items.push((key, val));
            self.ws();
            match self.peek()? {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    return Some(Json::Obj(items));
                }
                _ => return None,
            }
        }
    }

    fn array(&mut self, depth: usize) -> Option<Json> {
        self.i += 1; // '['
        let mut items: Vec<Json> = Vec::new();
        self.ws();
        if self.peek()? == b']' {
            self.i += 1;
            return Some(Json::Arr(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            self.ws();
            match self.peek()? {
                b',' => self.i += 1,
                b']' => {
                    self.i += 1;
                    return Some(Json::Arr(items));
                }
                _ => return None,
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let end = self.i.checked_add(4)?;
        let digits = self.b.get(self.i..end)?;
        let mut v = 0u32;
        for &c in digits {
            v = v * 16 + char::from(c).to_digit(16)?;
        }
        self.i = end;
        Some(v)
    }

    fn unicode_escape(&mut self) -> Option<char> {
        let hi = self.hex4()?;
        if (0xD800..0xDC00).contains(&hi) {
            // суррогатная пара: \uD83D\uDE00
            if self.b.get(self.i) == Some(&b'\\') && self.b.get(self.i + 1) == Some(&b'u') {
                let save = self.i;
                self.i += 2;
                if let Some(lo) = self.hex4() {
                    if (0xDC00..0xE000).contains(&lo) {
                        let cp = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                        return Some(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                    }
                }
                self.i = save;
            }
            return Some('\u{FFFD}');
        }
        Some(char::from_u32(hi).unwrap_or('\u{FFFD}'))
    }

    fn string(&mut self) -> Option<String> {
        if self.peek()? != b'"' {
            return None;
        }
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.b.get(self.i)?;
                    self.i += 1;
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let ch = self.unicode_escape()?;
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return None,
                    }
                }
                _ => out.push(c),
            }
        }
        String::from_utf8(out).ok()
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.i;
        while matches!(self.peek(), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
            self.i += 1;
        }
        if self.i == start {
            return None;
        }
        let text = std::str::from_utf8(self.b.get(start..self.i)?).ok()?;
        let n: f64 = text.parse().ok()?;
        if n.is_finite() {
            Some(Json::Num(n))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_document() {
        let j = parse(r#" {"a": [1, 2.5, -3e2], "b": {"c": "x\ny\u0041"}, "t": true, "n": null} "#)
            .expect("valid json");
        let a = j.get("a").and_then(Json::as_arr).unwrap();
        assert_eq!(a.len(), 3);
        assert_eq!(a[1].as_f64(), Some(2.5));
        assert_eq!(a[2].as_f64(), Some(-300.0));
        assert_eq!(j.get("b").and_then(|b| b.get("c")).and_then(Json::as_str), Some("x\nyA"));
        assert_eq!(j.get("t"), Some(&Json::Bool(true)));
        assert_eq!(j.get("n"), Some(&Json::Null));
        assert!(j.get("missing").is_none());
    }

    #[test]
    fn surrogate_pairs_and_lone_surrogates() {
        let j = parse(r#""\uD83D\uDE00|\uD800|""#).unwrap();
        assert_eq!(j.as_str(), Some("😀|\u{FFFD}|"));
    }

    #[test]
    fn deep_nesting_is_rejected_not_overflowed() {
        let deep = "[".repeat(10_000) + &"]".repeat(10_000);
        assert!(parse(&deep).is_none());
        let ok = "[".repeat(MAX_DEPTH) + &"]".repeat(MAX_DEPTH);
        assert!(parse(&ok).is_some());
    }

    #[test]
    fn garbage_gives_none() {
        for bad in ["", "{", "[1,", "{\"a\"}", "{\"a\":}", "\"abc", "tru", "--1", "[1 2]", "1e999"] {
            assert!(parse(bad).is_none(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn as_usize_rejects_negative_and_huge() {
        assert_eq!(Json::Num(3.0).as_usize(), Some(3));
        assert_eq!(Json::Num(-1.0).as_usize(), None);
        assert_eq!(Json::Num(1e300).as_usize(), None);
        assert_eq!(Json::Str("3".into()).as_usize(), None);
    }

    #[test]
    fn duplicate_keys_last_wins() {
        let j = parse(r#"{"k":1,"k":2}"#).unwrap();
        assert_eq!(j.get("k").and_then(Json::as_f64), Some(2.0));
    }
}
