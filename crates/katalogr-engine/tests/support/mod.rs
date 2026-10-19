//! Test-only extractor over a tiny line format; keeps engine tests independent of real languages.
#![allow(dead_code)]

use camino::{Utf8Path, Utf8PathBuf};
use katalogr_engine::registry::ExtensionRegistry;
use katalogr_engine::{Consistency, Engine, EventKind, NewEvent, QueryOptions, Scope};
use katalogr_model::{
    ArtifactName, Binding, Confidence, ContractName, ContractRole, Export, ExportKey, ExtractError,
    ExtractInput, Extractor, Fragment, Id, Link, LinkKind, Name, OutRef, Provenance, RelPath,
    Segment, Span, SymbolName, SymbolNode,
};
use katalogr_store::{Kindex, ProjectConfig, ProjectName, RepoConfig};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub struct TestExtractor;

impl Extractor for TestExtractor {
    fn id(&self) -> &'static str {
        "test"
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
                    .find(|s| {
                        s.name
                            .segments
                            .iter()
                            .map(|g| g.name.as_str())
                            .collect::<Vec<_>>()
                            .join("/")
                            == name
                    })
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
                // A symbol nested below an existing one, named by the parent's path.
                ["sub", parent, kind, name] => {
                    let parent_sym = symbols
                        .iter()
                        .find(|s| {
                            s.name
                                .segments
                                .iter()
                                .map(|g| g.name.as_str())
                                .collect::<Vec<_>>()
                                .join("/")
                                == *parent
                        })
                        .ok_or_else(|| bad(Some(span), format!("undeclared symbol {parent:?}")))?;
                    let mut segments = parent_sym.name.segments.clone();
                    let parent_id = parent_sym.name.id();
                    segments.push(Segment {
                        kind: kind.to_string(),
                        name: name.to_string(),
                    });
                    let ordinal = symbols
                        .iter()
                        .filter(|s| s.name.segments == segments)
                        .count() as u32
                        + 1;
                    let sym = SymbolName {
                        artifact: artifact.clone(),
                        segments,
                        ordinal,
                    };
                    links.push(Link {
                        from: parent_id,
                        to: sym.id(),
                        kind: LinkKind("contains".to_string()),
                        confidence: Confidence::Extracted,
                        span: Some(span),
                        provenance: provenance.clone(),
                    });
                    symbols.push(SymbolNode {
                        name: sym,
                        kind: kind.to_string(),
                        span,
                        facts: BTreeMap::new(),
                    });
                }
                // A string fact on a symbol named by its path.
                ["fact", path, key, text] => {
                    let id = find(path)?;
                    let sym = symbols
                        .iter_mut()
                        .find(|s| s.name.id() == id)
                        .expect("found");
                    sym.facts
                        .insert(key.to_string(), katalogr_model::Fact::Str(text.to_string()));
                }
                ["ref", from, kind, target] => refs.push(OutRef {
                    from: find(from)?,
                    kind: LinkKind(kind.to_string()),
                    span: Some(span),
                    candidates: vec![key(target)?],
                    confidence: Confidence::Extracted,
                    origin: None,
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

/// A second extractor that no path selects by itself (`handles` is false): a profile `select`
/// routes files to it. Every first word of a line becomes a symbol `alt:<word>`.
pub struct TestAltExtractor;

impl Extractor for TestAltExtractor {
    fn id(&self) -> &'static str {
        "test-alt"
    }

    fn handles(&self, _: &RelPath) -> bool {
        false
    }

    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        let text = std::str::from_utf8(input.content).map_err(|e| ExtractError::Parse {
            span: None,
            message: format!("not utf-8: {e}"),
        })?;
        let artifact = ArtifactName {
            repo: input.repo.to_string(),
            path: input.path.clone(),
        };
        let mut symbols: Vec<SymbolNode> = Vec::new();
        for (idx, line) in text.lines().enumerate() {
            let Some(word) = line.split_whitespace().next() else {
                continue;
            };
            let segments = vec![Segment {
                kind: "alt".into(),
                name: word.to_string(),
            }];
            let ordinal = symbols
                .iter()
                .filter(|s| s.name.segments == segments)
                .count() as u32
                + 1;
            let n = idx as u32 + 1;
            symbols.push(SymbolNode {
                name: SymbolName {
                    artifact: artifact.clone(),
                    segments,
                    ordinal,
                },
                kind: "alt".into(),
                span: Span {
                    start_line: n,
                    start_col: 1,
                    end_line: n,
                    end_col: line.chars().count().max(1) as u32,
                },
                facts: BTreeMap::new(),
            });
        }
        Ok(Fragment {
            provenance: Provenance {
                repo: input.repo.to_string(),
                path: input.path.clone(),
                revision: input.revision.to_string(),
            },
            extension: self.id().to_string(),
            artifact: katalogr_model::ArtifactNode {
                name: artifact,
                facts: BTreeMap::new(),
            },
            symbols,
            links: Vec::new(),
            refs: Vec::new(),
            exports: Vec::new(),
            bindings: Vec::new(),
        })
    }
}

/// Create a kindex at `kx` holding project `p` with one repository `r`.
pub fn init_project(kx: &Utf8Path) {
    Kindex::init(kx)
        .unwrap()
        .create_project(
            &ProjectName::new("p").unwrap(),
            ProjectConfig {
                repos: vec![RepoConfig::new("r")],
                ..Default::default()
            },
        )
        .unwrap();
}

/// Open project `p` of the kindex at `kx`, with the test extractor and repository `r` rooted
/// at `work`.
pub fn open_engine(kx: &Utf8Path, work: &Utf8Path) -> Engine {
    let dir = Kindex::open(kx)
        .unwrap()
        .project(&ProjectName::new("p").unwrap())
        .unwrap();
    let mut reg = ExtensionRegistry::new();
    reg.register(Box::new(TestExtractor));
    reg.register(Box::new(TestAltExtractor));
    let roots = BTreeMap::from([("r".to_string(), work.to_path_buf())]);
    Engine::open(dir, reg, roots, Instant::now() + Duration::from_secs(10)).unwrap()
}

/// Drain the queue (wait-all) and return the canonical graph.
pub fn drained_graph(e: &Engine) -> String {
    let o = QueryOptions {
        consistency: Consistency::WaitAll,
        timeout: Duration::from_secs(30),
        scope: Scope::Project,
    };
    let a = e
        .with_consistency(&o, |tx| katalogr_engine::canonical_graph(tx))
        .unwrap();
    assert!(!a.stale, "queue drained");
    a.value
}

/// Index everything under the repository root from scratch: a rescan event, then wait-all.
pub fn full_index(e: &Engine) -> String {
    e.inbox().append(&NewEvent::rescan("r")).unwrap();
    drained_graph(e)
}

/// A fresh kindex under `tmp/<name>`, indexed fully from `work`.
pub fn fresh_index(tmp: &Utf8Path, name: &str, work: &Utf8Path) -> (Utf8PathBuf, String) {
    let kx = tmp.join(name);
    init_project(&kx);
    let e = open_engine(&kx, work);
    let g = full_index(&e);
    (kx, g)
}

pub fn file_event(path: &str) -> NewEvent {
    NewEvent::file("r", &RelPath::new(path).unwrap(), EventKind::Changed)
}
