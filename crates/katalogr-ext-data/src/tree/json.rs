//! A recursive-descent RFC 8259 parser over bytes. Strings are unescaped, numbers are kept as
//! written, a leading BOM is skipped. At the first syntax error it records the error and
//! returns the tree built so far: closed containers are kept and the open one is closed there.

use super::{Entry, MAX_DEPTH, Node, Parsed, Scalar, Value, parse_error};
use crate::lines::Lines;
use katalogr_model::ExtractError;

pub fn parse_json(text: &str) -> Parsed {
    let mut p = P {
        text,
        b: text.as_bytes(),
        i: if text.starts_with('\u{feff}') { 3 } else { 0 },
        lines: Lines::new(text),
        errors: Vec::new(),
        stop: false,
    };
    p.ws();
    let mut docs = Vec::new();
    if p.i >= p.b.len() {
        p.fail("empty document");
    } else {
        if let Some(n) = p.value(0) {
            docs.push(n);
        }
        if !p.stop {
            p.ws();
            if p.i < p.b.len() {
                p.fail("unexpected characters after the document");
            }
        }
    }
    Parsed {
        docs,
        errors: p.errors,
    }
}

struct P<'a> {
    text: &'a str,
    b: &'a [u8],
    i: usize,
    lines: Lines<'a>,
    errors: Vec<ExtractError>,
    stop: bool,
}

