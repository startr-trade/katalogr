//! YAML through the `saphyr-parser` event stream. Anchors are recorded by the parser but never
//! resolved: an alias becomes [`Value::Alias`] holding its anchor name, so input that expands
//! exponentially when aliases are followed stays linear here.

use super::{Entry, MAX_DEPTH, Node, Parsed, Scalar, Value, parse_error};
use crate::lines::Lines;
use katalogr_model::Span;
use saphyr_parser::{Event, Marker, Parser, ScalarStyle};

pub fn parse_yaml(text: &str) -> Parsed {
    let mut b = B {
        text,
        lines: Lines::new(text),
        byte_of: if text.is_ascii() {
            None
        } else {
            Some(
                text.char_indices()
                    .map(|(i, _)| u32::try_from(i).unwrap_or(u32::MAX))
                    .collect(),
            )
        },
        stack: Vec::new(),
        root: None,
        out: Parsed::default(),
    };
    let mut parser = Parser::new_from_str(text);
    for ev in &mut parser {
        let (ev, span) = match ev {
            Ok(x) => x,
            Err(e) => {
                let at = b.byte(e.marker());
                b.out
                    .errors
                    .push(parse_error(Some(b.lines.span(at, at + 1)), e.info()));
                b.unwind(at);
                break;
            }
        };
        let (s, e) = (b.byte(&span.start), b.byte(&span.end));
        let is_map = matches!(ev, Event::MappingStart(..));
        match ev {
            Event::DocumentEnd => b.end_document(e),
            Event::DocumentStart(_) | Event::StreamStart | Event::StreamEnd | Event::Nothing => {}
            Event::Scalar(value, style, _, tag) => {
                let scalar = if style == ScalarStyle::Plain && tag.is_none() {
                    core_scalar(&value)
                } else {
                    Scalar::Str(value.into_owned())
                };
                let node = Node {
                    span: b.lines.span(s, e),
                    value: Value::Scalar(scalar),
                };
                b.push(node);
            }
            Event::Alias(_) => {
                let name = b.text[s..e].trim_start_matches('*').to_string();
                let node = Node {
                    span: b.lines.span(s, e),
                    value: Value::Alias(name),
                };
                b.push(node);
            }
            Event::MappingStart(..) | Event::SequenceStart(..) => {
                if b.stack.len() >= MAX_DEPTH {
                    b.out.errors.push(parse_error(
                        Some(b.lines.span(s, s + 1)),
                        format!("nesting deeper than {MAX_DEPTH}"),
                    ));
                    b.unwind(s);
                    break;
                }
                b.stack.push(if is_map {
                    Frame::Map {
                        entries: Vec::new(),
                        key: None,
                        start: s,
                    }
                } else {
                    Frame::Seq {
                        items: Vec::new(),
                        start: s,
                    }
                });
            }
            Event::MappingEnd | Event::SequenceEnd => b.close(e),
        }
    }
    b.end_document(text.len());
    b.out
}

enum Frame {
    Map {
        entries: Vec<Entry>,
        /// The key read and waiting for its value.
        key: Option<(String, Span)>,
        start: usize,
    },
    Seq {
        items: Vec<Node>,
        start: usize,
    },
}

struct B<'a> {
    text: &'a str,
    lines: Lines<'a>,
    /// Byte offset of each character, when the text is not ASCII.
    byte_of: Option<Vec<u32>>,
    stack: Vec<Frame>,
    root: Option<Node>,
    out: Parsed,
}

impl B<'_> {
    /// Byte offset of a marker (the parser counts characters).
    fn byte(&self, m: &Marker) -> usize {
        match &self.byte_of {
            None => m.index().min(self.text.len()),
            Some(v) => v.get(m.index()).map_or(self.text.len(), |&b| b as usize),
        }
    }

    fn push(&mut self, node: Node) {
        match self.stack.last_mut() {
            None => self.root = Some(node),
            Some(Frame::Seq { items, .. }) => items.push(node),
            Some(Frame::Map { entries, key, .. }) => match key.take() {
                Some((key, key_span)) => entries.push(Entry {
                    key,
                    key_span,
                    value: node,
                }),
                None => {
                    let text = match &node.value {
                        Value::Alias(a) => format!("*{a}"),
                        _ => node.as_text().unwrap_or("?").to_string(),
                    };
                    *key = Some((text, node.span));
                }
            },
        }
    }

    /// Close the innermost container, which ends before byte `end`.
    fn close(&mut self, end: usize) {
        let Some(frame) = self.stack.pop() else {
            return;
        };
        let (start, value) = match frame {
            Frame::Map {
                mut entries,
                key,
                start,
            } => {
                if let Some((k, key_span)) = key {
                    // A key without a value (cut off by an error) is kept as null.
                    entries.push(Entry {
                        key: k,
                        key_span,
                        value: Node {
                            span: key_span,
                            value: Value::Scalar(Scalar::Null),
                        },
                    });
                }
                (start, Value::Map(entries))
            }
            Frame::Seq { items, start } => (start, Value::Seq(items)),
        };
        let node = Node {
            span: self.lines.span(start, end),
            value,
        };
        self.push(node);
    }

    /// Close every open container at byte `at`, as after an error.
    fn unwind(&mut self, at: usize) {
        while !self.stack.is_empty() {
            self.close(at);
        }
        self.end_document(at);
    }

    fn end_document(&mut self, _at: usize) {
        if let Some(root) = self.root.take() {
            self.out.docs.push(root);
        }
    }
}

