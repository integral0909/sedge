//! A small JSON value type with a parser and serializer, enough for the
//! language server protocol.

use std::fmt::Write;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> &Json {
        match self {
            Json::Obj(fields) => fields
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v)
                .unwrap_or(&Json::Null),
            _ => &Json::Null,
        }
    }

    /// Follows a path of object keys.
    pub fn at(&self, path: &[&str]) -> &Json {
        path.iter().fold(self, |v, k| v.get(k))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Json::Num(n) if *n >= 0.0 => Some(*n as u32),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    pub fn obj(fields: Vec<(&str, Json)>) -> Json {
        Json::Obj(
            fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    pub fn str(s: impl Into<String>) -> Json {
        Json::Str(s.into())
    }

    pub fn num(n: impl Into<f64>) -> Json {
        Json::Num(n.into())
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(n) => {
                if n.fract() == 0.0 && n.abs() < 1e15 {
                    let _ = write!(out, "{}", *n as i64);
                } else {
                    let _ = write!(out, "{n}");
                }
            }
            Json::Str(s) => write_str(out, s),
            Json::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Json::Obj(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_str(out, k);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    pub fn parse(text: &str) -> Result<Json, String> {
        let mut p = Parser {
            s: text.as_bytes(),
            pos: 0,
        };
        let v = p.value()?;
        p.ws();
        if p.pos != p.s.len() {
            return Err(format!("trailing characters at {}", p.pos));
        }
        Ok(v)
    }
}

impl std::fmt::Display for Json {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = String::new();
        self.write(&mut out);
        f.write_str(&out)
    }
}

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.pos < self.s.len() && matches!(self.s[self.pos], b' ' | b'\n' | b'\r' | b'\t') {
            self.pos += 1;
        }
    }

    fn peek(&self) -> u8 {
        self.s.get(self.pos).copied().unwrap_or(0)
    }

    fn lit(&mut self, word: &str, v: Json) -> Result<Json, String> {
        if self.s[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(v)
        } else {
            Err(format!("unexpected input at {}", self.pos))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        match self.peek() {
            b'n' => self.lit("null", Json::Null),
            b't' => self.lit("true", Json::Bool(true)),
            b'f' => self.lit("false", Json::Bool(false)),
            b'"' => Ok(Json::Str(self.string()?)),
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                self.ws();
                if self.peek() == b']' {
                    self.pos += 1;
                    return Ok(Json::Arr(items));
                }
                loop {
                    items.push(self.value()?);
                    self.ws();
                    match self.peek() {
                        b',' => self.pos += 1,
                        b']' => {
                            self.pos += 1;
                            return Ok(Json::Arr(items));
                        }
                        _ => return Err(format!("expected `,` or `]` at {}", self.pos)),
                    }
                }
            }
            b'{' => {
                self.pos += 1;
                let mut fields = Vec::new();
                self.ws();
                if self.peek() == b'}' {
                    self.pos += 1;
                    return Ok(Json::Obj(fields));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.ws();
                    if self.peek() != b':' {
                        return Err(format!("expected `:` at {}", self.pos));
                    }
                    self.pos += 1;
                    let v = self.value()?;
                    fields.push((k, v));
                    self.ws();
                    match self.peek() {
                        b',' => self.pos += 1,
                        b'}' => {
                            self.pos += 1;
                            return Ok(Json::Obj(fields));
                        }
                        _ => return Err(format!("expected `,` or `}}` at {}", self.pos)),
                    }
                }
            }
            c if c == b'-' || c.is_ascii_digit() => {
                let start = self.pos;
                while self.pos < self.s.len()
                    && matches!(
                        self.s[self.pos],
                        b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'
                    )
                {
                    self.pos += 1;
                }
                let text = std::str::from_utf8(&self.s[start..self.pos]).unwrap();
                text.parse::<f64>()
                    .map(Json::Num)
                    .map_err(|_| format!("bad number at {start}"))
            }
            _ => Err(format!("unexpected input at {}", self.pos)),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        if self.peek() != b'"' {
            return Err(format!("expected a string at {}", self.pos));
        }
        self.pos += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = self.peek();
            if self.pos >= self.s.len() {
                return Err("unterminated string".into());
            }
            self.pos += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = self.peek();
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'/' => out.push(b'/'),
                        b'\\' => out.push(b'\\'),
                        b'"' => out.push(b'"'),
                        b'u' => {
                            let mut code = self.hex4()?;
                            if (0xD800..0xDC00).contains(&code)
                                && self.s[self.pos..].starts_with(b"\\u")
                            {
                                self.pos += 2;
                                let low = self.hex4()?;
                                code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            }
                            let ch = char::from_u32(code).unwrap_or('\u{FFFD}');
                            let mut buf = [0; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return Err(format!("bad escape at {}", self.pos)),
                    }
                }
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| "invalid UTF-8".into())
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let end = self.pos + 4;
        let text = self
            .s
            .get(self.pos..end)
            .and_then(|b| std::str::from_utf8(b).ok())
            .ok_or("bad unicode escape")?;
        self.pos = end;
        u32::from_str_radix(text, 16).map_err(|_| "bad unicode escape".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let text = r#"{"a":[1,2.5,-3,true,false,null],"b":{"c":"x\"y\né😀"}}"#;
        let v = Json::parse(text).unwrap();
        assert_eq!(v.at(&["b", "c"]).as_str(), Some("x\"y\né😀"));
        let again = Json::parse(&v.to_string()).unwrap();
        assert_eq!(v, again);
    }
}
