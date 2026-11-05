//! kindex repository, shard codec, path guard and redb cache (FEAT-003, ADR-0013).

pub mod blob;
pub mod cache;
mod codec;
pub mod compact;
mod dict;
pub mod error;
#[cfg(feature = "failpoints")]
pub mod failpoint;
mod fan;
pub mod guard;
pub mod inspect;
pub mod kindex;
pub mod marker;
pub mod rank;
pub mod shard;

pub use cache::ShardKey;
pub use cache::{
    ArtifactRow, BindingRow, CACHE_SCHEMA, Cache, ContractRow, ErrorRow, FanKey, JournalEntry,
    LEXICON_FIELDS, LexiconStats, LinkBrief, LinkEntry, LinkKey, LinkOrigin, LinkRow, PendingKey,
    Posting, RANK_TOP_ENTRIES, RankTop, ReadTx, Reader, RefId, Restitch, ShardStat, Snapshot,
    SymbolBrief, SymbolCore, SymbolRow, SyncReport, WriteTx,
};
pub use compact::{CompactMode, CompactOutcome, CompactPolicy, CompactReport, REWRITE_FILE};
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