/// A plain scalar read by the YAML 1.2 core schema.
fn core_scalar(s: &str) -> Scalar {
    match s {
        "" | "~" | "null" | "Null" | "NULL" => return Scalar::Null,
        "true" | "True" | "TRUE" => return Scalar::Bool(true),
        "false" | "False" | "FALSE" => return Scalar::Bool(false),
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" | "-.inf" | "-.Inf" | "-.INF"
        | ".nan" | ".NaN" | ".NAN" => return Scalar::Num(s.to_string()),
        _ => {}
    }
    let unsigned = s.strip_prefix(['+', '-']).unwrap_or(s);
    let all = |t: &str, f: fn(&u8) -> bool| !t.is_empty() && t.bytes().all(|c| f(&c));
    if all(unsigned, u8::is_ascii_digit) {
        return Scalar::Num(s.to_string());
    }
    if let Some(o) = s.strip_prefix("0o")
        && all(o, |c| (b'0'..=b'7').contains(c))
    {
        return Scalar::Num(s.to_string());
    }
    if let Some(h) = s.strip_prefix("0x")
        && all(h, u8::is_ascii_hexdigit)
    {
        return Scalar::Num(s.to_string());
    }
    // [-+]? ( \.[0-9]+ | [0-9]+ (\.[0-9]*)? ) ([eE][-+]?[0-9]+)?
    let (mant, exp) = match unsigned.find(['e', 'E']) {
        Some(i) => (&unsigned[..i], Some(&unsigned[i + 1..])),
        None => (unsigned, None),
    };
    let mant_ok = match mant.split_once('.') {
        Some((i, f)) => {
            (all(i, u8::is_ascii_digit) && f.bytes().all(|c| c.is_ascii_digit()))
                || (i.is_empty() && all(f, u8::is_ascii_digit))
        }
        None => all(mant, u8::is_ascii_digit),
    };
    let exp_ok =
        exp.is_none_or(|e| all(e.strip_prefix(['+', '-']).unwrap_or(e), u8::is_ascii_digit));
    if mant_ok && exp_ok {
        Scalar::Num(s.to_string())
    } else {
        Scalar::Str(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_core_schema_scalars() {
        let p = parse_yaml(
            "a: 12\nb: -3.5e2\nc: true\nd: ~\ne: '12'\nf: 0x1F\ng: 1.2.3\nh: yes\ni:\nj: .inf\nk: é\n",
        );
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        let r = &p.docs[0];
        let kind = |k: &str| match &r.get(k).unwrap().value {
            Value::Scalar(Scalar::Num(_)) => "num",
            Value::Scalar(Scalar::Bool(_)) => "bool",
            Value::Scalar(Scalar::Null) => "null",
            Value::Scalar(Scalar::Str(_)) => "str",
            _ => "other",
        };
        let got: Vec<_> = "abcdefghijk"
            .chars()
            .map(|c| kind(&c.to_string()))
            .collect();
        assert_eq!(
            got,
            [
                "num", "num", "bool", "null", "str", "num", "str", "str", "null", "num", "str"
            ]
        );
        assert_eq!(r.get("e").unwrap().as_text(), Some("12"));
        // Spans count characters, not bytes.
        let k = &r.entries()[10];
        assert_eq!((k.key_span.start_line, k.key_span.start_col), (11, 1));
        assert_eq!(k.value.span.start_col, 4);
    }

    #[test]
    fn yaml_multi_doc() {
        let p = parse_yaml("a: &r 1\n---\n- x\n- y: *r\n---\nz: 2\n");
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert_eq!(p.docs.len(), 3);
        assert_eq!(p.docs[1].items().len(), 2);
        let y = p.docs[1].items()[1].get("y").unwrap();
        assert_eq!(y.value, Value::Alias("r".into()));
        assert_eq!(p.docs[2].get("z").unwrap().as_text(), Some("2"));
        // An error keeps the documents before it and the partial tree.
        let p = parse_yaml("a: 1\n---\nb: [1, 2\nc: x\n");
        assert_eq!(p.errors.len(), 1);
        assert_eq!(p.docs.len(), 2);
        assert!(p.docs[1].get("b").is_some());
        // Depth bound.
        let deep = format!("{}1{}", "[".repeat(65), "]".repeat(65));
        let p = parse_yaml(&deep);
        assert!(
            p.errors
                .iter()
                .any(|e| e.to_string().contains("deeper than 64"))
        );
    }
}
