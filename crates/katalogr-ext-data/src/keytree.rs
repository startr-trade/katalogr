//! A parsed document as symbols (FEAT-009 section 5.1 and 6.3): `doc` -> `key` -> `item`, each
//! with a JSON Pointer, a value type and, for scalars, the value text.

use crate::MAX_SYMBOLS;
use crate::tree::{Node, Scalar, Value};
use katalogr_model::{
    ArtifactName, Confidence, Fact, Id, Link, LinkKind, Name, Provenance, Segment, Span,
    SymbolName, SymbolNode,
};
use std::collections::{BTreeMap, HashMap};

/// Accumulates the symbols and `contains` links of one file.
pub struct KeyTreeBuilder {
    artifact: ArtifactName,
    artifact_canon: String,
    provenance: Provenance,
    /// Occurrences of each canonical name (without ordinal) so far.
    ordinals: HashMap<String, u32>,
    pub symbols: Vec<SymbolNode>,
    pub links: Vec<Link>,
    /// Key and item symbols emitted so far.
    count: usize,
    /// True once the cap stopped the walk.
    pub truncated: bool,
}

impl KeyTreeBuilder {
    pub fn new(artifact: ArtifactName, provenance: Provenance) -> KeyTreeBuilder {
        KeyTreeBuilder {
            artifact_canon: artifact.canonical(),
            artifact,
            provenance,
            ordinals: HashMap::new(),
            symbols: Vec::new(),
            links: Vec::new(),
            count: 0,
            truncated: false,
        }
    }

    fn add(
        &mut self,
        parent: Option<(&[Segment], Id, &str)>,
        kind: &str,
        name: &str,
        span: Span,
        facts: BTreeMap<String, Fact>,
    ) -> (Vec<Segment>, Id, String) {
        let mut segments = parent.map_or_else(Vec::new, |(s, _, _)| s.to_vec());
        segments.push(Segment {
            kind: kind.to_string(),
            name: name.to_string(),
        });
        // `<artifact>#<kind>:<name>/…` built from the parent's, with the model's escaping.
        let mut base = match parent {
            Some((_, _, b)) => format!("{b}/"),
            None => format!("{}#", self.artifact_canon),
        };
        base.push_str(kind);
        base.push(':');
        escape_segment(name, &mut base);
        let mut sym = SymbolName {
            artifact: self.artifact.clone(),
            segments: segments.clone(),
            ordinal: 1,
        };
        let mut canon = base.clone();
        let n = match self.ordinals.get_mut(&canon) {
            Some(n) => {
                *n += 1;
                *n
            }
            None => {
                self.ordinals.insert(canon.clone(), 1);
                1
            }
        };
        if n >= 2 {
            sym.ordinal = n;
            canon.push_str(&format!("~{n}"));
        }
        let id = Id::of(&canon);
        if let Some((_, from, _)) = parent {
            self.links.push(Link {
                from,
                to: id,
                kind: LinkKind("contains".to_string()),
                confidence: Confidence::Extracted,
                span: Some(span),
                provenance: self.provenance.clone(),
            });
        }
        self.symbols.push(SymbolNode {
            name: sym,
            kind: kind.to_string(),
            span,
            facts,
        });
        debug_assert_eq!(self.symbols.last().map(|s| s.name.canonical()), Some(canon));
        (segments, id, base)
    }

    /// The symbols are in document order, which is deterministic; the shard codec sorts them
    /// by canonical name when it writes them.
    pub fn finish_symbols(&mut self) {}
}

/// The model's escaping of a segment name: `: / # ~ %` and whitespace, as uppercase `%XX`.
pub fn escape_segment(name: &str, out: &mut String) {
    for c in name.chars() {
        if matches!(c, '/' | ':' | '#' | '~' | '%') || c.is_whitespace() {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        } else {
            out.push(c);
        }
    }
}

/// The `value_type` word of a node.
pub fn value_type(n: &Node) -> &'static str {
    match &n.value {
        Value::Scalar(Scalar::Str(_)) => "str",
        Value::Scalar(Scalar::Num(_)) => "num",
        Value::Scalar(Scalar::Bool(_)) => "bool",
        Value::Scalar(Scalar::Null) => "null",
        Value::Map(_) => "map",
        Value::Seq(_) => "list",
        Value::Alias(_) => "alias",
    }
}

