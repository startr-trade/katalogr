//! Test-only extractor over a tiny line format; keeps engine tests independent of real languages.
#![allow(dead_code)]

use camino::{Utf8Path, Utf8PathBuf};
use katalogr_engine::parallel::Parallelism;
use katalogr_engine::registry::ExtensionRegistry;
use katalogr_engine::{Consistency, Engine, EventKind, NewEvent, QueryOptions, Scope};
use katalogr_model::{
    ArtifactName, Binding, CallContext, Confidence, ContractName, ContractRole, EmbeddedInput,
    EmbeddedText, Export, ExportKey, ExtractError, ExtractInput, Extraction, Extractor, Fragment,
    Id, KeyNormaliser, Link, LinkKind, Name, OutRef, Provenance, RelPath, Segment, Span,
    SymbolName, SymbolNode,
};
use katalogr_store::{Kindex, ProjectConfig, ProjectName, RepoConfig};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

pub struct TestExtractor;

/// The test key normaliser `upper`: upper-cases, and yields nothing for empty input.
pub struct Upper;
pub static UPPER: Upper = Upper;
static NORMALISERS: [&dyn KeyNormaliser; 1] = [&UPPER];
/// Its version; tests change the hash through `Upper::VERSION`.
impl KeyNormaliser for Upper {
    fn id(&self) -> &'static str {
        "upper"
    }
    fn version(&self) -> u32 {
        1
    }
    fn normalise(&self, raw: &str) -> Option<String> {
        (!raw.is_empty()).then(|| raw.to_uppercase())
    }
}

/// A second version of `upper`, to see the profile hash change.
pub struct UpperV2;
pub static UPPER_V2: UpperV2 = UpperV2;
impl KeyNormaliser for UpperV2 {
    fn id(&self) -> &'static str {
        "upper"
    }
    fn version(&self) -> u32 {
        2
    }
    fn normalise(&self, raw: &str) -> Option<String> {
        (!raw.is_empty()).then(|| raw.to_uppercase())
    }
}

/// Index into [`SALTS`] returned by [`SaltedExtractor::revision_salt`].
pub static SALT: AtomicUsize = AtomicUsize::new(0);
pub const SALTS: [&str; 2] = ["", "v2"];

/// Handles `*.salt`: one `fn` symbol per non-empty line. Its revision salt is switchable.
pub struct SaltedExtractor;

impl Extractor for SaltedExtractor {
    fn id(&self) -> &'static str {
        "salted"
    }

    fn handles(&self, path: &RelPath) -> bool {
        path.as_str().ends_with(".salt")
    }

    fn revision_salt(&self) -> &'static str {
        SALTS[SALT.load(Ordering::SeqCst)]
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
            let name = line.trim();
            if name.is_empty() {
                continue;
            }
            let line_no = idx as u32 + 1;
            let segments = vec![Segment {
                kind: "fn".to_string(),
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
                kind: "fn".to_string(),
                span: Span {
                    start_line: line_no,
                    start_col: 1,
                    end_line: line_no,
                    end_col: name.chars().count().max(1) as u32,
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

impl Extractor for TestExtractor {
    fn id(&self) -> &'static str {
        "test"
    }

    fn handles(&self, path: &RelPath) -> bool {
        path.as_str().ends_with(".tx")
    }

    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        self.extract_texts(input).map(|(f, _)| f)
    }

    fn embeds(&self) -> bool {
        true
    }

    fn key_normalisers(&self) -> &'static [&'static dyn KeyNormaliser] {
        &NORMALISERS
    }

    fn extract_full(&self, input: &ExtractInput) -> Result<Extraction, ExtractError> {
        self.extract_texts(input)
            .map(|(fragment, embedded)| Extraction {
                fragment,
                errors: Vec::new(),
                embedded,
            })
    }
}

/// The test extractor with a shared counter of `extract_full` calls.
pub struct CountedExtractor(pub Arc<AtomicUsize>);

impl Extractor for CountedExtractor {
    fn id(&self) -> &'static str {
        "test"
    }
    fn handles(&self, path: &RelPath) -> bool {
        TestExtractor.handles(path)
    }
    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        TestExtractor.extract(input)
    }
    fn embeds(&self) -> bool {
        true
    }
    fn extract_full(&self, input: &ExtractInput) -> Result<Extraction, ExtractError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        TestExtractor.extract_full(input)
    }
}

