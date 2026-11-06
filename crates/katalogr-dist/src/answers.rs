//! Stored answers (FEAT-096, ADR-0037): the whole-graph commands read their report from
//! `<project>/.cache/answers` when an entry for the same view ID and validity exists, and write
//! it when not. A stored answer is only ever served at a clean, exact position, so a hit is
//! byte-for-byte what a fresh computation would give.

use crate::{Consistency, CoverageOptions, DistError, Katalogr, Options, Outcome, Reply, Report};
use camino::Utf8PathBuf;
use katalogr_analytics::{
    CommunityOptions, CycleOptions, DeadCodeOptions, DeadConfidence, GraphOptions, HubOptions,
    Level,
};
use katalogr_engine::{Engine, QueryOptions, Scope};
use katalogr_query::ViewQuery;
use katalogr_store::{
    AnswerStore, DirAnswerStore, ProjectDir, PutOutcome, ReadTx, Reader, StoreError, StoredAnswer,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

/// Default of the project setting `answers.max_mib`; 0 turns the store off.
pub const DEFAULT_MAX_MIB: u64 = 256;
const MAX_MIB_KEY: &str = "answers.max_mib";

/// The entry format in `validity`; the store's own header format is separate.
pub const ANSWER_FORMAT: u32 = katalogr_store::ANSWER_FORMAT;

/// A question this store knows how to run again. Options hold resolved values, settings applied.
#[derive(Clone, Debug)]
pub enum StoredQuery {
    Hubs(HubOptions),
    Communities(CommunityOptions),
    Cycles(CycleOptions),
    DeadCode(DeadCodeOptions),
    Map,
    Coverage(CoverageOptions),
}

fn json_str<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("a plain value serialises")
}

fn kebab<T: serde::Serialize>(v: &T) -> String {
    json_str(v).trim_matches('"').to_string()
}

fn from_kebab<T: serde::de::DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_str(&json_str(&s)).ok()
}

/// A sorted list as a compact JSON array.
fn list(items: &[String]) -> String {
    let mut v = items.to_vec();
    v.sort();
    json_str(&v)
}

fn graph_args(g: &GraphOptions, a: &mut BTreeMap<String, String>) {
    a.insert("level".into(), kebab(&g.level));
    a.insert("include_inferred".into(), g.include_inferred.to_string());
    a.insert("exclude_kinds".into(), list(&g.exclude_kinds));
}

fn graph_from(a: &BTreeMap<String, String>) -> Option<GraphOptions> {
    let level: Level = from_kebab(a.get("level")?)?;
    let mut g = GraphOptions::new(level, a.get("include_inferred")?.parse().ok()?);
    g.exclude_kinds = serde_json::from_str(a.get("exclude_kinds")?).ok()?;
    Some(g)
}

fn num<T: std::str::FromStr>(a: &BTreeMap<String, String>, k: &str) -> Option<T> {
    a.get(k)?.parse().ok()
}

impl StoredQuery {
    /// The table under Data model. Never holds a budget or output flag. Lists are sorted and held
    /// as a compact JSON array; floats use Rust's shortest `{}` form.
    pub fn to_view_query(&self) -> ViewQuery {
        let mut a = BTreeMap::new();
        let op = match self {
            StoredQuery::Hubs(o) => {
                graph_args(&o.graph, &mut a);
                a.insert("min_degree".into(), format!("{}", o.min_degree));
                a.insert("limit".into(), o.limit.to_string());
                "hubs"
            }
            StoredQuery::Communities(o) => {
                graph_args(&o.graph, &mut a);
                a.insert("seed".into(), o.seed.to_string());
                a.insert("resolution".into(), format!("{}", o.resolution));
                a.insert("min_size".into(), o.min_size.to_string());
                a.insert("members".into(), o.members.to_string());
                a.insert("limit".into(), o.limit.to_string());
                if let Some(of) = &o.of {
                    a.insert("of".into(), of.clone());
                }
                "communities"
            }
            StoredQuery::Cycles(o) => {
                graph_args(&o.graph, &mut a);
                a.insert("members".into(), o.members.to_string());
                a.insert("limit".into(), o.limit.to_string());
                "cycles"
            }
            StoredQuery::DeadCode(o) => {
                a.insert("exclude_kinds".into(), list(&o.exclude_kinds));
                a.insert("internal".into(), o.internal.to_string());
                a.insert("min_confidence".into(), kebab(&o.min_confidence));
                if let Some(p) = &o.path_prefix {
                    a.insert("path_prefix".into(), p.clone());
                }
                a.insert("limit".into(), o.limit.to_string());
                "dead-code"
            }
            StoredQuery::Map => "map",
            StoredQuery::Coverage(o) => {
                if let Some(r) = &o.repo {
                    a.insert("repo".into(), r.clone());
                }
                a.insert("suffix_top".into(), o.suffix_top.to_string());
                "coverage"
            }
        };
        ViewQuery {
            op: op.to_string(),
            args: a,
        }
    }

