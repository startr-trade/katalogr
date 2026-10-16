//! The inbox spool: one small file per event, durable before the producer is acknowledged.

use super::{ChangeEvent, Context, EventKind, NewEvent};
use crate::error::EngineError;
use crate::failpoint::fail;
use camino::{Utf8Path, Utf8PathBuf};
use katalogr_model::RelPath;
use katalogr_store::ProjectDir;
use serde::{Deserialize, Serialize};
use std::hash::{BuildHasher, Hasher};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

/// Inbox entry format version.
const ENTRY_VERSION: u32 = 1;
const SUFFIX: &str = ".json";

#[derive(Serialize, Deserialize)]
struct Entry {
    v: u32,
    id: String,
    context: Context,
    repo: String,
    path: Option<RelPath>,
    kind: EventKind,
}

pub struct Inbox {
    dir: Utf8PathBuf,
}

impl Inbox {
    /// The inbox of a project: `<project>/.cache/inbox/`.
    pub fn new(project: &ProjectDir) -> Inbox {
        Inbox::at(project.cache_dir().join("inbox"))
    }

    pub fn at(dir: Utf8PathBuf) -> Inbox {
        Inbox { dir }
    }

    pub fn dir(&self) -> &Utf8Path {
        &self.dir
    }

    /// R-1. Writes `<unix_nanos:020>-<pid>-<rand:08x>.json` via a temp file in the inbox
    /// directory, fsync, rename, fsync of the directory. Returns the id (the file stem) only
    /// once the event is durable. Never touches the cache database.
    pub fn append(&self, ev: &NewEvent) -> Result<String, EngineError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| EngineError::io(&self.dir, e))?;
        let id = new_id();
        let entry = Entry {
            v: ENTRY_VERSION,
            id: id.clone(),
            context: ev.context.clone(),
            repo: ev.repo.clone(),
            path: ev.path.clone(),
            kind: ev.kind,
        };
        let bytes = serde_json::to_vec(&entry).map_err(|e| EngineError::Queue(e.to_string()))?;
        let target = self.dir.join(format!("{id}{SUFFIX}"));
        let io = |e| EngineError::io(&target, e);
        let mut tmp = tempfile::NamedTempFile::new_in(&self.dir).map_err(io)?;
        tmp.write_all(&bytes).map_err(io)?;
        tmp.as_file().sync_all().map_err(io)?;
        tmp.persist(&target)
            .map_err(|e| EngineError::io(&target, e.error))?;
        fail!("after_inbox_rename");
        sync_dir(&self.dir)?;
        Ok(id)
    }

    /// Entry files, sorted by file name. A missing inbox is empty.
    pub fn list(&self) -> Result<Vec<Utf8PathBuf>, EngineError> {
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(EngineError::io(&self.dir, e)),
        };
        let mut out = Vec::new();
        for e in rd {
            let e = e.map_err(|e| EngineError::io(&self.dir, e))?;
            let Some(name) = e.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.ends_with(SUFFIX) && !name.starts_with('.') {
                out.push(self.dir.join(name));
            }
        }
        out.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        Ok(out)
    }

    /// Parse one entry file; the id is the file stem.
    pub fn read(&self, file: &Utf8Path) -> Result<ChangeEvent, EngineError> {
        let bytes = std::fs::read(file).map_err(|e| EngineError::io(file, e))?;
        let entry: Entry = serde_json::from_slice(&bytes)
            .map_err(|e| EngineError::Queue(format!("{file}: {e}")))?;
        if entry.v != ENTRY_VERSION {
            return Err(EngineError::Queue(format!(
                "{file}: unsupported inbox entry version {}",
                entry.v
            )));
        }
        let id = file.file_stem().unwrap_or_default().to_string();
        Ok(ChangeEvent {
            id,
            context: entry.context,
            repo: entry.repo,
            path: entry.path,
            kind: entry.kind,
        })
    }

    /// Delete entry files (already ingested). A missing file is fine.
    pub fn remove(&self, files: &[Utf8PathBuf]) -> Result<(), EngineError> {
        fail!("before_inbox_delete");
        for f in files {
            match std::fs::remove_file(f) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(EngineError::io(f, e)),
            }
        }
        Ok(())
    }

    /// Set an unreadable entry aside (`<name>.bad`) so it stops being listed.
    pub fn set_aside(&self, file: &Utf8Path) -> Result<(), EngineError> {
        let to = Utf8PathBuf::from(format!("{file}.bad"));
        std::fs::rename(file, &to).map_err(|e| EngineError::io(file, e))
    }
}

fn new_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(nanos);
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    format!(
        "{nanos:020}-{}-{:08x}",
        std::process::id(),
        h.finish() as u32
    )
}

fn sync_dir(dir: &Utf8Path) -> Result<(), EngineError> {
    std::fs::File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(|e| EngineError::io(dir, e))
}
