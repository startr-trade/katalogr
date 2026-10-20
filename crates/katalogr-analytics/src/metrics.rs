//! Quality metrics as labelled samples with file baselines and deterministic gates (ADR-0024).
//!
//! Shared by the retrieval evaluation (FEAT-061) and the coverage report (FEAT-062).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Which direction is better; `Info` samples are reported and never gated.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Better {
    Higher,
    Lower,
    Info,
}

/// One measured value. `name` is dotted (`retrieval.ndcg@10`, `coverage.parse_error_rate`);
/// `labels` qualify it (`set`, `op`, `scope`, `repo`, `extension`, `language`); an absent label
/// means "all". `value` is rounded to 6 decimals.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MetricSample {
    pub name: String,
    pub value: f64,
    pub better: Better,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

impl MetricSample {
    /// A sample without labels; `value` is rounded to 6 decimals.
    pub fn new(name: impl Into<String>, value: f64, better: Better) -> Self {
        MetricSample {
            name: name.into(),
            value: round6(value),
            better,
            labels: BTreeMap::new(),
        }
    }

    /// Add (or replace) one label.
    pub fn label(mut self, k: &str, v: impl Into<String>) -> Self {
        self.labels.insert(k.to_string(), v.into());
        self
    }
}

/// `name{k1=v1,k2=v2}` with labels in key order; `name` alone without labels. Unique per report.
pub fn metric_key(s: &MetricSample) -> String {
    if s.labels.is_empty() {
        return s.name.clone();
    }
    let labels: Vec<String> = s.labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("{}{{{}}}", s.name, labels.join(","))
}

/// `(x * 1e6).round() / 1e6`.
pub fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// When a change counts as a regression. Higher-better: `current < baseline - abs`.
/// Lower-better: `current > baseline * (1 + rel) + abs` (abs guards a zero baseline).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tolerance {
    pub abs: f64,
    pub rel: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MetricChange {
    pub key: String,
    pub baseline: f64,
    pub current: f64,
    pub better: Better,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MetricDiff {
    pub regressions: Vec<MetricChange>,
    pub improvements: Vec<MetricChange>,
    pub unchanged: u64,
    /// Keys only in current.
    pub added: Vec<String>,
    /// Keys only in baseline.
    pub removed: Vec<String>,
}

/// Compare by `metric_key`. `tol(name)` gives the tolerance per metric name. `Info` samples and
/// equal values count as unchanged. Every list is sorted by key.
pub fn compare(
    baseline: &[MetricSample],
    current: &[MetricSample],
    tol: &dyn Fn(&str) -> Tolerance,
) -> MetricDiff {
    let base: BTreeMap<String, &MetricSample> =
        baseline.iter().map(|s| (metric_key(s), s)).collect();
    let cur: BTreeMap<String, &MetricSample> = current.iter().map(|s| (metric_key(s), s)).collect();
    let mut diff = MetricDiff::default();
    for (key, c) in &cur {
        let Some(b) = base.get(key) else {
            diff.added.push(key.clone());
            continue;
        };
        let t = tol(&c.name);
        let (regressed, improved) = match c.better {
            Better::Info => (false, false),
            Better::Higher => (c.value < b.value - t.abs, c.value > b.value),
            Better::Lower => (c.value > b.value * (1.0 + t.rel) + t.abs, c.value < b.value),
        };
        let change = || MetricChange {
            key: key.clone(),
            baseline: b.value,
            current: c.value,
            better: c.better,
        };
        if regressed {
            diff.regressions.push(change());
        } else if improved {
            diff.improvements.push(change());
        } else {
            diff.unchanged += 1;
        }
    }
    diff.removed = base
        .keys()
        .filter(|k| !cur.contains_key(*k))
        .cloned()
        .collect();
    diff
}
