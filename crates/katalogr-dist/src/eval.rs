//! Retrieval evaluation runner: indexes fixture golden sets (or uses a project's live index),
//! answers every golden query through the facade and scores the answers (FEAT-061).

use crate::error::DistError;
use crate::{
    Consistency, Katalogr, Options, Outcome, Overrides, kindex_init, on_target, project_init,
};
pub use katalogr_analytics::eval::*;
use katalogr_analytics::metrics::{Better, MetricSample, metric_key};
use katalogr_engine::Engine;
use katalogr_model::{AnyName, Name, parse_name};
use katalogr_query::Serialize;
use katalogr_query::budget::estimate_tokens;
use katalogr_query::{Explain, Fold, Folded, RefHit};
use katalogr_store::{Reader, StoreError};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

impl From<EvalError> for DistError {
    fn from(e: EvalError) -> Self {
        match e {
            EvalError::Format { .. } | EvalError::Usage(_) => DistError::Usage(e.to_string()),
            EvalError::Io { file, message } => DistError::Io {
                path: file,
                message,
            },
        }
    }
}

/// The largest k a run accepts.
pub const MAX_K: u32 = 50;

/// What to evaluate and how.
#[derive(Debug, Clone)]
pub struct EvalRequest {
    /// Golden files or directories (see `load_sets`). Fixture mode only.
    pub sets: Vec<PathBuf>,
    /// Default `[1, 5, 10]`.
    pub ks: Vec<u32>,
    pub params: EvalParams,
    /// Seconds to wait for each fixture project's index (default 120).
    pub index_timeout: f64,
}

impl Default for EvalRequest {
    fn default() -> Self {
        EvalRequest {
            sets: Vec::new(),
            ks: vec![1, 5, 10],
            params: EvalParams::default(),
            index_timeout: 120.0,
        }
    }
}

/// A fixture set indexed into a private temporary kindex; removed on drop.
pub struct FixtureProject {
    _tmp: tempfile::TempDir,
    pub k: Katalogr,
}

fn format_err(file: &Path, message: &str) -> DistError {
    EvalError::Format {
        file: file.display().to_string(),
        message: message.to_string(),
    }
    .into()
}

/// Index the repositories of `set` (relative to `dir`) and wait for the index.
fn index_set(
    set: &GoldenSet,
    dir: &Path,
    timeout: f64,
    file: &Path,
) -> Result<FixtureProject, DistError> {
    if set.repos.is_empty() {
        return Err(format_err(file, "repos: required in fixture mode"));
    }
    let tmp = tempfile::tempdir().map_err(|e| DistError::io("temporary directory", e))?;
    let kx = tmp.path().join("kx");
    kindex_init(&kx)?;
    let project = format!("eval-{}", set.set);
    let repos = set
        .repos
        .iter()
        .map(|r| (r.name.clone(), dir.join(&r.path)))
        .collect();
    project_init(&kx, &project, repos, false)?;
    let k = Katalogr::resolve(
        tmp.path(),
        &Overrides {
            kindex: Some(kx.display().to_string()),
            project: Some(project),
        },
    )?;
    let report = k.index(
        None,
        true,
        None,
        &Options {
            consistency: Some(Consistency::WaitAll),
            timeout: Some(timeout),
            diagnostic: false,
            expand: false,
        },
    )?;
    if report.timed_out {
        return Err(DistError::usage(format!(
            "fixture set {} did not finish indexing",
            set.set
        )));
    }
    Ok(FixtureProject { _tmp: tmp, k })
}

fn json_tokens<T: Serialize>(x: &T) -> u64 {
    estimate_tokens(&serde_json::to_string(x).unwrap_or_default())
}

/// The ranked items of a folded `refs` answer, as the default `refs` presents it: `hits` in
/// order, each fold's `first` inserted at its `at`; the fold's first item carries the fold's
/// JSON tokens, the others 0.
pub(crate) fn refs_items(f: &Folded<RefHit>) -> Vec<RankedItem> {
    let mut out = Vec::new();
    let mut folds = f.folds.iter().peekable();
    for (i, h) in f.hits.iter().enumerate() {
        while let Some(fold) = folds.next_if(|fold| fold.at as usize <= i) {
            push_fold(&mut out, fold);
        }
        out.push(RankedItem {
            name: h.from.name.clone(),
            path: Some(h.from.path.clone()),
            tokens: json_tokens(h),
        });
    }
    for fold in folds {
        push_fold(&mut out, fold);
    }
    out
}

