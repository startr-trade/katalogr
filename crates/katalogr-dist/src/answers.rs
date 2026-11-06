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
use schemars::JsonSchema;
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
    /// A diff report between two sets of positions (FEAT-095).
    Diff(DiffQuery),
}

/// A diff report's question: the repo, both sides' positions, the list limit and the format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffQuery {
    pub repo: String,
    pub a: Vec<katalogr_query::PositionRef>,
    pub b: Vec<katalogr_query::PositionRef>,
    pub limit: u64,
    pub render: crate::diff::DiffRender,
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
            StoredQuery::Diff(d) => {
                a.insert("repo".into(), d.repo.clone());
                a.insert("a".into(), json_str(&d.a));
                a.insert("b".into(), json_str(&d.b));
                a.insert("limit".into(), d.limit.to_string());
                a.insert("render".into(), d.render.name().to_string());
                a.insert(
                    "report_format".into(),
                    crate::diff::REPORT_FORMAT.to_string(),
                );
                "diff"
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
            "diff" => {
                if num::<u32>(a, "report_format")? != crate::diff::REPORT_FORMAT {
                    return None;
                }
                StoredQuery::Diff(DiffQuery {
                    repo: a.get("repo")?.clone(),
                    a: serde_json::from_str(a.get("a")?).ok()?,
                    b: serde_json::from_str(a.get("b")?).ok()?,
                    limit: num(a, "limit")?,
                    render: crate::diff::DiffRender::from_name(a.get("render")?)?,
                })
            }
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
        let Some(source) = engine.source(repo.as_str()) else {
            h.update(repo.as_bytes());
            h.update(b"\n-\n");
            continue;
        };
        let (include, exclude) = engine.repo_filters(repo);
        for f in source.list(&include, &exclude)? {
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
    /// Test support: runs just before an answer is computed.
    compute_hook: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl AnswerState {
    /// The same build, store and test hook, with counts starting at zero.
    pub(crate) fn fork(&self) -> AnswerState {
        AnswerState {
            build: self.build.clone(),
            store: self.store.clone(),
            counters: Mutex::new(AnswerCounters::default()),
            compute_hook: self.compute_hook.clone(),
        }
    }
}

/// An entry computed by a read, written once the read is known to have held.
pub(crate) struct PendingWrite {
    entry: StoredAnswer,
    max_mib: u64,
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

    /// Test support: run `hook` just before a stored-answer read computes its value.
    #[doc(hidden)]
    pub fn set_answer_compute_hook(&mut self, hook: Arc<dyn Fn() + Send + Sync>) {
        self.answers.compute_hook = Some(hook);
    }

    /// The clean build identity, if one is set.
    pub(crate) fn answers_build(&self) -> Option<&str> {
        self.answers.build.as_deref()
    }

    /// Runs the test hook, if one is set.
    pub(crate) fn run_compute_hook(&self) {
        if let Some(h) = &self.answers.compute_hook {
            h();
        }
    }

    /// Counts for this facade since it was made.
    pub fn answer_counters(&self) -> AnswerCounters {
        *self.answers.counters.lock().expect("answer counters")
    }

    fn count(&self, f: impl FnOnce(&mut AnswerCounters)) {
        f(&mut self.answers.counters.lock().expect("answer counters"));
    }

    /// Whether stored answers are on: a clean build identity is set and `answers.max_mib` is
    /// not 0.
    pub(crate) fn answers_enabled(&self) -> bool {
        self.answers.build.is_some() && self.answers_max_mib() > 0
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
        let (out, pending) = self.read_through(r, warnings, compute, fresh)?;
        if let Some(p) = pending {
            self.commit_write(p, warnings);
        }
        Ok(out)
    }

    /// Adds the store's warning once per answer.
    fn store_warning(e: &StoreError, warnings: &mut Vec<String>) {
        if !warnings.iter().any(|w| w.starts_with("stored answers:")) {
            warnings.push(format!("stored answers: {e}"));
        }
    }

    /// Writes an entry the read computed, then prunes. Errors become warnings.
    pub(crate) fn commit_write(&self, p: PendingWrite, warnings: &mut Vec<String>) {
        let store = self.answer_store();
        match store.put(&p.entry) {
            Ok(PutOutcome::Stored) => {
                self.count(|c| c.writes += 1);
                if let Err(e) = store.prune(p.max_mib.saturating_mul(1 << 20)) {
                    Self::store_warning(&e, warnings);
                }
            }
            Ok(PutOutcome::TooLarge) => {}
            Err(e) => Self::store_warning(&e, warnings),
        }
    }

    /// The read half of [`Katalogr::through_store`]: a hit, or the computed answer with the entry
    /// that should be written once the caller knows the answer held at its position.
    pub(crate) fn read_through<T: Serialize + DeserializeOwned>(
        &self,
        r: &AnswerRequest,
        warnings: &mut Vec<String>,
        compute: impl FnOnce() -> Result<Outcome<T>, StoreError>,
        fresh: bool,
    ) -> Result<(Outcome<T>, Option<PendingWrite>), StoreError> {
        let max_mib = self.answers_max_mib();
        let Some(build) = self.answers.build.as_deref() else {
            return Ok((compute()?, None));
        };
        if !r.storable || max_mib == 0 {
            return Ok((compute()?, None));
        }
        let validity = validity(build, &r.inputs);
        let store = self.answer_store();
        if !fresh {
            match store.get(&r.view_id) {
                Ok(Some(a)) if a.header.validity == validity => {
                    match serde_json::from_slice::<T>(&a.payload) {
                        Ok(v) => {
                            self.count(|c| c.hits += 1);
                            return Ok((Outcome::Found(v), None));
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
                    Self::store_warning(&e, warnings);
                }
            }
        }
        let out = compute()?;
        let mut pending = None;
        if let Outcome::Found(v) = &out {
            match serde_json::to_vec(v) {
                Ok(payload) => {
                    pending = Some(PendingWrite {
                        entry: StoredAnswer::new(
                            r.view_id.clone(),
                            validity,
                            r.op.to_string(),
                            r.args.clone(),
                            payload,
                        ),
                        max_mib,
                    });
                }
                Err(e) => Self::store_warning(&StoreError::Row(e.to_string()), warnings),
            }
        }
        Ok((out, pending))
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
        let usable = self.answers_enabled();
        let mut store_warnings = Vec::new();
        // The entry the read computed, and the positions its ID was computed from.
        let mut deferred: Option<(PendingWrite, Vec<katalogr_query::PositionRef>)> = None;
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
                let run = || {
                    if let Some(h) = &self.answers.compute_hook {
                        h();
                    }
                    f(tx)
                };
                let (out, pending) = self.read_through(&req, &mut store_warnings, run, o.fresh)?;
                deferred = pending.map(|p| (p, st.positions.clone()));
                Ok(out)
            },
        )?;
        // Store only an answer that held: exact once the value was computed, at the very
        // positions its ID was made from. Otherwise it is returned, not stored.
        if let Some((p, positions)) = deferred
            && a.exact
            && a.positions == positions
        {
            self.commit_write(p, &mut store_warnings);
        }
        let mut warnings = a.warnings;
        warnings.extend(store_warnings);
        let mut reply = Reply {
            workspace: None,
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

// ---------------------------------------------------------------- check, stats, prune

/// A stored entry that differs from a fresh computation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AnswerMismatch {
    pub view_id: String,
    pub op: String,
    /// Byte offset of the first difference.
    pub first_difference: u64,
}

/// What `answers check` found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AnswerCheckReport {
    pub schema: u32,
    /// False when the checkout's position could not be held for the check.
    pub exact: bool,
    pub checked: u64,
    pub matched: u64,
    pub mismatched: Vec<AnswerMismatch>,
    pub skipped_other_position: u64,
    pub skipped_other_validity: u64,
    pub skipped_unknown_op: u64,
    pub removed: u64,
}

/// What `answers stats` found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AnswerStats {
    pub schema: u32,
    pub entries: u64,
    pub bytes: u64,
    pub by_op: BTreeMap<String, u64>,
    pub max_mib: u64,
}

/// `schema` of both reports.
pub const ANSWERS_SCHEMA: u32 = 1;

pub(crate) fn first_difference(a: &[u8], b: &[u8]) -> u64 {
    a.iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len())) as u64
}

