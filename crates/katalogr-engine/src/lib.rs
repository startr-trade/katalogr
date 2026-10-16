//! Registry, stitcher, diff-range scanning, change queue and consistency modes (FEAT-001, FEAT-047).
pub mod consistency;
pub mod error;
pub mod queue;
pub mod registry;
pub mod scan;
pub mod stitch;

pub use consistency::{Answer, Consistency, Engine, QueryOptions, Scope};
pub use error::EngineError;
pub use queue::{ChangeEvent, Context, EventKind, NewEvent, Seq};
pub use stitch::{StitchDelta, Stitcher};
