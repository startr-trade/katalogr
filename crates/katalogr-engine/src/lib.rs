//! Registry, stitcher, diff-range scanning, change queue and consistency modes (FEAT-001, FEAT-047).
pub mod error;
pub mod registry;
pub mod scan;
pub mod stitch;

pub use error::EngineError;
pub use stitch::{StitchDelta, Stitcher};