    /// The inverse. `None` for an unknown op or a bad value. A dead-code query comes back with
    /// no `code_kinds`: the registry supplies them.
    pub fn from_view_query(q: &ViewQuery) -> Option<StoredQuery> {
        let a = &q.args;
        Some(match q.op.as_str() {
            "hubs" => StoredQuery::Hubs(HubOptions {
                graph: graph_from(a)?,
                min_degree: num(a, "min_degree")?,
                limit: num(a, "limit")?,
            }),
            "communities" => StoredQuery::Communities(CommunityOptions {
                graph: graph_from(a)?,
                seed: num(a, "seed")?,
                resolution: num(a, "resolution")?,
                min_size: num(a, "min_size")?,
                members: num(a, "members")?,
                limit: num(a, "limit")?,
                of: a.get("of").cloned(),
            }),
            "cycles" => StoredQuery::Cycles(CycleOptions {
                graph: graph_from(a)?,
                members: num(a, "members")?,
                limit: num(a, "limit")?,
            }),
            "dead-code" => StoredQuery::DeadCode(DeadCodeOptions {
                code_kinds: BTreeMap::<String, BTreeSet<String>>::new(),
                exclude_kinds: serde_json::from_str(a.get("exclude_kinds")?).ok()?,
                internal: num(a, "internal")?,
                min_confidence: from_kebab::<DeadConfidence>(a.get("min_confidence")?)?,
                path_prefix: a.get("path_prefix").cloned(),
                limit: num(a, "limit")?,
            }),
            "map" => StoredQuery::Map,
            "coverage" => StoredQuery::Coverage(CoverageOptions {
                repo: a.get("repo").cloned(),
                suffix_top: num(a, "suffix_top")?,
            }),
            _ => return None,
        })
    }
}

