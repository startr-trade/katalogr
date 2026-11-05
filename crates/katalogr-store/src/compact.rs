//! Cache compaction (FEAT-075 R-6, R-7).
//!
//! redb never returns freed pages, and rows written in many small rounds leave half-filled
//! pages, so a cache that was indexed incrementally is much larger than its live data. Two
//! remedies:
//!
//! - **Rewrite.** Every table is copied in key order into `index.redb.rewrite`, that file is
//!   compacted, and it is renamed over the cache while the old file is still open and locked by
//!   this process. The result is the fresh-file layout, 24 to 28 percent smaller than redb's
//!   in-place compaction.
//! - **In place.** redb's `Database::compact()`, for platforms where an open file cannot be
//!   renamed over.
//!
//! Nothing here takes the processor lock: the exclusive database lock this process already holds
//! is the only exclusion needed (see the spec's "Why no processor lock").
//!
//! **Read-only handles beside the compaction (FEAT-080).** A process that opened the cache with
//! `Cache::open_reader` before the rename keeps the old inode: it goes on reading a complete,
//! older snapshot, which is safe, and sees the new file the next time it opens the path. The
//! compacting process never writes to the old inode again. The fresh file is created through
//! [`crate::cache::writer_builder`], the same builder every writer open uses, so its concurrency
//! mode and repair callback match, and the reopen of the swapped file goes through the same
//! backoff as any writer open.

use crate::cache::{TableId, db, writer_builder};
use crate::error::StoreError;
use camino::Utf8Path;
use redb::{Database, ReadableDatabase, ReadableTable};
use std::time::Instant;

/// File name of the rewrite target, beside `index.redb`.
pub const REWRITE_FILE: &str = "index.redb.rewrite";

/// Rows copied per transaction, in bytes of keys and values.
const BATCH_BYTES: usize = 64 * 1024 * 1024;

/// `meta` key: the file size right after the last compaction (u64 LE).
pub(crate) const COMPACTED_BYTES_META: &str = "compacted_bytes";

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CompactMode {
    /// Copy every table in key order into `.cache/index.redb.rewrite`, compact that file,
    /// rename it over the cache while still holding the old one, then reopen.
    Rewrite,
    /// redb `Database::compact()` on the open file.
    InPlace,
}