fn push_fold(out: &mut Vec<RankedItem>, fold: &Fold) {
    let tokens = json_tokens(fold);
    for (n, h) in fold.first.iter().enumerate() {
        out.push(RankedItem {
            name: h.name.clone(),
            path: Some(h.path.clone()),
            tokens: if n == 0 { tokens } else { 0 },
        });
    }
}

/// The ranked items of an `explain` answer: outgoing links, then incoming, then rationale
/// targets, keeping the first occurrence of each name.
pub fn explain_items(e: &Explain) -> Vec<RankedItem> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for l in e.outgoing.iter().chain(&e.incoming) {
        if seen.insert(l.other.clone()) {
            out.push(RankedItem {
                name: l.other.clone(),
                path: None,
                tokens: json_tokens(l),
            });
        }
    }
    for name in &e.items {
        if seen.insert(name.clone()) {
            out.push(RankedItem {
                name: name.clone(),
                path: None,
                tokens: json_tokens(name),
            });
        }
    }
    out
}

/// Evidence names that are neither a symbol, an artifact nor a contract of the index.
fn missing(tx: &impl Reader, names: &[&str]) -> Result<Vec<String>, StoreError> {
    let mut out = Vec::new();
    for n in names {
        let known = match parse_name(n) {
            Ok(AnyName::Symbol(s)) => tx.symbol(s.id())?.is_some(),
            Ok(AnyName::Artifact(a)) => tx.artifact(a.id())?.is_some(),
            Ok(AnyName::Contract(c)) => tx.contract(c.id())?.is_some(),
            Err(_) => false,
        };
        if !known {
            out.push(n.to_string());
        }
    }
    Ok(out)
}

fn route_of<T>(o: &Outcome<T>) -> Route {
    match o {
        Outcome::Found(_) => Route::Found,
        Outcome::Ambiguous(_) => Route::Ambiguous,
        Outcome::NoMatch(_) | Outcome::StaleFile(_) => Route::NoMatch,
    }
}

/// Answer one golden query and record what came back. The reads are the facade's own
/// (`search_with`, `map`, `refs`, `explain`), run on `engine` so one engine serves every query.
pub fn run_query(
    k: &Katalogr,
    engine: &Engine,
    q: &GoldenQuery,
    p: &EvalParams,
    kmax: usize,
    o: &Options,
) -> Result<QueryRun, DistError> {
    let input = q.input.as_deref().unwrap_or("");
    let (route, items) = match q.op {
        Op::Search => {
            if katalogr_model::tokenize(input).is_empty() {
                return Err(DistError::usage(format!(
                    "{input:?}: no searchable word (words need two letters, or a digit)"
                )));
            }
            let r = k.query_on(engine, o, |tx| {
                let hits = katalogr_query::search_with(tx, input, kmax, &p.search)?;
                Ok(if hits.is_empty() {
                    Outcome::NoMatch(format!("nothing matches {input:?}"))
                } else {
                    Outcome::Found(hits)
                })
            })?;
            let items = match &r.outcome {
                Outcome::Found(hits) => hits
                    .iter()
                    .map(|h| RankedItem {
                        name: h.symbol.name.clone(),
                        path: Some(h.symbol.path.clone()),
                        tokens: json_tokens(h),
                    })
                    .collect(),
                _ => Vec::new(),
            };
            (route_of(&r.outcome), items)
        }
        Op::Map => {
            let r = k.query_on(engine, o, |tx| Ok(Outcome::Found(katalogr_query::map(tx)?)))?;
            let items = match &r.outcome {
                Outcome::Found(entries) => entries
                    .iter()
                    .take(kmax)
                    .map(|e| RankedItem {
                        name: e.symbol.name.clone(),
                        path: Some(e.symbol.path.clone()),
                        tokens: json_tokens(e),
                    })
                    .collect(),
                _ => Vec::new(),
            };
            (route_of(&r.outcome), items)
        }
        Op::Refs => {
            let r = k.query_on(engine, o, |tx| {
                on_target(tx, input, |t| {
                    katalogr_query::refs_folded_traced(
                        tx,
                        t,
                        Some(katalogr_query::FOLD_FROM),
                        &mut katalogr_query::Diagnostic::off(),
                    )
                })
            })?;
            let items = match &r.outcome {
                Outcome::Found(f) => refs_items(f),
                _ => Vec::new(),
            };
            (route_of(&r.outcome), items)
        }
        Op::Explain => {
            let r = k.query_on(engine, o, |tx| {
                on_target(tx, input, |t| katalogr_query::explain(tx, t, p.per_side))
            })?;
            let items = match &r.outcome {
                Outcome::Found(e) => explain_items(e),
                _ => Vec::new(),
            };
            (route_of(&r.outcome), items)
        }
    };
    let missing_evidence = if q.evidence.is_empty() {
        Vec::new()
    } else {
        let names: Vec<&str> = q.evidence.iter().map(|e| e.name.as_str()).collect();
        match k
            .query_on(engine, o, |tx| Ok(Outcome::Found(missing(tx, &names)?)))?
            .outcome
        {
            Outcome::Found(m) => m,
            _ => Vec::new(),
        }
    };
    Ok(QueryRun {
        route,
        items,
        missing_evidence,
    })
}

