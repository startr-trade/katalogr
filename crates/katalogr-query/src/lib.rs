//! Read-only queries over the graph cache: find, refs, callers, why, map (FEAT-004), search,
//! path, explain and show (FEAT-025).
//!
//! Every function reads from one [`katalogr_store::ReadTx`] snapshot, so a result is internally
//! consistent. Results are plain serialisable values; [`schemas`] describes their JSON form.

pub mod budget;
mod callers;
mod contract_map;
mod dangling;
mod deps;
pub mod diagnostic;
pub mod doc_impact;
mod explain;
mod find;
pub mod fold;
mod hints;
pub mod impact;
pub mod impact_seeds;
pub mod lineage;
mod map;
mod mirror;
mod path;
pub mod position;
mod rank;
mod refs;
mod resolve;
pub mod result;
mod schema;
mod search;
pub mod show;
mod step;
mod tests;
pub mod timeline;
mod view_answer;
mod why;
pub mod working_set;

pub use callers::{CallersAnswer, callers, callers_folded};
pub use contract_map::{ContractEnds, ContractMap, contract_map};
pub use dangling::{CONTRACT_KIND as DANGLING_CONTRACT, DEFAULT_KINDS as DANGLING_KINDS, dangling};
pub use deps::{DepHit, DepsOptions, deps};
pub use diagnostic::{
    DIAG_PACK, DIAG_REFS, DIAG_TERMS, DIAG_TOP, DIAG_WEIGHTS, Diagnostic, ExpandedTerm, FoldTrace,
    PackEntry, PackTrace, PartTrace, PrefixTrace, RankSource, RankTrace, RoutingTrace, TermTrace,
    Trace, WeightTrace,
};
pub use doc_impact::{
    AffectedDoc, AffectedLink, ChangedFile, DOC_LINK_KINDS, DocImpact, DocImpactInput, doc_impact,
};
pub use explain::explain;
pub use find::find;
pub use fold::{FOLD_FROM, FOLD_SHOWN, Folded};
pub use hints::{HintSource, hints};
pub use impact::{
    DEFAULT_IMPACT_DEPTH, DEFAULT_IMPACT_LIMIT, IMPACT_SKIPPED_KINDS, ImpactContract, ImpactGroup,
    ImpactHit, ImpactOptions, ImpactReport, ImpactSeed, SEED_BROKEN_REF, SEED_CONTRACT,
    SEED_CONTRACT_CONSUMERS, SEED_FILE, SEED_ITEM, SEED_SYMBOL, Seed, impact,
};
pub use impact_seeds::{seeds_for_changes, seeds_for_file, seeds_for_item, seeds_for_symbol};
pub use lineage::{
    DEFAULT_LINEAGE_DEPTH, DEFAULT_LINEAGE_LIMIT, DEFAULT_LINEAGE_PER_GROUP, LINEAGE_SKIPPED_KINDS,
    LineageAnswer, LineageField, LineageFieldCopy, LineageGroup, LineageHit, LineageMapping,
    LineageOptions, LineageRelated, LineageReport, lineage,
};
pub use map::{map, map_boosted_traced, map_top, map_top_behind, map_traced};
pub use mirror::mirror_drift;
pub use path::path;
pub use position::{PositionAnswer, PositionError, PositionRef, VIEW_FORMAT, ViewQuery, view_id};
pub use rank::{
    file_symbol_ranks, link_weight, rank, rank_cached_or_live, rank_cached_or_live_with_source,
    rank_live,
};
pub use refs::{refs, refs_folded_traced, refs_traced};
pub use resolve::{resolve, resolve_traced};
pub use result::{
    CallerHit, CommitHit, ContractEnd, ContractInfo, DanglingRef, Direction, Envelope, Explain,
    FieldDiff, FieldShape, Fold, LinkBrief, MapEntry, Matched, MirrorDrift, Note, PARTIAL_NOTE,
    Partial, PathStep, RationaleLink, RefHit, ReplaceResult, Resolution, SearchHit, ShowResult,
    SymbolHit, TestHit, Truncation, WhyResult, WhySource, WhyUse, cap,
};
pub use schema::{position_schema, schemas};
pub use search::{
    SearchParams, search, search_boosted_traced, search_traced, search_with, search_with_traced,
};
pub use tests::{DEFAULT_TEST_DEPTH, TestsOptions, tests, tests_for};
pub use timeline::{PointSource, Timeline, TimelinePoint};
pub use view_answer::{ViewAnswer, ViewQueryJson, view_schema};
pub use why::{Blame, DEFAULT_USE_LIMIT, RATIONALE_KINDS, attach_use, why, why_sources, why_use};
pub use working_set::{Boost, Tier, Weights, WorkingSet, WorkingSetSummary, boost};

/// Re-exported so wrappers can bound on it without a direct dependency.
pub use serde::Serialize;

/// Version of the JSON result envelope; a breaking change bumps it and adds `schema/v2`.
pub const RESULT_SCHEMA_VERSION: u32 = 1;