/// Hex SHA-256 of compact JSON, keys sorted: `{"answer_format":1,"build":…,"inputs":…}`.
pub fn validity(build: &str, inputs: &BTreeMap<String, String>) -> String {
    let text = format!(
        "{{\"answer_format\":{ANSWER_FORMAT},\"build\":{},\"inputs\":{}}}",
        json_str(&build),
        json_str(inputs)
    );
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `<project_root>/.cache/answers`, shared by all contexts of the project.
pub(crate) fn answer_store_dir(p: &ProjectDir) -> Utf8PathBuf {
    p.project_root().join(".cache").join("answers")
}

/// Hex SHA-256 of `<repo>\n<path>\n` lines, for each repo in name order and `scan::list_files`
/// order. A repo with no known root adds `<repo>\n-\n`.
pub(crate) fn listing_digest(
    engine: &Engine,
    repos: &[String],
) -> Result<String, katalogr_engine::EngineError> {
    let mut names: Vec<&String> = repos.iter().collect();
    names.sort();
    let mut h = Sha256::new();
    for repo in names {
        let Some(root) = engine.roots().get(repo.as_str()) else {
            h.update(repo.as_bytes());
            h.update(b"\n-\n");
            continue;
        };
        let (include, exclude) = engine.repo_filters(repo);
        let (files, _) =
            katalogr_engine::scan::list_files_warned(root.as_std_path(), &include, &exclude)?;
        for f in files {
            h.update(repo.as_bytes());
            h.update(b"\n");
            h.update(f.as_str().as_bytes());
            h.update(b"\n");
        }
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// What one read through the store needs to know.
pub struct AnswerRequest<'a> {
    pub view_id: String,
    pub op: &'a str,
    pub args: BTreeMap<String, String>,
    /// Each name and value enters `validity`.
    pub inputs: BTreeMap<String, String>,
    /// False when a position is dirty: read nothing and write nothing.
    pub storable: bool,
}

/// Counts for one facade since it was made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnswerCounters {
    pub hits: u64,
    pub misses: u64,
    pub writes: u64,
    pub unusable: u64,
}

/// The facade's part of the store: the build, an optional replacement store, the counts.
#[derive(Default)]
pub(crate) struct AnswerState {
    build: Option<String>,
    store: Option<Arc<dyn AnswerStore>>,
    counters: Mutex<AnswerCounters>,
}

/// What a caller adds to a store read once it holds the snapshot: values that enter `validity`
/// (`inputs`) and arguments that enter the view ID (`args`).
#[derive(Default)]
pub(crate) struct Extra {
    pub inputs: BTreeMap<String, String>,
    pub args: BTreeMap<String, String>,
}

impl Katalogr {
    /// The clean build identity, or `None` to turn the store off. Default `None`.
    pub fn set_answer_build(&mut self, build: Option<String>) {
        self.answers.build = build;
    }

    /// Test support: replace the store.
    #[doc(hidden)]
    pub fn set_answer_store(&mut self, s: Arc<dyn AnswerStore>) {
        self.answers.store = Some(s);
    }

    /// Counts for this facade since it was made.
    pub fn answer_counters(&self) -> AnswerCounters {
        *self.answers.counters.lock().expect("answer counters")
    }

    fn count(&self, f: impl FnOnce(&mut AnswerCounters)) {
        f(&mut self.answers.counters.lock().expect("answer counters"));
    }

    pub(crate) fn answer_store(&self) -> Arc<dyn AnswerStore> {
        self.answers
            .store
            .clone()
            .unwrap_or_else(|| Arc::new(DirAnswerStore::new(answer_store_dir(&self.project))))
    }

    /// `answers.max_mib`: the project setting, else [`DEFAULT_MAX_MIB`]. 0 turns the store off.
    pub(crate) fn answers_max_mib(&self) -> u64 {
        self.project
            .config()
            .settings
            .get(MAX_MIB_KEY)
            .and_then(serde_yaml_ng::Value::as_u64)
            .unwrap_or(DEFAULT_MAX_MIB)
    }

    /// Serve from the store or compute and write. Errors of the store become warnings.
    pub fn through_store<T: Serialize + DeserializeOwned>(
        &self,
        r: &AnswerRequest,
        warnings: &mut Vec<String>,
        compute: impl FnOnce() -> Result<Outcome<T>, StoreError>,
        fresh: bool,
    ) -> Result<Outcome<T>, StoreError> {
        let max_mib = self.answers_max_mib();
        let Some(build) = self.answers.build.as_deref() else {
            return compute();
        };
        if !r.storable || max_mib == 0 {
            return compute();
        }
        let validity = validity(build, &r.inputs);
        let store = self.answer_store();
        let mut warned = false;
        let mut warn = |e: &StoreError, warnings: &mut Vec<String>| {
            if !warned {
                warned = true;
                warnings.push(format!("stored answers: {e}"));
            }
        };
        if !fresh {
            match store.get(&r.view_id) {
                Ok(Some(a)) if a.header.validity == validity => {
                    match serde_json::from_slice::<T>(&a.payload) {
                        Ok(v) => {
                            self.count(|c| c.hits += 1);
                            return Ok(Outcome::Found(v));
                        }
                        Err(_) => {
                            let _ = store.remove(&r.view_id);
                            self.count(|c| c.unusable += 1);
                        }
                    }
                }
                Ok(_) => self.count(|c| c.misses += 1),
                Err(e) => {
                    self.count(|c| c.misses += 1);
                    warn(&e, warnings);
                }
            }
        }
        let out = compute()?;
        if let Outcome::Found(v) = &out {
            match serde_json::to_vec(v) {
                Ok(payload) => {
                    let entry = StoredAnswer::new(
                        r.view_id.clone(),
                        validity,
                        r.op.to_string(),
                        r.args.clone(),
                        payload,
                    );
                    match store.put(&entry) {
                        Ok(PutOutcome::Stored) => {
                            self.count(|c| c.writes += 1);
                            if let Err(e) = store.prune(max_mib.saturating_mul(1 << 20)) {
                                warn(&e, warnings);
                            }
                        }
                        Ok(PutOutcome::TooLarge) => {}
                        Err(e) => warn(&e, warnings),
                    }
                }
                Err(e) => warn(&StoreError::Row(e.to_string()), warnings),
            }
        }
        Ok(out)
    }

    /// Reply for a command: opens the state read, builds the request, calls `through_store`.
    /// `extra` runs on the snapshot and returns what joins the key and validity, or `None` when
    /// this answer must not be stored. `f` computes the answer.
    pub(crate) fn query_stored<T: Serialize + DeserializeOwned>(
        &self,
        engine: &Engine,
        o: &Options,
        q: &StoredQuery,
        extra: impl FnOnce(&ReadTx) -> Option<Extra>,
        f: impl FnOnce(&ReadTx) -> Result<Outcome<T>, StoreError>,
    ) -> Result<Reply<T>, DistError> {
        let timeout = o.timeout()?;
        let consistency = o.consistency.unwrap_or(Consistency::Wait);
        let key_query = q.to_view_query();
        // The envelope's ID follows the question's own arguments, plus the budget when one was
        // given: the budget cuts the answer shown, not the answer stored.
        let envelope_view = o.view.as_ref().map(|v| {
            let mut e = key_query.clone();
            if let Some(b) = v.args.get("budget") {
                e.args.insert("budget".to_string(), b.clone());
            }
            e
        });
        let usable = self.answers.build.is_some() && self.answers_max_mib() > 0;
        let mut store_warnings = Vec::new();
        let a = engine.with_consistency_state(
            &QueryOptions {
                consistency: consistency.into(),
                timeout,
                scope: Scope::Project,
            },
            |tx, st| {
                if !st.exact || !usable {
                    return f(tx);
                }
                let Some(ex) = extra(tx) else { return f(tx) };
                let Ok(facts) = engine.facts_version(tx) else {
                    return f(tx);
                };
                let Ok(index) = tx.index_digest() else {
                    return f(tx);
                };
                let mut key = key_query.clone();
                key.args.extend(ex.args);
                let mut inputs = ex.inputs;
                inputs.insert("index".to_string(), index);
                let req = AnswerRequest {
                    view_id: katalogr_query::view_id(&st.positions, &facts, &key),
                    op: &key.op,
                    args: key.args.clone(),
                    inputs,
                    storable: st.positions.iter().all(|p| p.dirty.is_none()),
                };
                self.through_store(&req, &mut store_warnings, || f(tx), o.fresh)
            },
        )?;
        let mut warnings = a.warnings;
        warnings.extend(store_warnings);
        let mut reply = Reply {
            outcome: a.value?,
            partial: Vec::new(),
            folded: Vec::new(),
            report: Report {
                watermark: a.watermark.0,
                pending: a.pending,
                stale: a.stale,
                timed_out: a.stale && consistency != Consistency::StaleOk,
                queued: 0,
                warnings,
                positions: a.positions,
                exact: a.exact,
                view_id: None,
                context: None,
                working_set: None,
            },
            diagnostic: None,
        };
        self.finish_report(engine, &mut reply.report, envelope_view.as_ref());
        Ok(reply)
    }
}