impl Katalogr {
    /// The fresh payload bytes of a stored query, for `answers_check`. `None` when the question
    /// has no answer to compare (no match).
    fn fresh_payload(
        &self,
        engine: &Engine,
        tx: &ReadTx,
        q: &StoredQuery,
    ) -> Result<Option<Vec<u8>>, DistError> {
        let bytes = |v: &dyn erased::Ser| v.to_bytes();
        Ok(Some(match q {
            StoredQuery::Hubs(o) => bytes(&katalogr_analytics::hubs(tx, o)?),
            StoredQuery::Communities(o) => bytes(&katalogr_analytics::communities(tx, o)?),
            StoredQuery::Cycles(o) => bytes(&katalogr_analytics::cycles(tx, o)?),
            StoredQuery::DeadCode(o) => {
                let mut o = o.clone();
                o.code_kinds = crate::registry_code_kinds();
                bytes(&katalogr_analytics::dead_code(tx, &o)?)
            }
            StoredQuery::Map => bytes(&katalogr_query::map_boosted_traced(
                tx,
                None,
                &mut katalogr_query::Diagnostic::off(),
            )?),
            StoredQuery::Coverage(o) => {
                let parts = self.coverage_parts(engine, o)?;
                bytes(&crate::coverage_run(engine, o, &parts, tx)?)
            }
            // Checked against a fresh build by the report's own check.
            StoredQuery::Diff(_) => return Ok(None),
        }))
    }

