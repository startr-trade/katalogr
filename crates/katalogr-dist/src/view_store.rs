//! The store of generated views (FEAT-088, ADR-0036): the exact body of an exact view, keyed by
//! its view ID, under the project's gitignored cache.
//!
//! `<project>/.cache/views/<first 2 hex after v1->/<view_id>.json`. A view ID covers position,
//! facts and question, so one store serves every context of a project. Writers each use their
//! own temp file and rename it, so a reader never sees a partial file and two writers of one
//! ID (equal bytes) leave one file. A store failure never fails a read.

use crate::DistError;
use katalogr_store::ProjectDir;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::SystemTime;

/// The folder under the project's cache directory.
const DIR: &str = "views";
/// An entry's extension.
const EXT: &str = "json";
/// The first characters of every view ID of format 1.
const ID_PREFIX: &str = "v1-";
/// Hex digits after the prefix.
const ID_HEX: usize = 32;

/// Distinguishes temp files of threads in one process.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);
/// Prune runs from `put` at most once per process.
static PRUNED_BY_PUT: AtomicBool = AtomicBool::new(false);

/// Views stored under one project.
#[derive(Debug, Clone)]
pub struct ViewStore {
    root: PathBuf,
    cap_bytes: u64,
}

/// `v1-` and 32 lower-case hex digits: the only IDs that name a file.
fn valid_id(id: &str) -> bool {
    id.strip_prefix(ID_PREFIX).is_some_and(|h| {
        h.len() == ID_HEX && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// An entry found on disk.
struct Entry {
    path: PathBuf,
    id: String,
    size: u64,
    modified: SystemTime,
}

impl ViewStore {
    /// Under the project's `ProjectDir::cache_dir()`, in `views/`. Never fails on a missing
    /// folder: it is made when the first view is put.
    pub fn open(project: &ProjectDir, cap_bytes: u64) -> ViewStore {
        ViewStore {
            root: project.cache_dir().as_std_path().join(DIR),
            cap_bytes,
        }
    }

    fn path_of(&self, id: &str) -> PathBuf {
        self.root
            .join(&id[ID_PREFIX.len()..ID_PREFIX.len() + 2])
            .join(format!("{id}.{EXT}"))
    }

    /// `None` when absent, unreadable, unparsable or holding another ID.
    pub fn get(&self, id: &str) -> Option<Vec<u8>> {
        if !valid_id(id) {
            return None;
        }
        let bytes = std::fs::read(self.path_of(id)).ok()?;
        let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        (v.get("view_id").and_then(serde_json::Value::as_str) == Some(id)).then_some(bytes)
    }

    /// Atomic, and may be called at once by many processes. The caller turns an error into a
    /// warning. When the folder is over the cap, prunes it, at most once per process.
    pub fn put(&self, id: &str, bytes: &[u8]) -> Result<(), DistError> {
        if !valid_id(id) {
            return Err(DistError::usage(format!("{id:?} is not a view ID")));
        }
        let path = self.path_of(id);
        let dir = path.parent().expect("an entry has a folder");
        std::fs::create_dir_all(dir).map_err(|e| DistError::io(dir.display(), e))?;
        let temp = dir.join(format!(
            ".{id}.{}.{}.tmp",
            std::process::id(),
            TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let written = (|| {
            use std::io::Write;
            let mut f = std::fs::File::create(&temp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            std::fs::rename(&temp, &path)
        })();
        if let Err(e) = written {
            let _ = std::fs::remove_file(&temp);
            return Err(DistError::io(path.display(), e));
        }
        if !PRUNED_BY_PUT.load(Ordering::Relaxed)
            && self.total_bytes() > self.cap_bytes
            && !PRUNED_BY_PUT.swap(true, Ordering::Relaxed)
        {
            self.prune()?;
        }
        Ok(())
    }

    /// Removes oldest by modified time, then by ID, until under the cap. Returns the count.
    pub fn prune(&self) -> Result<usize, DistError> {
        self.sweep_temps();
        let mut entries = self.entries();
        let mut total: u64 = entries.iter().map(|e| e.size).sum();
        if total <= self.cap_bytes {
            return Ok(0);
        }
        entries.sort_by(|a, b| (a.modified, &a.id).cmp(&(b.modified, &b.id)));
        let mut removed = 0;
        for e in entries {
            if total <= self.cap_bytes {
                break;
            }
            match std::fs::remove_file(&e.path) {
                Ok(()) => {
                    total -= e.size;
                    removed += 1;
                }
                // Another process removed it first.
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => total -= e.size,
                Err(err) => return Err(DistError::io(e.path.display(), err)),
            }
        }
        Ok(removed)
    }

    /// Removes temp files a crashed writer left behind (older than an hour).
    fn sweep_temps(&self) {
        let Ok(buckets) = std::fs::read_dir(&self.root) else {
            return;
        };
        let old = |m: &std::fs::Metadata| {
            m.modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|d| d.as_secs() > 3600)
        };
        for bucket in buckets.flatten() {
            let Ok(files) = std::fs::read_dir(bucket.path()) else {
                continue;
            };
            for f in files.flatten() {
                let is_temp = f.file_name().to_str().is_some_and(|n| n.ends_with(".tmp"));
                if is_temp && f.metadata().is_ok_and(|m| old(&m)) {
                    let _ = std::fs::remove_file(f.path());
                }
            }
        }
    }

    fn total_bytes(&self) -> u64 {
        self.entries().iter().map(|e| e.size).sum()
    }

    /// Every entry on disk; temp files and foreign files are not entries.
    fn entries(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        let Ok(buckets) = std::fs::read_dir(&self.root) else {
            return out;
        };
        for bucket in buckets.flatten() {
            let Ok(files) = std::fs::read_dir(bucket.path()) else {
                continue;
            };
            for f in files.flatten() {
                let path = f.path();
                let Some(id) = entry_id(&path) else { continue };
                let Ok(meta) = f.metadata() else { continue };
                out.push(Entry {
                    path,
                    id,
                    size: meta.len(),
                    modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                });
            }
        }
        out
    }
}

/// The ID a file name carries, when it is `<view id>.json`.
fn entry_id(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let id = name.strip_suffix(&format!(".{EXT}"))?;
    valid_id(id).then(|| id.to_string())
}
