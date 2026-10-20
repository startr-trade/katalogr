//! Retrieval evaluation: golden sets with graded evidence, scoring, baselines and sweeps
//! (FEAT-061). Pure data and arithmetic; running the queries is the composition root's job.

pub mod baseline;
pub mod golden;
pub mod params;
pub mod result;
pub mod score;

pub use baseline::{
    BASELINE_FORMAT, compare_report, default_tolerance, read_baseline, write_baseline,
};
pub use golden::{
    Evidence, FixtureRepo, GOLDEN_FILE, GOLDEN_FORMAT, GoldenQuery, GoldenSet, Op, Route,
    ScopeExpect, load_set, load_sets,
};
pub use params::{
    EvalParams, MAX_COMBINATIONS, PARAM_KEYS, SweepParam, apply, describe, grid, parse_sweep,
};
pub use result::{
    Comparison, EvalMode, EvalReport, QueryRegression, QueryResult, SetSummary, SweepReport,
    SweepRow,
};
pub use score::{QueryRun, RankedItem, aggregate, score_query};

/// What the harness refuses or cannot read.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// A golden set or baseline file is malformed; `file` is the path as given.
    #[error("{file}: {message}")]
    Format { file: String, message: String },
    #[error("{file}: {message}")]
    Io { file: String, message: String },
    /// A request the harness refuses (unknown sweep key, too many combinations, unknown
    /// objective, baseline recorded with other parameters).
    #[error("{0}")]
    Usage(String),
}
