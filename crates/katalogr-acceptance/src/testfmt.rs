//! The `.tx` test format extractor (a copy of the engine's test support extractor, plus
//! `patterns()`), used until `katalogr-dist` registers one.

use katalogr_engine::registry::ExtensionRegistry;
use katalogr_model::{
    ArtifactName, Binding, Confidence, ContractName, ContractRole, Export, ExportKey, ExtractError,
    ExtractInput, Extractor, Fragment, Id, Link, LinkKind, Name, OutRef, Provenance, RelPath,
    Segment, Span, SymbolName, SymbolNode,
};
use std::collections::BTreeMap;

/// A registry holding only the test extractor.
pub fn registry() -> ExtensionRegistry {
    let mut reg = ExtensionRegistry::new();
    reg.register(Box::new(TestExtractor));
    reg
}

pub struct TestExtractor;

impl Extractor for TestExtractor {
    fn id(&self) -> &'static str {
        "test"
    }

    fn patterns(&self) -> &'static [&'static str] {
        &[
            "symbols",
            "refs-resolve",
            "refs-ambiguous",
            "refs-unresolved",
        ]
    }

    fn handles(&self, path: &RelPath) -> bool {
        path.as_str().ends_with(".tx")
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
                facts: BTreeMap::new(),
            },
            symbols,
            links,
            refs,
            exports,
            bindings,
        })
    }
}
