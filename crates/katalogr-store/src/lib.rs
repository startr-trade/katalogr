//! kindex repository, shard codec, path guard and redb cache (FEAT-003, ADR-0013).

pub mod blob;
pub mod cache;
pub mod error;
pub mod guard;
pub mod kindex;
pub mod marker;
pub mod shard;

pub use cache::{
    ArtifactRow, CACHE_SCHEMA, Cache, ErrorRow, LinkEntry, LinkKey, LinkOrigin, LinkRow, ReadTx,
    Reader, RefId, Restitch, Snapshot, SymbolRow, SyncReport, WriteTx,
};
pub use error::StoreError;
pub use guard::{GuardError, scan_for_absolute};
pub use kindex::{
    FormatFile, Kindex, ProjectConfig, ProjectDir, ProjectName, RepoConfig, SHARD_FORMAT,
};