fn check_ks(ks: &[u32]) -> Result<Vec<u32>, DistError> {
    if ks.is_empty() {
        return Err(DistError::usage("--k: at least one value is required"));
    }
    if let Some(bad) = ks.iter().find(|k| !(1..=MAX_K).contains(*k)) {
        return Err(DistError::usage(format!(
            "--k: {bad} is outside 1..={MAX_K}"
        )));
    }
    let mut ks = ks.to_vec();
    ks.sort_unstable();
    ks.dedup();
    Ok(ks)
}

/// Run every query of every target, sets in the order given, queries in file order.
fn run_all(
    targets: &[(&GoldenSet, &Katalogr)],
    p: &EvalParams,
    ks: &[u32],
    o: &Options,
) -> Result<Vec<QueryResult>, DistError> {
    let kmax = ks.iter().copied().max().unwrap_or(10) as usize;
    let mut out = Vec::new();
    let timeout = Duration::try_from_secs_f64(o.timeout.unwrap_or(crate::DEFAULT_TIMEOUT_SECS))
        .map_err(|_| DistError::usage("invalid timeout"))?;
    for (set, k) in targets {
        let engine = k.engine(Instant::now() + timeout)?;
        for q in &set.queries {
            let run = run_query(k, &engine, q, p, kmax, o)?;
            out.push(score_query(&set.set, q, &run, ks));
        }
    }
    Ok(out)
}

fn build_report(
    mode: EvalMode,
    ks: &[u32],
    p: &EvalParams,
    sets: &[&GoldenSet],
    mut results: Vec<QueryResult>,
) -> EvalReport {
    results.sort_by(|a, b| (&a.set, &a.id).cmp(&(&b.set, &b.id)));
    let mut summaries: Vec<SetSummary> = sets
        .iter()
        .map(|s| SetSummary {
            set: s.set.clone(),
            version: s.version,
            queries: s.queries.len() as u64,
        })
        .collect();
    summaries.sort_by(|a, b| a.set.cmp(&b.set));
    EvalReport {
        format: BASELINE_FORMAT,
        mode,
        ks: ks.to_vec(),
        params: describe(p),
        sets: summaries,
        metrics: aggregate(&results, ks),
        queries: results,
        comparison: None,
    }
}

/// Load and index the fixture sets of a request, in set-name order.
fn prepare(req: &EvalRequest) -> Result<Vec<(GoldenSet, FixtureProject)>, DistError> {
    let mut loaded: Vec<(GoldenSet, PathBuf, PathBuf)> = Vec::new();
    for path in &req.sets {
        for (set, dir) in load_sets(path)? {
            let file = dir.join(GOLDEN_FILE);
            if loaded.iter().any(|(s, _, _)| s.set == set.set) {
                return Err(format_err(
                    &file,
                    &format!("set: duplicate set name {:?}", set.set),
                ));
            }
            loaded.push((set, dir, file));
        }
    }
    loaded.sort_by(|a, b| a.0.set.cmp(&b.0.set));
    loaded
        .into_iter()
        .map(|(set, dir, file)| {
            let fp = index_set(&set, &dir, req.index_timeout, &file)?;
            Ok((set, fp))
        })
        .collect()
}

