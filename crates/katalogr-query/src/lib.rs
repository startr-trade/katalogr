//! Read-only queries over the graph cache: find, refs, callers, why and map (FEAT-004).
//!
//! Every function reads from one [`katalogr_store::ReadTx`] snapshot, so a result is internally
//! consistent. Results are plain serialisable values; [`schemas`] describes their JSON form.

pub mod budget;
mod callers;
mod find;
mod map;
mod refs;
mod resolve;
pub mod result;
mod schema;
mod why;

pub use callers::callers;
pub use find::find;
pub use map::map;
pub use refs::refs;
pub use resolve::resolve;
pub use result::{
    CallerHit, Envelope, MapEntry, RationaleLink, RefHit, Resolution, SymbolHit, Truncation,
    WhyResult,
};
pub use schema::schemas;
pub use why::{RATIONALE_KINDS, why};

/// Re-exported so wrappers can bound on it without a direct dependency.
pub use serde::Serialize;

/// Version of the JSON result envelope; a breaking change bumps it and adds `schema/v2`.
pub const RESULT_SCHEMA_VERSION: u32 = 1;
