//! Scoring one query and aggregating queries into labelled metric samples.

use super::golden::{GoldenQuery, Route};
use super::result::QueryResult;
use crate::metrics::{Better, MetricSample, metric_key, round6};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::{BTreeMap, BTreeSet};

/// One ranked answer item as the CLI would print it.
#[derive(Clone, Debug, PartialEq)]
pub struct RankedItem {
    /// Canonical name (symbol, artifact or contract), or a hex id for an unknown node.
    pub name: String,
    /// Repository-relative path when the item has one.
    pub path: Option<String>,
    /// Estimated tokens of the item's compact JSON.
    pub tokens: u64,
}

/// What the runner observed for one query.
#[derive(Clone, Debug, PartialEq)]
pub struct QueryRun {
    pub route: Route,
    pub items: Vec<RankedItem>,
    pub missing_evidence: Vec<String>,
}

/// The repository of a canonical name: the text before the first `:`, unless the name is a
/// contract or has no `:`.
fn repo_of(name: &str) -> Option<&str> {
    if name.starts_with("contract:") {
        return None;
    }
    name.split_once(':').map(|(r, _)| r)
}

fn dcg(gains: impl Iterator<Item = f64>) -> f64 {
    gains
        .enumerate()
        .map(|(i, g)| g / ((i + 2) as f64).log2())
        .sum()
}

fn gain(grade: u8) -> f64 {
    f64::from((1u32 << grade) - 1)
}

fn scope_leak(repos: &[String], paths: &Option<GlobSet>, item: &RankedItem) -> bool {
    if !repos.is_empty()
        && let Some(r) = repo_of(&item.name)
        && !repos.iter().any(|x| x == r)
    {
        return true;
    }
    if let (Some(set), Some(p)) = (paths, &item.path) {
        return !set.is_match(p);
    }
    false
}

/// Metrics of one query. `ks` sorted ascending, deduplicated, each 1..=50.
pub fn score_query(set: &str, q: &GoldenQuery, run: &QueryRun, ks: &[u32]) -> QueryResult {
    let kmax = ks.iter().copied().max().unwrap_or(10) as usize;
    let grades: BTreeMap<&str, u8> = q
        .evidence
        .iter()
        .map(|e| (e.name.as_str(), e.grade))
        .collect();
    let top: Vec<&RankedItem> = run.items.iter().take(kmax).collect();
    let item_gain = |i: usize| -> f64 {
        top.get(i)
            .and_then(|it| grades.get(it.name.as_str()))
            .map_or(0.0, |g| gain(*g))
    };
    let relevant_in_k = top
        .iter()
        .filter(|it| grades.contains_key(it.name.as_str()))
        .count() as u64;
    let has_evidence = !q.evidence.is_empty();

    let mut hit = BTreeMap::new();
    let mut ndcg = BTreeMap::new();
    let mut rr = None;
    if has_evidence {
        let mut ideal: Vec<f64> = q.evidence.iter().map(|e| gain(e.grade)).collect();
        ideal.sort_by(|a, b| b.total_cmp(a));
        for &k in ks {
            let k = k as usize;
            let h = (0..k.min(top.len())).any(|i| grades.contains_key(top[i].name.as_str()));
            hit.insert(k.to_string(), if h { 1.0 } else { 0.0 });
            let d = dcg((0..k).map(item_gain));
            let ideal_d = dcg(ideal.iter().copied().take(k));
            ndcg.insert(
                k.to_string(),
                round6(if ideal_d > 0.0 { d / ideal_d } else { 0.0 }),
            );
        }
        rr = Some(round6(
            top.iter()
                .position(|it| grades.contains_key(it.name.as_str()))
                .map_or(0.0, |i| 1.0 / (i + 1) as f64),
        ));
    }

    let scope_leaks = q.scope.as_ref().map(|s| {
        let globs = if s.paths.is_empty() {
            None
        } else {
            let mut b = GlobSetBuilder::new();
            for p in &s.paths {
                if let Ok(g) = Glob::new(p) {
                    b.add(g);
                }
            }
            b.build().ok()
        };
        top.iter()
            .filter(|it| scope_leak(&s.repos, &globs, it))
            .count() as u64
    });

    let evidence_repos: BTreeSet<&str> =
        q.evidence.iter().filter_map(|e| repo_of(&e.name)).collect();
    let scope_class = if !has_evidence {
        "none"
    } else if evidence_repos.len() >= 2 {
        "cross"
    } else {
        "within"
    };

    QueryResult {
        set: set.to_string(),
        id: q.id.clone(),
        op: q.op,
        tags: q.tags.clone(),
        route_expected: q.route,
        route_actual: run.route,
        route_ok: q.route == run.route,
        scope_class: scope_class.to_string(),
        returned: run.items.len() as u64,
        relevant_in_k,
        hit,
        ndcg,
        rr,
        tokens: top.iter().map(|it| it.tokens).sum(),
        scope_leaks,
        missing_evidence: run.missing_evidence.clone(),
    }
}

