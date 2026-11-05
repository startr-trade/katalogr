//! Read-only inspection of the cache (FEAT-064 section 3): what `katalogr doctor` looks at.
//!
//! Unlike [`Cache::open_or_rebuild`], inspecting never creates the database, replays the journal,
//! rebuilds anything or quarantines a shard. It opens redb's read-only handle, reads one
//! snapshot and closes it.

use crate::cache::{
    CACHE_SCHEMA, Cache, JournalEntry, PendingKey, ReadTx, Reader, ShardHashRow, ShardKey,
};
use crate::error::StoreError;
use crate::kindex::ProjectDir;
use redb::{DatabaseError, ReadableDatabase, StorageError};

/// What a read-only look at the cache found. Never creates, repairs or rebuilds anything.
#[derive(Debug)]
pub enum CacheInspection {
    /// No `index.redb` yet.
    Missing,
    /// Nothing could be read: another process holds the database exclusively (a writer of a
    /// build without byte-range locks, or one repairing the file).
    InUse,
    /// The file exists but could not be read (corruption, or a redb format upgrade needed).
    /// The message names no path.
    Unreadable(String),
    Open(OpenCache),
}

/// A cache that opened.
#[derive(Debug)]
pub struct OpenCache {
    /// `meta.cache_schema`, or None when the key is absent or malformed.
    pub stored_schema: Option<u32>,
    /// Present only when `stored_schema == Some(CACHE_SCHEMA)`; a cache of another schema is
    /// not read further (its tables may differ).
    pub tables: Option<InspectedTables>,
}

/// The tables the health checks need, read from one snapshot.
#[derive(Debug)]
pub struct InspectedTables {
    /// `Reader::watermark(RANK_CONTEXT)`.
    pub watermark: u64,
    /// `Reader::pending_entries(None)`.
    pub pending: Vec<(PendingKey, u64)>,
    /// `Reader::publish_journal`.
    pub journal: Vec<JournalEntry>,
    /// Every stored shard hash with its recorded `(size, mtime)`, sorted by key.
    pub shard_hashes: Vec<ShardHashRow>,
    /// Files with at least one index-error row, and the row count.
    pub error_files: Vec<(ShardKey, usize)>,
    /// `meta` key `profiles_errors`, as stored.
    pub profile_errors_json: Option<Vec<u8>>,
}

const UNREADABLE_TABLES: &str = "the cache tables cannot be read";

impl Cache {
    /// Opens `<project>/.cache/index.redb` read-only, beside a running writer (FEAT-080). Maps a
    /// database that cannot be shared (a repair running or needed, or no byte-range locks) to
    /// `InUse`, a missing file to `Missing`, and corruption, a needed upgrade and any
    /// other open error to `Unreadable` (a message without paths). Reads one snapshot.
    pub fn inspect(dir: &ProjectDir) -> Result<CacheInspection, StoreError> {
        let path = dir.cache_dir().join("index.redb");
        if !path.exists() {
            return Ok(CacheInspection::Missing);
        }
        let database = match crate::cache::reader_builder().open_read_only(path.as_std_path()) {
            Ok(d) => d,
            Err(DatabaseError::DatabaseAlreadyOpen) | Err(DatabaseError::RepairAborted) => {
                return Ok(CacheInspection::InUse);
            }
            Err(DatabaseError::Storage(StorageError::Corrupted(_))) => {
                return Ok(CacheInspection::Unreadable("the cache is corrupt".into()));
            }
            Err(DatabaseError::UpgradeRequired(_)) => {
                return Ok(CacheInspection::Unreadable(
                    "the cache needs a redb format upgrade".into(),
                ));
            }
            Err(DatabaseError::Storage(StorageError::Io(e))) => {
                return Ok(CacheInspection::Unreadable(format!(
                    "the cache cannot be read ({:?})",
                    e.kind()
                )));
            }
            Err(_) => {
                return Ok(CacheInspection::Unreadable(
                    "the cache cannot be opened".into(),
                ));
            }
        };
        let unreadable = || CacheInspection::Unreadable(UNREADABLE_TABLES.into());
        let Ok(txn) = database.begin_read() else {
            return Ok(unreadable());
        };
        let tx = ReadTx::from_txn(txn);
        let Ok(meta) = tx.meta("cache_schema") else {
            return Ok(unreadable());
        };
        let stored_schema = meta
            .and_then(|v| <[u8; 4]>::try_from(v.as_slice()).ok())
            .map(u32::from_le_bytes);
        if stored_schema != Some(CACHE_SCHEMA) {
            return Ok(CacheInspection::Open(OpenCache {
                stored_schema,
                tables: None,
            }));
        }
        match read_tables(&tx) {
            Ok(tables) => Ok(CacheInspection::Open(OpenCache {
                stored_schema,
                tables: Some(tables),
            })),
            Err(_) => Ok(unreadable()),
        }
    }
}

fn read_tables(tx: &ReadTx) -> Result<InspectedTables, StoreError> {
    Ok(InspectedTables {
        watermark: tx.watermark(crate::rank::RANK_CONTEXT)?,
        pending: tx.pending_entries(None)?,
        journal: tx.publish_journal()?,
        shard_hashes: tx.shard_hash_rows()?,
        error_files: tx.index_error_files()?,
        profile_errors_json: tx.meta("profiles_errors")?,
    })
}