    /// Compare every stored entry that belongs to this position and build with a fresh
    /// computation. `repair` removes the entries that differ.
    pub fn answers_check(
        &self,
        limit: Option<usize>,
        repair: bool,
        o: &Options,
    ) -> Result<AnswerCheckReport, DistError> {
        let timeout = o.timeout()?;
        let consistency = o.consistency.unwrap_or(Consistency::Wait);
        let engine = self.engine_for(consistency, std::time::Instant::now() + timeout)?;
        let store = self.answer_store();
        let build = self.answers.build.clone();
        let a = engine.with_consistency_state(
            &QueryOptions {
                consistency: consistency.into(),
                timeout,
                scope: Scope::Project,
            },
            |tx, st| -> Result<AnswerCheckReport, DistError> {
                let mut rep = AnswerCheckReport {
                    schema: ANSWERS_SCHEMA,
                    exact: st.exact,
                    checked: 0,
                    matched: 0,
                    mismatched: Vec::new(),
                    skipped_other_position: 0,
                    skipped_other_validity: 0,
                    skipped_unknown_op: 0,
                    removed: 0,
                };
                if !st.exact {
                    return Ok(rep);
                }
                let facts = engine.facts_version(tx)?;
                let index = tx.index_digest()?;
                let mut listings = store.list()?;
                listings.sort_by(|a, b| a.header.view_id.cmp(&b.header.view_id));
                for l in listings.into_iter().take(limit.unwrap_or(usize::MAX)) {
                    let h = &l.header;
                    let vq = ViewQuery {
                        op: h.op.clone(),
                        args: h.args.clone(),
                    };
                    let known = StoredQuery::from_view_query(&vq)
                        // A map read under a working-set boost cannot be recomputed here.
                        .filter(|q| q.to_view_query() == vq);
                    let Some(q) = known else {
                        rep.skipped_unknown_op += 1;
                        continue;
                    };
                    if let StoredQuery::Diff(dq) = &q {
                        // A report is keyed on its own commits, not on this checkout's position.
                        let Some(stored) = store.get(&h.view_id)? else {
                            continue;
                        };
                        match self.check_diff(
                            dq,
                            &h.view_id,
                            &h.validity,
                            &facts,
                            &stored.payload,
                            o,
                        )? {
                            crate::diff::CheckOne::OtherPosition => rep.skipped_other_position += 1,
                            crate::diff::CheckOne::OtherValidity => rep.skipped_other_validity += 1,
                            crate::diff::CheckOne::Checked(None) => {
                                rep.checked += 1;
                                rep.matched += 1;
                            }
                            crate::diff::CheckOne::Checked(Some(first)) => {
                                rep.checked += 1;
                                rep.mismatched.push(AnswerMismatch {
                                    view_id: h.view_id.clone(),
                                    op: h.op.clone(),
                                    first_difference: first,
                                });
                                if repair {
                                    store.remove(&h.view_id)?;
                                    rep.removed += 1;
                                }
                            }
                        }
                        continue;
                    }
                    if katalogr_query::view_id(&st.positions, &facts, &vq) != h.view_id {
                        rep.skipped_other_position += 1;
                        continue;
                    }
                    let mut inputs = BTreeMap::from([("index".to_string(), index.clone())]);
                    if let StoredQuery::Coverage(c) = &q {
                        let named: Vec<String> = self
                            .project
                            .config()
                            .repos
                            .iter()
                            .filter(|r| c.repo.as_deref().is_none_or(|w| w == r.name))
                            .map(|r| r.name.clone())
                            .collect();
                        inputs.insert("listing".into(), listing_digest(&engine, &named)?);
                    }
                    let same = build
                        .as_deref()
                        .is_some_and(|b| validity(b, &inputs) == h.validity);
                    if !same {
                        rep.skipped_other_validity += 1;
                        continue;
                    }
                    let Some(stored) = store.get(&h.view_id)? else {
                        continue;
                    };
                    let Some(fresh) = self.fresh_payload(&engine, tx, &q)? else {
                        continue;
                    };
                    rep.checked += 1;
                    if fresh == stored.payload {
                        rep.matched += 1;
                    } else {
                        rep.mismatched.push(AnswerMismatch {
                            view_id: h.view_id.clone(),
                            op: h.op.clone(),
                            first_difference: first_difference(&fresh, &stored.payload),
                        });
                        if repair {
                            store.remove(&h.view_id)?;
                            rep.removed += 1;
                        }
                    }
                }
                Ok(rep)
            },
        )?;
        a.value
    }

