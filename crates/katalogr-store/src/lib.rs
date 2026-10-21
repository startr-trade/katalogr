//! kindex repository, shard codec, path guard and redb cache (FEAT-003, ADR-0013).

pub mod blob;
pub mod cache;
// Not wired into the cache yet: FEAT-075 T5 switches the tables to it.
#[allow(dead_code)]
mod codec;
pub mod error;
#[cfg(feature = "failpoints")]
pub mod failpoint;
pub mod guard;
pub mod inspect;
pub mod kindex;
pub mod marker;
pub mod rank;
pub mod shard;

pub use cache::ShardKey;
pub use cache::{
    ArtifactRow, BindingRow, CACHE_SCHEMA, Cache, ContractRow, ErrorRow, JournalEntry,
    LEXICON_FIELDS, LexiconStats, LinkBrief, LinkEntry, LinkKey, LinkOrigin, LinkRow, PendingKey,
    Posting, ReadTx, Reader, RefId, Restitch, ShardStat, Snapshot, SymbolBrief, SymbolRow,
    SyncReport, WriteTx,
};
pub use error::StoreError;
pub use guard::{GuardError, scan_for_absolute};
pub use inspect::{CacheInspection, InspectedTables, OpenCache};
pub use kindex::{
    FormatFile, GitState, Kindex, ProjectConfig, ProjectDir, ProjectName, RepoConfig, SHARD_FORMAT,
};
pub use rank::{
    PAGERANK_DAMPING, PAGERANK_ITERATIONS, RANK_CONTEXT, link_weight, pagerank, row_weight,
};