/// A guest (id `tguest`) that binds `use <word>` texts to `res:<word>`, with a deliberately
/// wrong symbol and `Extracted` confidence so tests can see the engine normalise them.
pub struct TestGuest(pub &'static str);

impl Extractor for TestGuest {
    fn id(&self) -> &'static str {
        "tguest"
    }
    fn handles(&self, _: &RelPath) -> bool {
        false
    }
    fn extract(&self, _: &ExtractInput) -> Result<Fragment, ExtractError> {
        Err(ExtractError::Unsupported("guest only".into()))
    }
    fn context_files(&self) -> &'static [&'static str] {
        &["tguest.ctx"]
    }
    fn build_context(&self, files: &[(RelPath, Vec<u8>)]) -> Result<Vec<u8>, ExtractError> {
        Ok(files.iter().flat_map(|(_, b)| b.clone()).collect())
    }
    fn embedded_context(&self, context: &[u8]) -> Vec<u8> {
        context
            .split(|b| *b == b'\n')
            .next()
            .unwrap_or_default()
            .to_vec()
    }
    fn embedded_version(&self) -> Option<&'static str> {
        Some(self.0)
    }
    fn accepts_embedded(&self, text: &str) -> bool {
        text.starts_with("use ")
    }
    fn extract_embedded(&self, input: &EmbeddedInput) -> Vec<Binding> {
        let word = input.text.split_whitespace().nth(1).unwrap_or("");
        let ctx = String::from_utf8_lossy(input.context);
        let key = |k: String| Binding {
            symbol: Id::of("wrong"),
            contract: ContractName {
                kind: "res".into(),
                key: k,
            },
            role: ContractRole::Consumes,
            span: None,
            confidence: Confidence::Extracted,
            origin: None,
        };
        let mut out = vec![key(if ctx.is_empty() {
            word.to_string()
        } else {
            format!("{ctx}.{word}")
        })];
        if let Some(c) = input.call {
            out.push(key(format!("call.{}.{}", c.callee, c.arg)));
        }
        out
    }
}

impl TestExtractor {
    /// The fragment and the texts offered by `lit <sym> [call=<callee>:<arg>] <text...>` lines.
    fn extract_texts(
        &self,
        input: &ExtractInput,
    ) -> Result<(Fragment, Vec<EmbeddedText>), ExtractError> {
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
        let mut lits: Vec<(usize, String, Option<CallContext>, String, Span)> = Vec::new();
        let mut str_calls: BTreeMap<String, Vec<katalogr_model::StrCall>> = BTreeMap::new();

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
            // `strcall <sym|@> <text…>`: a recorded call of the symbol (`@`: the artifact).
            if let ["strcall", target, ..] = words.as_slice() {
                let text = trimmed["strcall".len()..].trim_start()[target.len()..].trim_start();
                str_calls
                    .entry(target.to_string())
                    .or_default()
                    .push(katalogr_model::StrCall {
                        text: text.to_string(),
                        span,
                        refs: Vec::new(),
                    });
                continue;
            }
            // `argref <sym|@> <text> local:<name>|<ns>:<key>[!]`: attaches to the last strcall.
            if let ["argref", target, text, r] = words.as_slice() {
                let (r, inferred) = match r.strip_suffix('!') {
                    Some(r) => (r, true),
                    None => (*r, false),
                };
                let arg = match r.strip_prefix("local:") {
                    Some(rel) => katalogr_model::ArgRef::Local(rel.to_string()),
                    None => katalogr_model::ArgRef::Candidates(vec![key(r)?]),
                };
                let call = str_calls
                    .get_mut(*target)
                    .and_then(|v| v.last_mut())
                    .ok_or_else(|| bad(Some(span), format!("no strcall for {target:?}")))?;
                call.refs.push((text.to_string(), arg, inferred));
                continue;
            }
            if let ["lit", sym, ..] = words.as_slice() {
                let rest = trimmed["lit".len()..].trim_start()[sym.len()..].trim_start();
                let (call, text) = match rest.split_once(' ') {
                    Some((w, t)) if w.starts_with("call=") => {
                        let (callee, arg) = w["call=".len()..]
                            .rsplit_once(':')
                            .ok_or_else(|| bad(Some(span), format!("bad call {w:?}")))?;
                        let arg: u32 = arg
                            .parse()
                            .map_err(|_| bad(Some(span), format!("bad call {w:?}")))?;
                        (CallContext::new(callee, arg), t)
                    }
                    _ => (None, rest),
                };
                lits.push((idx, sym.to_string(), call, text.to_string(), span));
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
                    binds: None,
                }),
                // A reference with several candidates, tried in the order given.
                ["refs", from, kind, cands @ ..] if !cands.is_empty() => refs.push(OutRef {
                    from: find(from)?,
                    kind: LinkKind(kind.to_string()),
                    span: Some(span),
                    candidates: cands.iter().map(|c| key(c)).collect::<Result<_, _>>()?,
                    confidence: Confidence::Extracted,
                    origin: None,
                    binds: None,
                }),
                // A reference holding a contract binding (ADR-0022).
                ["boundref", from, role, contract, cand] => {
                    let role = parse_role(role).map_err(|m| bad(Some(span), m))?;
                    let (ckind, ckey) = contract
                        .split_once(':')
                        .ok_or_else(|| bad(Some(span), format!("bad contract {contract:?}")))?;
                    refs.push(OutRef {
                        from: find(from)?,
                        kind: LinkKind(role.as_str().to_string()),
                        span: Some(span),
                        candidates: vec![key(cand)?],
                        confidence: Confidence::Extracted,
                        origin: None,
                        binds: Some(katalogr_model::RefBinding {
                            contract: ContractName {
                                kind: ckind.to_string(),
                                key: ckey.to_string(),
                            },
                            role,
                        }),
                    });
                }
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

