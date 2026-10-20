//! Test-only extractor over a tiny line format (`.tx` files), registered only with the
//! `test-extractor` feature so integration tests have symbols to query.
//! Directives: `sym <kind> <name>`, `ref <from> <kind> <ns:key>`, `export <ns:key> <sym>`,
//! `link <from> <kind> <to>`, `bind <sym> <provides|consumes|declares> <kind>:<key>`,
//! `iref <from> <kind> <ns:key>` (an inferred reference),
//! `fact <sym|@> <key> <value...>` (a string fact on a symbol or, with `@`, on the artifact; the
//! value is the rest of the line), `note <sym|@> <MARKER> <text...>` (appends a rationale entry
//! at this line); `#` starts a comment line. Code kinds: `fn` and `type`.

use katalogr_model::insight::FACT_RATIONALE;
use katalogr_model::{
    ArtifactName, Binding, Confidence, ContractName, ContractRole, Export, ExportKey, ExtractError,
    ExtractInput, Extractor, Fact, Fragment, Id, Link, LinkKind, Name, OutRef, Provenance, RelPath,
    Segment, Span, SymbolName, SymbolNode,
};
use std::collections::BTreeMap;

pub struct TestExtractor;

impl Extractor for TestExtractor {
    fn id(&self) -> &'static str {
        "test"
    }

    fn handles(&self, path: &RelPath) -> bool {
        path.as_str().ends_with(".tx")
    }

    fn code_kinds(&self) -> &'static [&'static str] {
        &["fn", "type"]
    }

    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        let bad = |span: Option<Span>, message: String| ExtractError::Parse { span, message };
        let text =
            std::str::from_utf8(input.content).map_err(|e| bad(None, format!("not utf-8: {e}")))?;
        let artifact = ArtifactName {
            repo: input.repo.to_string(),
            path: input.path.clone(),
        };
        let provenance = Provenance {
            repo: input.repo.to_string(),
            path: input.path.clone(),
            revision: input.revision.to_string(),
        };
        let mut symbols: Vec<SymbolNode> = Vec::new();
        let mut links = Vec::new();
        let mut refs = Vec::new();
        let mut exports = Vec::new();
        let mut bindings = Vec::new();
        let mut artifact_facts: BTreeMap<String, Fact> = BTreeMap::new();

        for (idx, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let line_no = idx as u32 + 1;
            let span = Span {
                start_line: line_no,
                start_col: 1,
                end_line: line_no,
                end_col: line.chars().count().max(1) as u32,
            };
            let words: Vec<&str> = trimmed.split_whitespace().collect();
            let find = |name: &str| -> Result<Id, ExtractError> {
                symbols
                    .iter()
                    .find(|s| s.name.segments[0].name == name)
                    .map(|s| s.name.id())
                    .ok_or_else(|| bad(Some(span), format!("undeclared symbol {name:?}")))
            };
            let key = |s: &str| -> Result<ExportKey, ExtractError> {
                s.split_once(':')
                    .map(|(namespace, key)| ExportKey {
                        namespace: namespace.to_string(),
                        key: key.to_string(),
                    })
                    .ok_or_else(|| bad(Some(span), format!("bad key {s:?}")))
            };
            // `fact` and `note` take the rest of the line as their value.
            if matches!(words.first(), Some(&"fact") | Some(&"note")) {
                let mut parts = trimmed.splitn(4, char::is_whitespace);
                let directive = parts.next().unwrap_or("");
                let (Some(target), Some(word), Some(value)) = (
                    parts.next(),
                    parts.next(),
                    parts.next().map(str::trim).filter(|v| !v.is_empty()),
                ) else {
                    return Err(bad(Some(span), format!("incomplete directive {trimmed:?}")));
                };
                let facts = if target == "@" {
                    &mut artifact_facts
                } else {
                    let at = symbols
                        .iter()
                        .position(|s| s.name.segments[0].name == target)
                        .ok_or_else(|| bad(Some(span), format!("undeclared symbol {target:?}")))?;
                    &mut symbols[at].facts
                };
                if directive == "fact" {
                    facts.insert(word.to_string(), Fact::Str(value.to_string()));
                } else {
                    let entry = Fact::Map(BTreeMap::from([
                        ("marker".to_string(), Fact::Str(word.to_string())),
                        ("line".to_string(), Fact::Int(i64::from(line_no))),
                        ("text".to_string(), Fact::Str(value.to_string())),
                    ]));
                    match facts.entry(FACT_RATIONALE.to_string()) {
                        std::collections::btree_map::Entry::Occupied(mut o) => {
                            if let Fact::List(l) = o.get_mut() {
                                l.push(entry);
                            }
                        }
                        std::collections::btree_map::Entry::Vacant(v) => {
                            v.insert(Fact::List(vec![entry]));
                        }
                    }
                }
                continue;
            }
            match words.as_slice() {
                ["sym", kind, name] => {
                    let segments = vec![Segment {
                        kind: kind.to_string(),
                        name: name.to_string(),
                    }];
                    let ordinal = symbols
                        .iter()
                        .filter(|s| s.name.segments == segments)
                        .count() as u32
                        + 1;
                    symbols.push(SymbolNode {
                        name: SymbolName {
                            artifact: artifact.clone(),
                            segments,
                            ordinal,
                        },
                        kind: kind.to_string(),
                        span,
                        facts: BTreeMap::new(),
                    });
                }
                ["ref", from, kind, target] => refs.push(OutRef {
                    from: find(from)?,
                    kind: LinkKind(kind.to_string()),
                    span: Some(span),
                    candidates: vec![key(target)?],
                    confidence: Confidence::Extracted,
                    origin: None,
                    binds: None,
                }),
                ["iref", from, kind, target] => refs.push(OutRef {
                    from: find(from)?,
                    kind: LinkKind(kind.to_string()),
                    span: Some(span),
                    candidates: vec![key(target)?],
                    confidence: Confidence::Inferred,
                    origin: None,
                    binds: None,
                }),
                ["export", k, sym] => exports.push(Export {
                    key: key(k)?,
                    target: find(sym)?,
                }),
                ["link", from, kind, to] => links.push(Link {
                    from: find(from)?,
                    to: find(to)?,
                    kind: LinkKind(kind.to_string()),
                    confidence: Confidence::Extracted,
                    span: Some(span),
                    provenance: provenance.clone(),
                }),
                ["bind", sym, role, contract] => {
                    let role = match *role {
                        "provides" => ContractRole::Provides,
                        "consumes" => ContractRole::Consumes,
                        "declares" => ContractRole::Declares,
                        other => return Err(bad(Some(span), format!("bad role {other:?}"))),
                    };
                    let (kind, ckey) = contract
                        .split_once(':')
                        .ok_or_else(|| bad(Some(span), format!("bad contract {contract:?}")))?;
                    bindings.push(Binding {
                        symbol: find(sym)?,
                        contract: ContractName {
                            kind: kind.to_string(),
                            key: ckey.to_string(),
                        },
                        role,
                        span: Some(span),
                        confidence: Confidence::Extracted,
                        origin: None,
                    });
                }
                _ => return Err(bad(Some(span), format!("unknown directive {trimmed:?}"))),
            }
        }

        Ok(Fragment {
            provenance,
            extension: self.id().to_string(),
            artifact: katalogr_model::ArtifactNode {
                name: artifact,
                facts: artifact_facts,
            },
            symbols,
            links,
            refs,
            exports,
            bindings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(text: &str) -> Result<Fragment, ExtractError> {
        let path = RelPath::new("a.tx").unwrap();
        TestExtractor.extract(&ExtractInput {
            repo: "r",
            path: &path,
            revision: "rev",
            content: text.as_bytes(),
            context: &[],
        })
    }

    #[test]
    fn directives_parse() {
        let f = extract(
            "sym fn a\nsym type T\nfact a root main\nfact @ package p1 and more\n\
             note a WHY because of x\nnote a NOTE second\nnote @ RATIONALE file level\n\
             iref a calls ns:key\n",
        )
        .unwrap();
        assert_eq!(f.symbols[0].facts["root"], Fact::Str("main".into()));
        assert_eq!(f.artifact.facts["package"], Fact::Str("p1 and more".into()));
        let Fact::List(l) = &f.symbols[0].facts["rationale"] else {
            panic!("list")
        };
        assert_eq!(l.len(), 2);
        let Fact::Map(m) = &l[0] else { panic!("map") };
        assert_eq!(m["marker"], Fact::Str("WHY".into()));
        assert_eq!(m["line"], Fact::Int(5));
        assert_eq!(m["text"], Fact::Str("because of x".into()));
        assert!(f.artifact.facts.contains_key("rationale"));
        assert_eq!(f.refs.len(), 1);
        assert_eq!(f.refs[0].confidence, Confidence::Inferred);
        assert_eq!(TestExtractor.code_kinds(), ["fn", "type"]);
        assert!(extract("fact nope k v\n").is_err());
        assert!(extract("sym fn a\nfact a k\n").is_err());
    }
}
