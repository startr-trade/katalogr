//! Results of the idle re-measure of the ignored cargo benches (types only; the runner is a
//! later task of FEAT-068).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    Met,
    MissedSetFromMeasurement,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MicroResult {
    pub id: String,
    /// `ms` or `ratio`.
    pub unit: String,
    pub budget: f64,
    pub runs: Vec<f64>,
    pub median: f64,
    pub verdict: Verdict,
    pub proposed_budget: Option<f64>,
    pub item: String,
    pub req: String,
}
