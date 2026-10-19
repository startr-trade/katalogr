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
    ArtifactRow, CACHE_SCHEMA, Cache, ErrorRow, JournalEntry, LEXICON_FIELDS, LexiconStats,
    LinkEntry, LinkKey, LinkOrigin, LinkRow, PendingKey, Posting, ReadTx, Reader, RefId, Restitch,
    ShardStat, Snapshot, SymbolRow, SyncReport, WriteTx,
};
pub use error::StoreError;
pub use guard::{GuardError, scan_for_absolute};
pub use kindex::{
    FormatFile, Kindex, ProjectConfig, ProjectDir, ProjectName, RepoConfig, SHARD_FORMAT,
};
pub use rank::{RANK_CONTEXT, link_weight, pagerank, row_weight};