fn mean(xs: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, n) = xs.fold((0.0, 0usize), |(s, n), x| (s + x, n + 1));
    (n > 0).then(|| sum / n as f64)
}

fn group(
    out: &mut Vec<MetricSample>,
    rs: &[&QueryResult],
    ks: &[u32],
    kmax: u64,
    labels: &[(&str, &str)],
) {
    if rs.is_empty() {
        return;
    }
    let mut push = |s: MetricSample| {
        let s = labels.iter().fold(s, |s, (k, v)| s.label(k, *v));
        out.push(s);
    };
    let ranked: Vec<&&QueryResult> = rs.iter().filter(|r| r.rr.is_some()).collect();
    if !ranked.is_empty() {
        for &k in ks {
            let key = k.to_string();
            if let Some(m) = mean(
                ranked
                    .iter()
                    .map(|r| r.hit.get(&key).copied().unwrap_or(0.0)),
            ) {
                push(MetricSample::new(
                    format!("retrieval.hit@{k}"),
                    m,
                    Better::Higher,
                ));
            }
            if let Some(m) = mean(
                ranked
                    .iter()
                    .map(|r| r.ndcg.get(&key).copied().unwrap_or(0.0)),
            ) {
                push(MetricSample::new(
                    format!("retrieval.ndcg@{k}"),
                    m,
                    Better::Higher,
                ));
            }
        }
        if let Some(m) = mean(ranked.iter().map(|r| r.rr.unwrap_or(0.0))) {
            push(MetricSample::new("retrieval.mrr", m, Better::Higher));
        }
        let rel: u64 = ranked.iter().map(|r| r.relevant_in_k).sum();
        if rel > 0 {
            let tok: u64 = ranked.iter().map(|r| r.tokens).sum();
            push(MetricSample::new(
                "retrieval.tokens_per_relevant_hit",
                tok as f64 / rel as f64,
                Better::Lower,
            ));
        }
    }
    if let Some(m) = mean(rs.iter().map(|r| if r.route_ok { 1.0 } else { 0.0 })) {
        push(MetricSample::new(
            "retrieval.route_accuracy",
            m,
            Better::Higher,
        ));
    }
    let (leaks, denom) = rs
        .iter()
        .filter(|r| r.scope_leaks.is_some() && r.returned > 0)
        .fold((0u64, 0u64), |(l, d), r| {
            (l + r.scope_leaks.unwrap_or(0), d + r.returned.min(kmax))
        });
    if denom > 0 {
        push(MetricSample::new(
            "retrieval.scope_leak_rate",
            leaks as f64 / denom as f64,
            Better::Lower,
        ));
    }
    let missing: u64 = rs.iter().map(|r| r.missing_evidence.len() as u64).sum();
    push(MetricSample::new(
        "retrieval.evidence_missing",
        missing as f64,
        Better::Lower,
    ));
    push(MetricSample::new(
        "retrieval.queries",
        rs.len() as f64,
        Better::Info,
    ));
}

/// Aggregate samples over (set, op), set, overall, and overall by `scope` within/cross,
/// sorted by `metric_key`.
pub fn aggregate(results: &[QueryResult], ks: &[u32]) -> Vec<MetricSample> {
    let kmax = u64::from(ks.iter().copied().max().unwrap_or(10));
    let mut out = Vec::new();
    let all: Vec<&QueryResult> = results.iter().collect();
    let mut by_set_op: BTreeMap<(&str, super::golden::Op), Vec<&QueryResult>> = BTreeMap::new();
    let mut by_set: BTreeMap<&str, Vec<&QueryResult>> = BTreeMap::new();
    for r in results {
        by_set_op.entry((&r.set, r.op)).or_default().push(r);
        by_set.entry(&r.set).or_default().push(r);
    }
    for ((set, op), rs) in &by_set_op {
        let op = serde_json::to_value(op)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        group(&mut out, rs, ks, kmax, &[("set", set), ("op", &op)]);
    }
    for (set, rs) in &by_set {
        group(&mut out, rs, ks, kmax, &[("set", set)]);
    }
    group(&mut out, &all, ks, kmax, &[]);
    for class in ["within", "cross"] {
        let rs: Vec<&QueryResult> = results.iter().filter(|r| r.scope_class == class).collect();
        group(&mut out, &rs, ks, kmax, &[("scope", class)]);
    }
    out.sort_by_cached_key(metric_key);
    out
}
