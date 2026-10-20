//! Read-only queries over the graph cache: find, refs, callers, why, map (FEAT-004), search,
//! path, explain and show (FEAT-025).
//!
//! Every function reads from one [`katalogr_store::ReadTx`] snapshot, so a result is internally
//! consistent. Results are plain serialisable values; [`schemas`] describes their JSON form.

pub mod budget;
mod callers;
mod dangling;
pub mod diagnostic;
mod explain;
mod find;
mod hints;
mod map;
mod mirror;
mod path;
mod rank;
mod refs;
mod resolve;
pub mod result;
mod schema;
mod search;
pub mod show;
mod why;

pub use callers::callers;
pub use dangling::{CONTRACT_KIND as DANGLING_CONTRACT, DEFAULT_KINDS as DANGLING_KINDS, dangling};
pub use diagnostic::{
    DIAG_PACK, DIAG_REFS, DIAG_TERMS, DIAG_TOP, DIAG_WEIGHTS, Diagnostic, ExpandedTerm, PackEntry,
    PackTrace, PartTrace, PrefixTrace, RankSource, RankTrace, RoutingTrace, TermTrace, Trace,
    WeightTrace,
};
pub use explain::explain;
pub use find::find;
pub use hints::{HintSource, hints};
pub use map::{map, map_traced};
pub use mirror::mirror_drift;
pub use path::path;
pub use rank::{
    link_weight, rank, rank_cached_or_live, rank_cached_or_live_with_source, rank_live,
};
pub use refs::{refs, refs_traced};
pub use resolve::{resolve, resolve_traced};
pub use result::{
    CallerHit, CommitHit, DanglingRef, Direction, Envelope, Explain, FieldDiff, FieldShape,
    LinkBrief, MapEntry, Matched, MirrorDrift, Note, PARTIAL_NOTE, Partial, PathStep,
    RationaleLink, RefHit, ReplaceResult, Resolution, SearchHit, ShowResult, SymbolHit, Truncation,
    WhyResult, cap,
};
pub use schema::schemas;
pub use search::{search, search_traced};
pub use why::{Blame, RATIONALE_KINDS, why};

/// Re-exported so wrappers can bound on it without a direct dependency.
pub use serde::Serialize;

/// Version of the JSON result envelope; a breaking change bumps it and adds `schema/v2`.
pub const RESULT_SCHEMA_VERSION: u32 = 1;
