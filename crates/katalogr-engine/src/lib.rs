//! Registry, stitcher, diff-range scanning, change queue and consistency modes (FEAT-001, FEAT-047).
pub mod base;
pub mod census;
pub mod commit_time;
pub mod consistency;
pub mod context;
pub mod doctor;
pub(crate) mod embed;
pub mod error;
pub mod export;
pub(crate) mod failpoint;
pub mod history;
pub mod lexicon;
pub mod parallel;
pub mod position;
pub mod profiles;
pub mod queue;
pub mod registry;
pub mod scan;
pub mod source;
pub mod stitch;

pub use consistency::{Answer, Consistency, Engine, IndexStats, QueryOptions, Scope};
pub use error::EngineError;
pub use export::canonical_graph;
pub use queue::{ChangeEvent, Context, EventKind, NewEvent, Seq};
pub use source::FileSource;
pub use stitch::{FAN_ROWS_FROM, StitchDelta, Stitcher};

/// Project setting: record the fields of code types (symbols with the `field_type` fact from an
/// extension that declares `code_kinds`). FEAT-101.
pub const CODE_FIELDS_KEY: &str = "extract.code_fields";
/// The value when the setting is absent. Decided by the cost benches (FEAT-101 T24).
pub const CODE_FIELDS_DEFAULT: bool = true;

/// `extract.code_fields` in a project's settings, or [`CODE_FIELDS_DEFAULT`].
pub fn code_fields_enabled(
    settings: &std::collections::BTreeMap<String, serde_yaml_ng::Value>,
) -> bool {
    settings
        .get(CODE_FIELDS_KEY)
        .and_then(serde_yaml_ng::Value::as_bool)
        .unwrap_or(CODE_FIELDS_DEFAULT)
}