fn facts(pointer: Option<&str>, n: &Node) -> BTreeMap<String, Fact> {
    let mut f = BTreeMap::new();
    if let Some(p) = pointer {
        f.insert("pointer".to_string(), Fact::Str(p.to_string()));
    }
    f.insert(
        "value_type".to_string(),
        Fact::Str(value_type(n).to_string()),
    );
    match &n.value {
        Value::Alias(a) => {
            f.insert("value".to_string(), Fact::Str(format!("*{a}")));
        }
        _ => {
            if let Some(t) = n.as_text() {
                f.insert("value".to_string(), Fact::Str(t.to_string()));
            }
        }
    }
    f
}

/// RFC 6901 escaping of one reference token.
fn escape_token(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

/// The local part of an XML qualified name; an attribute keeps its `@`.
fn xml_local(key: &str) -> String {
    let (at, rest) = match key.strip_prefix('@') {
        Some(r) => ("@", r),
        None => ("", key),
    };
    let local = rest.rsplit_once(':').map_or(rest, |(_, l)| l);
    format!("{at}{local}")
}

/// Emit one document. `xml` strips namespace prefixes from names.
pub fn emit(b: &mut KeyTreeBuilder, doc: &Node, stem: &str, xml: bool) {
    let (segs, id, base) = b.add(None, "doc", stem, doc.span, facts(None, doc));
    walk(b, doc, &segs, id, &base, "", xml);
}

fn walk(
    b: &mut KeyTreeBuilder,
    n: &Node,
    segs: &[Segment],
    id: Id,
    base: &str,
    pointer: &str,
    xml: bool,
) {
    match &n.value {
        Value::Map(entries) => {
            let mut seen: HashMap<String, u32> = HashMap::new();
            for e in entries {
                if b.truncated {
                    return;
                }
                if b.count >= MAX_SYMBOLS {
                    b.truncated = true;
                    return;
                }
                let name = if xml {
                    xml_local(&e.key)
                } else {
                    e.key.clone()
                };
                let times = seen.entry(name.clone()).or_insert(0);
                *times += 1;
                let mut p = format!("{pointer}/{}", escape_token(&name));
                if *times >= 2 {
                    p.push_str(&format!("[{times}]"));
                }
                let span = Span {
                    start_line: e.key_span.start_line,
                    start_col: e.key_span.start_col,
                    end_line: e.value.span.end_line.max(e.key_span.end_line),
                    end_col: if e.value.span.end_line >= e.key_span.end_line {
                        e.value.span.end_col
                    } else {
                        e.key_span.end_col
                    },
                };
                b.count += 1;
                let (s, i, nb) = b.add(
                    Some((segs, id, base)),
                    "key",
                    &name,
                    span,
                    facts(Some(&p), &e.value),
                );
                walk(b, &e.value, &s, i, &nb, &p, xml);
            }
        }
        Value::Seq(items) => {
            for (idx, item) in items.iter().enumerate() {
                if b.truncated {
                    return;
                }
                if b.count >= MAX_SYMBOLS {
                    b.truncated = true;
                    return;
                }
                let p = format!("{pointer}/{idx}");
                b.count += 1;
                let (s, i, nb) = b.add(
                    Some((segs, id, base)),
                    "item",
                    &idx.to_string(),
                    item.span,
                    facts(Some(&p), item),
                );
                walk(b, item, &s, i, &nb, &p, xml);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_matches_the_model() {
        let artifact = ArtifactName::new("r", "a b/c.yaml").unwrap();
        for name in [
            "plain",
            "a/b",
            "a:b",
            "a#b",
            "a~b",
            "100%",
            "sp ace\ttab",
            "é/ü:",
            "",
        ] {
            let mut ours = format!("{}#key:", artifact.canonical());
            escape_segment(name, &mut ours);
            let sym = SymbolName {
                artifact: artifact.clone(),
                segments: vec![Segment {
                    kind: "key".into(),
                    name: name.into(),
                }],
                ordinal: 1,
            };
            assert_eq!(ours, sym.canonical(), "{name:?}");
        }
    }
}