impl CompactMode {
    /// `Rewrite` on Unix; `InPlace` where a file cannot be renamed over an open one (Windows).
    pub fn preferred() -> CompactMode {
        if cfg!(windows) {
            CompactMode::InPlace
        } else {
            CompactMode::Rewrite
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CompactPolicy {
    /// Compact when the file exceeds `growth_factor` times its last compacted size ...
    pub growth_factor: f64,
    /// ... and also exceeds it by at least this many bytes.
    pub min_slack_bytes: u64,
}

impl Default for CompactPolicy {
    fn default() -> Self {
        CompactPolicy {
            growth_factor: 2.0,
            min_slack_bytes: 64 * 1024 * 1024,
        }
    }
}

impl CompactPolicy {
    /// Whether a file of `file` bytes, last compacted at `compacted` bytes (`None`: never), is
    /// due. A cache that was never compacted counts as compacted at zero bytes.
    pub fn due(&self, file: u64, compacted: Option<u64>) -> bool {
        let base = compacted.unwrap_or(0);
        file as f64 > self.growth_factor * base as f64
            && file.saturating_sub(base) >= self.min_slack_bytes
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompactOutcome {
    Done,
    /// Nothing changed on disk; the reason is a short message without paths.
    Skipped(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactReport {
    pub mode: CompactMode,
    pub outcome: CompactOutcome,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub millis: u64,
}

/// Copies every table of `old` in key order into a new file at `tmp`, then compacts that file.
pub(crate) fn rewrite(old: &Database, tmp: &Utf8Path, deadline: Instant) -> Result<(), StoreError> {
    let mut new = writer_builder(deadline)
        .create(tmp.as_std_path())
        .map_err(db)?;
    let rtx = old.begin_read().map_err(db)?;
    // The tables are copied in transactions of about `BATCH_BYTES` of rows, so memory stays
    // bounded on a large cache and a small one pays for a single commit. A file shared with other
    // processes allows no non-durable commit, so every commit here is durable.
    let mut w = new.begin_write().map_err(db)?;
    let mut bytes = 0usize;
    for t in TableId::ALL {
        let src = rtx.open_table(t.def()).map_err(db)?;
        let mut rows = src.iter().map_err(db)?;
        loop {
            let mut full = false;
            {
                let mut dst = w.open_table(t.def()).map_err(db)?;
                for r in rows.by_ref() {
                    let (k, v) = r.map_err(db)?;
                    dst.insert(k.value(), v.value()).map_err(db)?;
                    bytes += k.value().len() + v.value().len();
                    if bytes >= BATCH_BYTES {
                        full = true;
                        break;
                    }
                }
            }
            if !full {
                break;
            }
            w.commit().map_err(db)?;
            w = new.begin_write().map_err(db)?;
            bytes = 0;
        }
    }
    w.commit().map_err(db)?;
    drop(rtx);
    new.compact().map_err(db)?;
    drop(new);
    record_size(tmp, deadline)
}

/// Writes `meta.compacted_bytes` into the closed file at `path` as the size the file has once it
/// is closed again (redb trims the unused tail on close). The write can itself change that size,
/// so it repeats until the stored number is the size it left; a few rounds at most.
pub(crate) fn record_size(path: &Utf8Path, deadline: Instant) -> Result<(), StoreError> {
    let len = |p: &Utf8Path| {
        std::fs::metadata(p)
            .map(|m| m.len())
            .map_err(|e| StoreError::io(p, e))
    };
    let mut size = len(path)?;
    for _ in 0..4 {
        {
            let database = writer_builder(deadline)
                .create(path.as_std_path())
                .map_err(db)?;
            let w = database.begin_write().map_err(db)?;
            {
                let mut meta = w.open_table(TableId::Meta.def()).map_err(db)?;
                meta.insert(
                    COMPACTED_BYTES_META.as_bytes(),
                    size.to_le_bytes().as_slice(),
                )
                .map_err(db)?;
            }
            w.commit().map_err(db)?;
        }
        let now = len(path)?;
        if now == size {
            break;
        }
        size = now;
    }
    Ok(())
}

/// Renames the rewritten file over the cache and makes the rename durable.
pub(crate) fn swap(tmp: &Utf8Path, path: &Utf8Path) -> std::io::Result<()> {
    std::fs::rename(tmp, path)?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Removes a rewrite file left by a process that stopped mid-rewrite. Best effort: a rewrite
/// file that cannot be removed is left for the next open (and makes the next rewrite skip).
pub(crate) fn remove_orphan(cache_dir: &Utf8Path) {
    let _ = std::fs::remove_file(cache_dir.join(REWRITE_FILE));
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn compaction_due_threshold() {
        let p = CompactPolicy::default();
        // (file, compacted, due)
        let table: [(u64, Option<u64>, bool); 9] = [
            (10 * MIB, Some(10 * MIB), false),
            (100 * MIB, Some(100 * MIB), false),
            // More than twice, but less than 64 MiB of slack.
            (50 * MIB, Some(20 * MIB), false),
            // 64 MiB of slack, but not twice.
            (200 * MIB, Some(140 * MIB), false),
            // Both hold.
            (300 * MIB, Some(100 * MIB), true),
            (160 * MIB, Some(70 * MIB), true),
            // Exactly twice is not more than twice.
            (200 * MIB, Some(100 * MIB), false),
            // Never compacted: due once the file holds 64 MiB.
            (63 * MIB, None, false),
            (65 * MIB, None, true),
        ];
        for (file, compacted, due) in table {
            assert_eq!(
                p.due(file, compacted),
                due,
                "file {file}, compacted {compacted:?}"
            );
        }
        let eager = CompactPolicy {
            growth_factor: 1.0,
            min_slack_bytes: 0,
        };
        assert!(eager.due(2, Some(1)));
        assert!(!eager.due(1, Some(1)));
    }

    #[test]
    fn preferred_mode_by_platform() {
        assert_eq!(
            CompactMode::preferred(),
            if cfg!(windows) {
                CompactMode::InPlace
            } else {
                CompactMode::Rewrite
            }
        );
    }
}