        let mut artifact_facts = BTreeMap::new();
        let mut table = katalogr_model::calls::CandTable::default();
        for (target, calls) in &str_calls {
            let Some(fact) = katalogr_model::calls::str_calls_fact(calls, &mut table) else {
                continue;
            };
            let name = katalogr_model::calls::STR_CALLS_FACT.to_string();
            if target == "@" {
                artifact_facts.insert(name, fact);
                continue;
            }
            let sym = symbols
                .iter_mut()
                .find(|s| {
                    s.name
                        .segments
                        .iter()
                        .map(|g| g.name.as_str())
                        .collect::<Vec<_>>()
                        .join("/")
                        == *target
                })
                .ok_or_else(|| bad(None, format!("undeclared symbol {target:?}")))?;
            sym.facts.insert(name, fact);
        }
        if let Some(t) = table.into_fact() {
            artifact_facts.insert(katalogr_model::calls::STR_CALL_CANDS_FACT.to_string(), t);
        }
        let mut embedded = Vec::new();
        for (_, sym, call, text, span) in lits {
            let owner = symbols
                .iter()
                .find(|s| s.name.segments[0].name == sym)
                .map(|s| s.name.id())
                .ok_or_else(|| bad(Some(span), format!("undeclared symbol {sym:?}")))?;
            embedded.push(EmbeddedText {
                owner,
                span,
                text,
                holes: Vec::new(),
                call,
            });
        }
        Ok((
            Fragment {
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
            },
            embedded,
        ))
    }
}

