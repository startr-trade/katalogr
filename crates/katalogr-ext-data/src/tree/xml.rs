//! XML through the `quick-xml` event reader. An element becomes a map: its attributes first as
//! `@<qualified name>` entries (namespace declarations included), then its child elements by
//! qualified name, in document order. DOCTYPE, processing instructions, comments and the
//! declaration are skipped, and entity references other than the five predefined ones and
//! character references stay literal: nothing is fetched, defined or expanded.

use super::{Entry, MAX_DEPTH, Node, Parsed, Scalar, Value, parse_error};
use crate::lines::Lines;
use katalogr_model::Span;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

pub fn parse_xml(text: &str) -> Parsed {
    let lines = Lines::new(text);
    let mut reader = Reader::from_str(text);
    {
        let c = reader.config_mut();
        c.expand_empty_elements = true;
        c.check_end_names = true;
        c.trim_text(false);
    }
    let mut out = Parsed::default();
    let mut stack: Vec<El> = Vec::new();
    let mut root: Option<Entry> = None;
    loop {
        let before = reader.buffer_position() as usize;
        let ev = match reader.read_event() {
            Ok(ev) => ev,
            Err(e) => {
                let at = (reader.error_position() as usize)
                    .max(before)
                    .min(text.len());
                out.errors
                    .push(parse_error(Some(lines.span(at, at + 1)), e.to_string()));
                while !stack.is_empty() {
                    close(&mut stack, &mut root, &lines, at);
                }
                break;
            }
        };
        let after = reader.buffer_position() as usize;
        match ev {
            Event::Eof => break,
            Event::Start(s) => {
                if stack.len() >= MAX_DEPTH {
                    out.errors.push(parse_error(
                        Some(lines.span(before, before + 1)),
                        format!("nesting deeper than {MAX_DEPTH}"),
                    ));
                    while !stack.is_empty() {
                        close(&mut stack, &mut root, &lines, before);
                    }
                    break;
                }
                let span = lines.span(before, after);
                let mut el = El {
                    name: String::from_utf8_lossy(s.name().as_ref()).into_owned(),
                    entries: Vec::new(),
                    text: String::new(),
                    start: before,
                    start_span: span,
                    children: false,
                };
                attributes(&s, &mut el, span, &mut out);
                stack.push(el);
            }
            Event::End(_) => close(&mut stack, &mut root, &lines, after),
            Event::Text(t) => {
                if let Some(el) = stack.last_mut() {
                    el.text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::CData(t) => {
                if let Some(el) = stack.last_mut() {
                    el.text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::GeneralRef(r) => {
                if let Some(el) = stack.last_mut() {
                    let name = String::from_utf8_lossy(&r).into_owned();
                    match resolve(&name) {
                        Some(s) => el.text.push_str(&s),
                        None => {
                            el.text.push('&');
                            el.text.push_str(&name);
                            el.text.push(';');
                        }
                    }
                }
            }
            Event::Empty(_)
            | Event::Comment(_)
            | Event::Decl(_)
            | Event::PI(_)
            | Event::DocType(_) => {}
        }
    }
    match root {
        Some(entry) => out.docs.push(Node {
            span: entry.value.span,
            value: Value::Map(vec![entry]),
        }),
        None => {
            if out.errors.is_empty() {
                out.errors.push(parse_error(None, "no root element"));
            }
        }
    }
    out
}

struct El {
    name: String,
    entries: Vec<Entry>,
    text: String,
    start: usize,
    start_span: Span,
    children: bool,
}

fn attributes(s: &BytesStart<'_>, el: &mut El, span: Span, out: &mut Parsed) {
    for a in s.attributes() {
        match a {
            Ok(a) => {
                let key = format!("@{}", String::from_utf8_lossy(a.key.as_ref()));
                let raw = String::from_utf8_lossy(&a.value).into_owned();
                el.entries.push(Entry {
                    key,
                    key_span: span,
                    value: Node {
                        span,
                        value: Value::Scalar(Scalar::Str(unescape(&raw))),
                    },
                });
            }
            Err(e) => out.errors.push(parse_error(Some(span), e.to_string())),
        }
    }
}

/// Close the innermost element, which ends before byte `end`.
fn close(stack: &mut Vec<El>, root: &mut Option<Entry>, lines: &Lines<'_>, end: usize) {
    let Some(mut el) = stack.pop() else { return };
    let span = lines.span(el.start, end);
    let text = el.text.trim().to_string();
    let value = if el.entries.is_empty() && !el.children {
        Value::Scalar(Scalar::Str(text))
    } else {
        if !text.is_empty() {
            el.entries.push(Entry {
                key: "#text".to_string(),
                key_span: el.start_span,
                value: Node {
                    span: el.start_span,
                    value: Value::Scalar(Scalar::Str(text)),
                },
            });
        }
        Value::Map(el.entries)
    };
    let entry = Entry {
        key: el.name,
        key_span: el.start_span,
        value: Node { span, value },
    };
    match stack.last_mut() {
        Some(parent) => {
            parent.children = true;
            parent.entries.push(entry);
        }
        None => {
            if root.is_none() {
                *root = Some(entry);
            }
        }
    }
}

/// The text of `&name;` when it is a predefined entity or a character reference.
fn resolve(name: &str) -> Option<String> {
    match name {
        "lt" => Some("<".into()),
        "gt" => Some(">".into()),
        "amp" => Some("&".into()),
        "quot" => Some("\"".into()),
        "apos" => Some("'".into()),
        _ => {
            let n = name.strip_prefix('#')?;
            let code = match n.strip_prefix(['x', 'X']) {
                Some(h) => u32::from_str_radix(h, 16).ok()?,
                None => n.parse().ok()?,
            };
            Some(char::from_u32(code).unwrap_or('\u{fffd}').to_string())
        }
    }
}

/// Resolve the predefined entities and character references in `raw`; any other reference
/// stays as written.
fn unescape(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        match rest.find(';').filter(|&j| j <= 12) {
            Some(j) => match resolve(&rest[1..j]) {
                Some(s) => {
                    out.push_str(&s);
                    rest = &rest[j + 1..];
                }
                None => {
                    out.push('&');
                    rest = &rest[1..];
                }
            },
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_attributes_then_children() {
        let t = "<?xml version=\"1.0\"?>\n<a xmlns:p=\"urn:x\" id=\"1&amp;2\">\n  <p:b>text &lt; more</p:b>\n  <c/>\n  <c k=\"v\">t</c>\n</a>";
        let p = parse_xml(t);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        let a = p.docs[0].get("a").unwrap();
        let keys: Vec<_> = a.entries().iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, ["@xmlns:p", "@id", "p:b", "c", "c"]);
        assert_eq!(a.get("@id").unwrap().as_text(), Some("1&2"));
        assert_eq!(a.get("p:b").unwrap().as_text(), Some("text < more"));
        assert_eq!(a.get("c").unwrap().as_text(), Some(""));
        let c2 = a.entries()[4].value.clone();
        assert_eq!(c2.get("@k").unwrap().as_text(), Some("v"));
        assert_eq!(c2.get("#text").unwrap().as_text(), Some("t"));
        let s = a.span;
        assert_eq!((s.start_line, s.start_col, s.end_line), (2, 1, 6));
        // Malformed: the open elements are closed and kept.
        let p = parse_xml("<a><b>1</b><c></d></a>");
        assert_eq!(p.errors.len(), 1);
        let a = p.docs[0].get("a").unwrap();
        assert_eq!(a.get("b").unwrap().as_text(), Some("1"));
    }

    #[test]
    fn xml_doctype_skipped() {
        let t = "<!DOCTYPE a [<!ENTITY x SYSTEM \"file:///nonexistent/secret\">]>\n<a>&x; &#65; &amp;</a>";
        let p = parse_xml(t);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert_eq!(p.docs[0].get("a").unwrap().as_text(), Some("&x; A &"));
        let deep = format!("{}{}", "<a>".repeat(65), "</a>".repeat(65));
        let p = parse_xml(&deep);
        assert!(
            p.errors
                .iter()
                .any(|e| e.to_string().contains("deeper than 64"))
        );
    }
}
