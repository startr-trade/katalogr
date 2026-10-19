//! kindex repository, shard codec, path guard and redb cache (FEAT-003, ADR-0013).

pub mod blob;
pub mod cache;
pub mod error;
#[cfg(feature = "failpoints")]
pub mod failpoint;
pub mod guard;
pub mod kindex;
pub mod marker;
pub mod rank;
pub mod shard;

pub use cache::{
    ArtifactRow, BindingRow, CACHE_SCHEMA, Cache, ContractRow, ErrorRow, JournalEntry,
    LEXICON_FIELDS, LexiconStats, LinkEntry, LinkKey, LinkOrigin, LinkRow, PendingKey, Posting,
    ReadTx, Reader, RefId, Restitch, ShardStat, Snapshot, SymbolRow, SyncReport, WriteTx,
};
pub use error::StoreError;
pub use guard::{GuardError, scan_for_absolute};
pub use kindex::{
    FormatFile, Kindex, ProjectConfig, ProjectDir, ProjectName, RepoConfig, SHARD_FORMAT,
};
pub use rank::{
    PAGERANK_DAMPING, PAGERANK_ITERATIONS, RANK_CONTEXT, link_weight, pagerank, row_weight,
};