fn fixture_options(req: &EvalRequest) -> Options {
    Options {
        consistency: Some(Consistency::WaitAll),
        timeout: Some(req.index_timeout),
        diagnostic: false,
        expand: false,
    }
}

/// Fixture mode: for each set, index its `repos` into a fresh temporary kindex and project,
/// wait for the index, run every query, score and aggregate. Temp dirs are removed on return.
pub fn run_fixtures(req: &EvalRequest) -> Result<EvalReport, DistError> {
    let ks = check_ks(&req.ks)?;
    let prepared = prepare(req)?;
    let targets: Vec<(&GoldenSet, &Katalogr)> = prepared.iter().map(|(s, f)| (s, &f.k)).collect();
    let results = run_all(&targets, &req.params, &ks, &fixture_options(req))?;
    let sets: Vec<&GoldenSet> = prepared.iter().map(|(s, _)| s).collect();
    Ok(build_report(
        EvalMode::Fixture,
        &ks,
        &req.params,
        &sets,
        results,
    ))
}

/// The overall samples (no labels) of `metrics`.
fn overall(metrics: &[MetricSample]) -> Vec<MetricSample> {
    metrics
        .iter()
        .filter(|s| s.labels.is_empty())
        .cloned()
        .collect()
}

fn sweep_over(
    targets: &[(&GoldenSet, &Katalogr)],
    base: &EvalParams,
    ks: &[u32],
    sweep_params: &[SweepParam],
    objective: &str,
    o: &Options,
) -> Result<SweepReport, DistError> {
    let combos = grid(base, sweep_params)?;
    let default = EvalParams::default();
    let mut rows: Vec<SweepRow> = Vec::new();
    for p in &combos {
        let results = run_all(targets, p, ks, o)?;
        let metrics = overall(&aggregate(&results, ks));
        rows.push(SweepRow {
            params: describe(p),
            is_default: *p == default,
            objective: 0.0,
            metrics,
        });
    }
    // The objective must be an overall sample present for every combination.
    fn find<'a>(row: &'a SweepRow, objective: &str) -> Option<&'a MetricSample> {
        row.metrics.iter().find(|s| metric_key(s) == objective)
    }
    let valid = |rows: &[SweepRow]| -> Vec<String> {
        let mut keys: BTreeSet<String> = rows
            .first()
            .map(|r| r.metrics.iter().map(metric_key).collect())
            .unwrap_or_default();
        for r in rows {
            let here: BTreeSet<String> = r.metrics.iter().map(metric_key).collect();
            keys = keys.intersection(&here).cloned().collect();
        }
        keys.into_iter().collect()
    };
    if rows.is_empty() || rows.iter().any(|r| find(r, objective).is_none()) {
        return Err(DistError::usage(format!(
            "unknown objective {objective:?}; valid: {}",
            valid(&rows).join(", ")
        )));
    }
    let better = find(&rows[0], objective).map_or(Better::Higher, |s| s.better);
    if better == Better::Info {
        return Err(DistError::usage(format!(
            "objective {objective:?} is informational; pick a quality metric"
        )));
    }
    for r in &mut rows {
        r.objective = find(r, objective).map_or(0.0, |s| s.value);
    }
    let text = |r: &SweepRow| serde_json::to_string(&r.params).unwrap_or_default();
    rows.sort_by(|a, b| {
        let by = if better == Better::Higher {
            b.objective.total_cmp(&a.objective)
        } else {
            a.objective.total_cmp(&b.objective)
        };
        by.then_with(|| text(a).cmp(&text(b)))
    });
    Ok(SweepReport {
        objective: objective.to_string(),
        better,
        combinations: combos.len() as u64,
        rows,
    })
}

/// Like `run_fixtures`, once per parameter combination; each set is indexed once.
pub fn sweep(
    req: &EvalRequest,
    grid: &[SweepParam],
    objective: &str,
) -> Result<SweepReport, DistError> {
    let ks = check_ks(&req.ks)?;
    let prepared = prepare(req)?;
    let targets: Vec<(&GoldenSet, &Katalogr)> = prepared.iter().map(|(s, f)| (s, &f.k)).collect();
    sweep_over(
        &targets,
        &req.params,
        &ks,
        grid,
        objective,
        &fixture_options(req),
    )
}