    /// How many entries, how many bytes, by op.
    pub fn answers_stats(&self) -> Result<AnswerStats, DistError> {
        let mut stats = AnswerStats {
            schema: ANSWERS_SCHEMA,
            entries: 0,
            bytes: 0,
            by_op: BTreeMap::new(),
            max_mib: self.answers_max_mib(),
        };
        for l in self.answer_store().list()? {
            stats.entries += 1;
            stats.bytes += l.bytes;
            *stats.by_op.entry(l.header.op).or_insert(0) += 1;
        }
        Ok(stats)
    }

    /// Remove the oldest entries until at most `max_mib` remain; `max_mib` defaults to the
    /// setting.
    pub fn answers_prune(
        &self,
        max_mib: Option<u64>,
    ) -> Result<katalogr_store::PruneReport, DistError> {
        let mib = max_mib.unwrap_or_else(|| self.answers_max_mib());
        Ok(self.answer_store().prune(mib.saturating_mul(1 << 20))?)
    }
}

/// Serialising behind a trait object, so `fresh_payload` can turn any report into bytes.
mod erased {
    pub trait Ser {
        fn to_bytes(&self) -> Vec<u8>;
    }
    impl<T: serde::Serialize> Ser for T {
        fn to_bytes(&self) -> Vec<u8> {
            serde_json::to_vec(self).expect("a report serialises")
        }
    }
}

/// The JSON Schema of an [`AnswerCheckReport`].
pub fn check_schema() -> serde_json::Value {
    schemars::schema_for!(AnswerCheckReport).to_value()
}

/// The JSON Schema of an [`AnswerStats`].
pub fn stats_schema() -> serde_json::Value {
    schemars::schema_for!(AnswerStats).to_value()
}