impl P<'_> {
    fn fail(&mut self, message: &str) {
        if !self.stop {
            let span = self.lines.span(self.i, self.i + 1);
            self.errors.push(parse_error(Some(span), message));
        }
        self.stop = true;
    }

    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Option<Node> {
        let start = self.i;
        let value = match self.b.get(self.i).copied() {
            None => {
                self.fail("unexpected end of input");
                return None;
            }
            Some(b'{') => return self.object(depth),
            Some(b'[') => return self.array(depth),
            Some(b'"') => Value::Scalar(Scalar::Str(self.string()?)),
            Some(b't') if self.word("true") => Value::Scalar(Scalar::Bool(true)),
            Some(b'f') if self.word("false") => Value::Scalar(Scalar::Bool(false)),
            Some(b'n') if self.word("null") => Value::Scalar(Scalar::Null),
            Some(b'-' | b'0'..=b'9') => Value::Scalar(Scalar::Num(self.number()?)),
            Some(_) => {
                self.fail("expected a value");
                return None;
            }
        };
        Some(Node {
            span: self.lines.span(start, self.i),
            value,
        })
    }

    fn word(&mut self, w: &str) -> bool {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            true
        } else {
            self.fail("expected a value");
            false
        }
    }

    fn number(&mut self) -> Option<String> {
        let start = self.i;
        let digits = |p: &mut Self| {
            let s = p.i;
            while p.b.get(p.i).is_some_and(u8::is_ascii_digit) {
                p.i += 1;
            }
            p.i > s
        };
        if self.b[self.i] == b'-' {
            self.i += 1;
        }
        match self.b.get(self.i) {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                digits(self);
            }
            _ => {
                self.fail("a number needs digits");
                return None;
            }
        }
        if self.b.get(self.i) == Some(&b'.') {
            self.i += 1;
            if !digits(self) {
                self.fail("a fraction needs digits");
                return None;
            }
        }
        if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !digits(self) {
                self.fail("an exponent needs digits");
                return None;
            }
        }
        Some(self.text[start..self.i].to_string())
    }

    fn hex4(&mut self) -> Option<u32> {
        let h = self.text.get(self.i..self.i + 4)?;
        let v = u32::from_str_radix(h, 16).ok()?;
        if !h.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        self.i += 4;
        Some(v)
    }

    fn string(&mut self) -> Option<String> {
        self.i += 1; // opening quote
        let mut out = String::new();
        let mut run = self.i;
        loop {
            let Some(&c) = self.b.get(self.i) else {
                self.fail("unterminated string");
                return None;
            };
            match c {
                b'"' => {
                    out.push_str(&self.text[run..self.i]);
                    self.i += 1;
                    return Some(out);
                }
                b'\\' => {
                    out.push_str(&self.text[run..self.i]);
                    self.i += 1;
                    let Some(&e) = self.b.get(self.i) else {
                        self.fail("unterminated string");
                        return None;
                    };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let Some(hi) = self.hex4() else {
                                self.fail("a \\u escape needs four hex digits");
                                return None;
                            };
                            let ch = if (0xD800..0xDC00).contains(&hi)
                                && self.b[self.i..].starts_with(b"\\u")
                            {
                                let save = self.i;
                                self.i += 2;
                                match self.hex4() {
                                    Some(lo) if (0xDC00..0xE000).contains(&lo) => char::from_u32(
                                        0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00),
                                    ),
                                    _ => {
                                        self.i = save;
                                        None
                                    }
                                }
                            } else {
                                char::from_u32(hi)
                            };
                            out.push(ch.unwrap_or('\u{fffd}'));
                        }
                        _ => {
                            self.i -= 1;
                            self.fail("unknown escape");
                            return None;
                        }
                    }
                    run = self.i;
                }
                0..=0x1f => {
                    self.fail("control character in string");
                    return None;
                }
                _ => self.i += 1,
            }
        }
    }

    fn object(&mut self, depth: usize) -> Option<Node> {
        if depth >= MAX_DEPTH {
            self.fail(&format!("nesting deeper than {MAX_DEPTH}"));
            return None;
        }
        let start = self.i;
        self.i += 1;
        let mut entries = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b'}') {
            self.i += 1;
        } else {
            loop {
                self.ws();
                if self.b.get(self.i) != Some(&b'"') {
                    self.fail("expected a string key");
                    break;
                }
                let ks = self.i;
                let Some(key) = self.string() else { break };
                let key_span = self.lines.span(ks, self.i);
                self.ws();
                if self.b.get(self.i) != Some(&b':') {
                    self.fail("expected ':'");
                    break;
                }
                self.i += 1;
                self.ws();
                match self.value(depth + 1) {
                    Some(value) => entries.push(Entry {
                        key,
                        key_span,
                        value,
                    }),
                    None => break,
                }
                if self.stop {
                    break;
                }
                self.ws();
                match self.b.get(self.i) {
                    Some(b',') => self.i += 1,
                    Some(b'}') => {
                        self.i += 1;
                        break;
                    }
                    _ => {
                        self.fail("expected ',' or '}'");
                        break;
                    }
                }
            }
        }
        Some(Node {
            span: self.lines.span(start, self.i),
            value: Value::Map(entries),
        })
    }

    fn array(&mut self, depth: usize) -> Option<Node> {
        if depth >= MAX_DEPTH {
            self.fail(&format!("nesting deeper than {MAX_DEPTH}"));
            return None;
        }
        let start = self.i;
        self.i += 1;
        let mut items = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b']') {
            self.i += 1;
        } else {
            loop {
                self.ws();
                match self.value(depth + 1) {
                    Some(n) => items.push(n),
                    None => break,
                }
                if self.stop {
                    break;
                }
                self.ws();
                match self.b.get(self.i) {
                    Some(b',') => self.i += 1,
                    Some(b']') => {
                        self.i += 1;
                        break;
                    }
                    _ => {
                        self.fail("expected ',' or ']'");
                        break;
                    }
                }
            }
        }
        Some(Node {
            span: self.lines.span(start, self.i),
            value: Value::Seq(items),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_spans_and_unescape() {
        let t = "{\"a\": \"x\\ny\\u00e9\\ud83d\\ude00\", \"b\": [1, -2.5e3, true, null]}";
        let p = parse_json(t);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        let root = &p.docs[0];
        assert_eq!(root.get("a").unwrap().as_text(), Some("x\nyé😀"));
        let b = root.get("b").unwrap();
        assert_eq!(b.items()[1].as_text(), Some("-2.5e3"));
        assert_eq!(b.items()[2].as_text(), Some("true"));
        assert_eq!(b.items()[3].as_text(), Some("null"));
        let e = &root.entries()[1];
        assert_eq!((e.key_span.start_col, e.key_span.end_col), (33, 35));
        assert_eq!(e.value.span.start_col, 38);
        // Multi-line spans and a BOM.
        let p = parse_json("\u{feff}{\n  \"k\": 1\n}");
        assert!(p.errors.is_empty());
        let s = p.docs[0].span;
        assert_eq!(
            (s.start_line, s.start_col, s.end_line, s.end_col),
            (1, 2, 3, 1)
        );
    }

    #[test]
    fn json_error_keeps_partial() {
        let p = parse_json("{\"a\": {\"b\": 1}, \"c\": [1, 2, oops], \"d\": 4}");
        assert_eq!(p.errors.len(), 1);
        let ExtractError::Parse { span, .. } = &p.errors[0] else {
            panic!()
        };
        assert_eq!(span.unwrap().start_col, 29);
        let root = &p.docs[0];
        assert_eq!(
            root.get("a").unwrap().get("b").unwrap().as_text(),
            Some("1")
        );
        assert_eq!(root.get("c").unwrap().items().len(), 2, "open array closed");
        assert!(root.get("d").is_none());
        // Depth bound.
        let deep = format!("{}1{}", "[".repeat(65), "]".repeat(65));
        let p = parse_json(&deep);
        assert!(
            p.errors
                .iter()
                .any(|e| e.to_string().contains("deeper than 64"))
        );
        let ok = format!("{}1{}", "[".repeat(64), "]".repeat(64));
        assert!(parse_json(&ok).errors.is_empty());
        // Empty and trailing text.
        assert!(!parse_json("").errors.is_empty());
        assert!(!parse_json("1 2").errors.is_empty());
    }
}