impl Katalogr {
    /// `<project dir>/eval/baseline.json`.
    pub fn eval_baseline_path(&self) -> PathBuf {
        self.project()
            .root()
            .join("eval")
            .join("baseline.json")
            .into_std_path_buf()
    }

    fn project_sets(&self) -> Result<Vec<GoldenSet>, DistError> {
        let dir = self.project().root().join("eval").into_std_path_buf();
        if !dir.is_dir() {
            return Err(DistError::usage(format!(
                "{}: no golden sets (project mode reads <project>/eval/*.yaml)",
                dir.display()
            )));
        }
        let mut out = Vec::new();
        for (set, d) in load_sets(&dir)? {
            if !set.repos.is_empty() {
                return Err(format_err(
                    &d.join(GOLDEN_FILE),
                    &format!("set {}: repos: forbidden in project mode", set.set),
                ));
            }
            out.push(set);
        }
        out.sort_by(|a, b| a.set.cmp(&b.set));
        Ok(out)
    }

    fn project_options(o: &Options) -> Options {
        Options {
            consistency: Some(o.consistency.unwrap_or(Consistency::WaitAll)),
            timeout: o.timeout,
            diagnostic: false,
            expand: false,
        }
    }

    /// Project mode: the sets in `<project dir>/eval/` (every `*.yaml`; a set with `repos` is a
    /// Format error) run against this project's live index (default consistency: wait-all).
    pub fn eval_project(
        &self,
        ks: &[u32],
        params: &EvalParams,
        o: &Options,
    ) -> Result<EvalReport, DistError> {
        let ks = check_ks(ks)?;
        let sets = self.project_sets()?;
        let targets: Vec<(&GoldenSet, &Katalogr)> = sets.iter().map(|s| (s, self)).collect();
        let results = run_all(&targets, params, &ks, &Self::project_options(o))?;
        let refs: Vec<&GoldenSet> = sets.iter().collect();
        Ok(build_report(EvalMode::Project, &ks, params, &refs, results))
    }

    /// Project mode sweep.
    pub fn eval_project_sweep(
        &self,
        ks: &[u32],
        base: &EvalParams,
        grid: &[SweepParam],
        objective: &str,
        o: &Options,
    ) -> Result<SweepReport, DistError> {
        let ks = check_ks(ks)?;
        let sets = self.project_sets()?;
        let targets: Vec<(&GoldenSet, &Katalogr)> = sets.iter().map(|s| (s, self)).collect();
        sweep_over(
            &targets,
            base,
            &ks,
            grid,
            objective,
            &Self::project_options(o),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use katalogr_model::{Confidence, Span};
    use katalogr_query::SymbolHit;

    fn sym(n: &str) -> SymbolHit {
        SymbolHit {
            name: n.into(),
            kind: "fn".into(),
            path: format!("{n}.rs"),
            span: Span {
                start_line: 1,
                start_col: 1,
                end_line: 1,
                end_col: 1,
            },
        }
    }

    fn hit(n: &str) -> RefHit {
        RefHit {
            from: sym(n),
            kind: "calls".into(),
            confidence: Confidence::Extracted,
            via: Vec::new(),
        }
    }

    #[test]
    fn refs_items_from_folded_answer() {
        let fold = Fold {
            key: "m:area".into(),
            kind: "calls".into(),
            targets: 60,
            hits: 7,
            first: vec![sym("f1"), sym("f2")],
            at: 1,
            via: Vec::new(),
            depth: None,
        };
        let f = Folded {
            hits: vec![hit("a"), hit("b")],
            folds: vec![fold.clone()],
        };
        let items = refs_items(&f);
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["a", "f1", "f2", "b"]);
        assert_eq!(items[1].tokens, json_tokens(&fold));
        assert_eq!(items[2].tokens, 0);
        assert_eq!(items[1].path.as_deref(), Some("f1.rs"));
        // A fold after the last hit goes last; no folds is the plain list.
        let tail = Folded {
            hits: vec![hit("a")],
            folds: vec![Fold { at: 1, ..fold }],
        };
        assert_eq!(refs_items(&tail).len(), 3);
        let plain = Folded {
            hits: vec![hit("a")],
            folds: Vec::new(),
        };
        assert_eq!(refs_items(&plain).len(), 1);
    }
}