fn parse_role(role: &str) -> Result<ContractRole, String> {
    match role {
        "provides" => Ok(ContractRole::Provides),
        "consumes" => Ok(ContractRole::Consumes),
        "declares" => Ok(ContractRole::Declares),
        other => Err(format!("bad role {other:?}")),
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
    let mut reg = ExtensionRegistry::new();
    reg.register(Box::new(TestExtractor));
    reg.register(Box::new(TestAltExtractor));
    reg.register(Box::new(TestGuest("1")));
    open_engine_with(kx, work, reg)
}

/// Like [`open_engine`] with a registry of the caller's choosing. One thread: the existing
/// suites exercise the sequential path; the parallel path has its own variants.
pub fn open_engine_with(kx: &Utf8Path, work: &Utf8Path, reg: ExtensionRegistry) -> Engine {
    open_engine_par(kx, work, reg, Parallelism::sequential())
}

/// [`open_engine`] with `n` threads.
pub fn open_engine_threads(kx: &Utf8Path, work: &Utf8Path, n: usize) -> Engine {
    let mut reg = ExtensionRegistry::new();
    reg.register(Box::new(TestExtractor));
    reg.register(Box::new(TestAltExtractor));
    reg.register(Box::new(TestGuest("1")));
    let p = Parallelism {
        threads: std::num::NonZeroUsize::new(n).unwrap(),
        ..Parallelism::sequential()
    };
    open_engine_par(kx, work, reg, p)
}

/// Like [`open_engine_with`] with the parallelism of the caller's choosing.
pub fn open_engine_par(
    kx: &Utf8Path,
    work: &Utf8Path,
    reg: ExtensionRegistry,
    par: Parallelism,
) -> Engine {
    let dir = Kindex::open(kx)
        .unwrap()
        .project(&ProjectName::new("p").unwrap())
        .unwrap();
    let roots = BTreeMap::from([("r".to_string(), work.to_path_buf())]);
    let mut e = Engine::open(dir, reg, roots, Instant::now() + Duration::from_secs(10)).unwrap();
    e.set_parallelism(par);
    e
}

/// The cache as raw rows: every graph table, then the lexicon and shard-hash tables.
pub type CacheDump = Vec<(String, Vec<(Vec<u8>, Vec<u8>)>)>;

pub fn cache_dump(e: &Engine) -> CacheDump {
    use katalogr_store::Reader;
    use katalogr_store::cache::TableId;
    let r = e.cache().read().unwrap();
    let mut out: CacheDump = r
        .dump_graph()
        .unwrap()
        .into_iter()
        .map(|(n, rows)| (n.to_string(), rows))
        .collect();
    for (name, t) in [
        ("postings", TableId::Postings),
        ("doc_len", TableId::DocLen),
        ("lexicon_stats", TableId::LexiconStats),
        ("shard_hashes", TableId::ShardHashes),
        ("rank", TableId::Rank),
    ] {
        out.push((name.to_string(), r.raw_scan(t, &[]).unwrap()));
    }
    out
}

/// The raw dictionary tables. Their ids follow the order rows were written, so two caches built
/// by different histories number them differently; runs of the same input at different thread
/// counts must agree on them byte for byte.
pub fn dictionary_dump(e: &Engine) -> CacheDump {
    use katalogr_store::Reader;
    use katalogr_store::cache::TableId;
    let r = e.cache().read().unwrap();
    [
        ("vocab", TableId::Vocab),
        ("vocab_ix", TableId::VocabIx),
        ("prov", TableId::Prov),
        ("prov_ix", TableId::ProvIx),
    ]
    .into_iter()
    .map(|(n, t)| (n.to_string(), r.raw_scan(t, &[]).unwrap()))
    .collect()
}

/// Every file under the project's `shards/` as `(relative path, bytes)`.
pub fn shard_bytes(kx: &Utf8Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().to_string();
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let root = kx.join("projects/p/shards");
    let mut out = BTreeMap::new();
    walk(root.as_std_path(), root.as_std_path(), &mut out);
    out
}

/// Handles `*<suffix>`: sleeps `delay`, records the name of the extracting thread, then
/// extracts like [`TestExtractor`].
pub struct TestProbe {
    pub id: &'static str,
    pub suffix: &'static str,
    pub delay: Duration,
    pub seen: Arc<std::sync::Mutex<std::collections::BTreeSet<String>>>,
}

impl Extractor for TestProbe {
    fn id(&self) -> &'static str {
        self.id
    }

    fn handles(&self, path: &RelPath) -> bool {
        path.as_str().ends_with(self.suffix)
    }

    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        let name = std::thread::current().name().unwrap_or("").to_string();
        self.seen.lock().unwrap().insert(name);
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        let mut f = TestExtractor.extract(input)?;
        f.extension = self.id.to_string();
        Ok(f)
    }
}

/// The sleeping test extractor `slow`: handles `*.slow`, sleeps 20 ms per file.
pub fn test_slow() -> TestProbe {
    TestProbe {
        id: "slow",
        suffix: ".slow",
        delay: Duration::from_millis(20),
        seen: Arc::default(),
    }
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
