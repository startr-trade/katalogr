//! Baseline files and comparison of a report with one.

use super::EvalError;
use super::result::{Comparison, EvalReport, QueryRegression};
use crate::metrics::{self, Tolerance, round6};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const BASELINE_FORMAT: u32 = 1;

fn io_err(file: &Path, e: impl std::fmt::Display) -> EvalError {
    EvalError::Io {
        file: file.display().to_string(),
        message: e.to_string(),
    }
}

/// The report as pretty JSON (2-space indent, trailing newline), without `comparison`. Written
/// to `<file>.tmp` then renamed, so a crash leaves the old baseline.
pub fn write_baseline(path: &Path, report: &EvalReport) -> Result<(), EvalError> {
    let mut r = report.clone();
    r.comparison = None;
    let mut text = serde_json::to_string_pretty(&r).map_err(|e| io_err(path, e))?;
    text.push('\n');
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, text).map_err(|e| io_err(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| io_err(path, e))
}

pub fn read_baseline(path: &Path) -> Result<EvalReport, EvalError> {
    let text = std::fs::read_to_string(path).map_err(|e| io_err(path, e))?;
    let report: EvalReport = serde_json::from_str(&text).map_err(|e| EvalError::Format {
        file: path.display().to_string(),
        message: e.to_string(),
    })?;
    if report.format != BASELINE_FORMAT {
        return Err(EvalError::Format {
            file: path.display().to_string(),
            message: format!(
                "format {} is not supported (expected {BASELINE_FORMAT})",
                report.format
            ),
        });
    }
    Ok(report)
}

/// Quality metrics compare exactly; `retrieval.tokens_per_relevant_hit` may grow by 10 %.
pub fn default_tolerance(name: &str) -> Tolerance {
    if name == "retrieval.tokens_per_relevant_hit" {
        Tolerance {
            abs: 0.0,
            rel: 0.10,
        }
    } else {
        Tolerance { abs: 0.0, rel: 0.0 }
    }
}

/// Compare `current` with `baseline`. Errors with Usage when the parameters differ.
pub fn compare_report(
    baseline: &EvalReport,
    current: &EvalReport,
    tol: &dyn Fn(&str) -> Tolerance,
) -> Result<Comparison, EvalError> {
    let keys: BTreeSet<&String> = baseline
        .params
        .keys()
        .chain(current.params.keys())
        .collect();
    let differing: Vec<&str> = keys
        .into_iter()
        .filter(|k| baseline.params.get(*k) != current.params.get(*k))
        .map(String::as_str)
        .collect();
    if !differing.is_empty() {
        return Err(EvalError::Usage(format!(
            "baseline was recorded with other parameters: {}",
            differing.join(", ")
        )));
    }

    let base_sets: BTreeMap<&str, u32> = baseline
        .sets
        .iter()
        .map(|s| (s.set.as_str(), s.version))
        .collect();
    let cur_sets: BTreeMap<&str, u32> = current
        .sets
        .iter()
        .map(|s| (s.set.as_str(), s.version))
        .collect();
    let missing_sets: Vec<String> = base_sets
        .keys()
        .filter(|s| !cur_sets.contains_key(*s))
        .map(|s| s.to_string())
        .collect();
    let new_sets: Vec<String> = cur_sets
        .keys()
        .filter(|s| !base_sets.contains_key(*s))
        .map(|s| s.to_string())
        .collect();
    let version_mismatch: Vec<String> = cur_sets
        .iter()
        .filter(|(s, v)| base_sets.get(*s).is_some_and(|b| b != *v))
        .map(|(s, _)| s.to_string())
        .collect();
    let excluded: BTreeSet<&str> = new_sets
        .iter()
        .chain(&version_mismatch)
        .map(String::as_str)
        .collect();
    // The set-less aggregates mix every set, so they only compare when the set lists agree.
    let sets_agree = excluded.is_empty() && missing_sets.is_empty();
    let keep = |s: &&crate::metrics::MetricSample| match s.labels.get("set") {
        Some(set) => !excluded.contains(set.as_str()) && cur_sets.contains_key(set.as_str()),
        None => sets_agree,
    };
    let base_samples: Vec<_> = baseline
        .metrics
        .iter()
        .filter(|s| match s.labels.get("set") {
            Some(set) => !excluded.contains(set.as_str()) && cur_sets.contains_key(set.as_str()),
            None => sets_agree,
        })
        .cloned()
        .collect();
    let cur_samples: Vec<_> = current.metrics.iter().filter(keep).cloned().collect();
    let diff = metrics::compare(&base_samples, &cur_samples, tol);

    let kmax = current.ks.iter().copied().max().unwrap_or(10).to_string();
    let base_q: BTreeMap<(&str, &str), _> = baseline
        .queries
        .iter()
        .map(|q| ((q.set.as_str(), q.id.as_str()), q))
        .collect();
    let mut queries = Vec::new();
    for q in &current.queries {
        if excluded.contains(q.set.as_str()) {
            continue;
        }
        let Some(b) = base_q.get(&(q.set.as_str(), q.id.as_str())) else {
            continue;
        };
        let mut reg = |metric: String, baseline: f64, current: f64| {
            queries.push(QueryRegression {
                set: q.set.clone(),
                id: q.id.clone(),
                metric,
                baseline: round6(baseline),
                current: round6(current),
            });
        };
        if let (Some(bh), Some(ch)) = (b.hit.get(&kmax), q.hit.get(&kmax))
            && *bh == 1.0
            && *ch == 0.0
        {
            reg(format!("hit@{kmax}"), *bh, *ch);
        }
        if let (Some(bn), Some(cn)) = (b.ndcg.get(&kmax), q.ndcg.get(&kmax))
            && *cn < *bn - tol(&format!("retrieval.ndcg@{kmax}")).abs
        {
            reg(format!("ndcg@{kmax}"), *bn, *cn);
        }
        if b.route_ok && !q.route_ok {
            reg("route".to_string(), 1.0, 0.0);
        }
    }
    queries.sort_by(|a, b| (&a.set, &a.id, &a.metric).cmp(&(&b.set, &b.id, &b.metric)));

    let failed = !diff.regressions.is_empty()
        || !queries.is_empty()
        || !missing_sets.is_empty()
        || !version_mismatch.is_empty();
    Ok(Comparison {
        metrics: diff,
        queries,
        new_sets,
        missing_sets,
        version_mismatch,
        failed,
    })
}
