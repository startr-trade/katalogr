//! kindex repository, shard codec, path guard and redb cache (FEAT-003, ADR-0013).

pub mod answers;
pub mod blob;
pub mod cache;
pub mod canon;
mod codec;
pub mod compact;
pub mod context;
mod dict;
pub mod error;
#[cfg(feature = "failpoints")]
pub mod failpoint;
mod fan;
pub mod guard;
pub mod inspect;
pub mod kindex;
pub mod marker;
mod postings;
pub mod rank;
pub mod shard;
pub mod snapshot;

pub use answers::{
    ANSWER_FORMAT, AnswerHeader, AnswerListing, AnswerStore, DirAnswerStore, MAX_ANSWER_BYTES,
    PruneReport, PutOutcome, StoredAnswer,
};
pub use cache::PathKey;
pub use cache::{
    ArtifactRow, BindingRow, CACHE_SCHEMA, Cache, ContractRow, ErrorRow, FanKey, LEXICON_FIELDS,
    LexiconStats, LinkBrief, LinkEntry, LinkKey, LinkOrigin, LinkRow, OpenState, PendingKey,
    Posting, RANK_TOP_ENTRIES, RankTop, ReadTx, Reader, RefId, Snapshot, SymbolBrief, SymbolCore,
    SymbolRow, TableId, WriteTx,
};
pub use compact::{
    CompactMode, CompactOutcome, CompactPolicy, CompactReport, ExportPlan, REWRITE_FILE,
};
pub use context::{ContextId, ContextKind, ContextManifest, seed_layer, seed_layer_with_repos};
#[doc(hidden)]
pub use dict::Memo;
pub use error::StoreError;
pub use guard::{GuardError, needles_from, scan_for_absolute, scan_for_absolute_with};
pub use inspect::{CacheInspection, InspectedTables, OpenCache};
pub use kindex::{
    FormatFile, GitState, Kindex, ProjectConfig, ProjectDir, ProjectName, RepoConfig, SHARD_FORMAT,
};
pub use rank::{
    PAGERANK_DAMPING, PAGERANK_ITERATIONS, RANK_CONTEXT, fan_weight, link_weight, pagerank,
    row_weight,
};
pub use snapshot::{
    IMPORT_FILE, MAX_MANIFEST_BYTES, SNAPSHOT_FORMAT, SNAPSHOT_MAGIC, SNAPSHOT_SUFFIX,
    SnapshotManifest, SnapshotPosition, decode_snapshot, install_snapshot, read_manifest,
    write_snapshot,
};
