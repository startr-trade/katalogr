//! Registry, stitcher, diff-range scanning, change queue and consistency modes (FEAT-001, FEAT-047).
pub mod census;
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
pub mod profiles;
pub mod queue;
pub mod registry;
pub mod scan;
pub mod stitch;

pub use consistency::{Answer, Consistency, Engine, IndexStats, QueryOptions, Scope};
pub use error::EngineError;
pub use export::canonical_graph;
pub use queue::{ChangeEvent, Context, EventKind, NewEvent, Seq};
pub use stitch::{FAN_ROWS_FROM, StitchDelta, Stitcher};
