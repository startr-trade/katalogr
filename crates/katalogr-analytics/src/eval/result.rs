//! Evaluation results: per-query metrics, the report, the comparison and the sweep.

use super::golden::{Op, Route};
use crate::metrics::{Better, MetricDiff, MetricSample};
use crate::result::Itemized;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum EvalMode {
    Fixture,
    Project,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SetSummary {
    pub set: String,
    pub version: u32,
    pub queries: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct QueryResult {
    pub set: String,
    pub id: String,
    pub op: Op,
    pub tags: Vec<String>,
    pub route_expected: Route,
    pub route_actual: Route,
    pub route_ok: bool,
    /// `within` (evidence in 1 repo), `cross` (evidence in 2+ repos), `none` (no evidence).
    pub scope_class: String,
    /// Items returned (before the k cut).
    pub returned: u64,
    /// Evidence items among the first kmax.
    pub relevant_in_k: u64,
    /// Per k ("1", "5", ...); empty when the query has no evidence.
    pub hit: BTreeMap<String, f64>,
    pub ndcg: BTreeMap<String, f64>,
    /// Reciprocal rank within kmax; absent when the query has no evidence.
    pub rr: Option<f64>,
    /// Estimated tokens of the first kmax items.
    pub tokens: u64,
    /// Absent when the query has no scope.
    pub scope_leaks: Option<u64>,
    /// Evidence names not in the index.
    pub missing_evidence: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EvalReport {
    pub format: u32,
    pub mode: EvalMode,
    pub ks: Vec<u32>,
    pub params: BTreeMap<String, f64>,
    pub sets: Vec<SetSummary>,
    pub metrics: Vec<MetricSample>,
    pub queries: Vec<QueryResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison: Option<Comparison>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct QueryRegression {
    pub set: String,
    pub id: String,
    pub metric: String,
    pub baseline: f64,
    pub current: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Comparison {
    pub metrics: MetricDiff,
    pub queries: Vec<QueryRegression>,
    pub new_sets: Vec<String>,
    pub missing_sets: Vec<String>,
    /// Sets whose version differs from the baseline's.
    pub version_mismatch: Vec<String>,
    /// True when the gate fails: any metric or query regression, missing set or version mismatch.
    pub failed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SweepRow {
    pub params: BTreeMap<String, f64>,
    pub is_default: bool,
    pub objective: f64,
    /// The overall group only.
    pub metrics: Vec<MetricSample>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SweepReport {
    pub objective: String,
    pub better: Better,
    pub combinations: u64,
    pub rows: Vec<SweepRow>,
}

impl Itemized for EvalReport {
    type Item = QueryResult;
    fn items_mut(&mut self) -> &mut Vec<QueryResult> {
        &mut self.queries
    }
}

impl Itemized for SweepReport {
    type Item = SweepRow;
    fn items_mut(&mut self) -> &mut Vec<SweepRow> {
        &mut self.rows
    }
}
