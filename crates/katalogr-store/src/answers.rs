//! Stored answers (FEAT-096, ADR-0037): the exact payload of a computed answer, keyed by view ID.
//!
//! An entry is one line of compact JSON (`AnswerHeader`), a newline, then `payload_len` bytes.
//! The store is neutral: it keeps bytes with a small header and knows nothing of what they mean.

use crate::StoreError;
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// The entry format written and accepted.
pub const ANSWER_FORMAT: u32 = 1;
/// The largest payload stored.
pub const MAX_ANSWER_BYTES: u64 = 64 * 1024 * 1024;

const EXT: &str = "answer";
const ID_PREFIX: &str = "v1-";
const ID_HEX: usize = 32;
const TEMP_PREFIX: &str = ".tmp-";
const TEMP_MAX_AGE: Duration = Duration::from_secs(3600);

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// The line that starts every entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerHeader {
    pub format: u32,
    pub view_id: String,
    /// Hex SHA-256 of what must also match for the payload to be served.
    pub validity: String,
    pub op: String,
    pub args: BTreeMap<String, String>,
    pub payload_len: u64,
}

/// A header and its payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredAnswer {
    pub header: AnswerHeader,
    pub payload: Vec<u8>,
}

impl StoredAnswer {
    /// Sets `format` and `payload_len`.
    pub fn new(
        view_id: String,
        validity: String,
        op: String,
        args: BTreeMap<String, String>,
        payload: Vec<u8>,
    ) -> StoredAnswer {
        StoredAnswer {
            header: AnswerHeader {
                format: ANSWER_FORMAT,
                view_id,
                validity,
                op,
                args,
                payload_len: payload.len() as u64,
            },
            payload,
        }
    }
}

/// One entry as listed: its header and its size on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnswerListing {
    pub header: AnswerHeader,
    pub bytes: u64,
}

/// What `put` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PutOutcome {
    Stored,
    TooLarge,
}

/// What `prune` did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub removed: u64,
    pub bytes_left: u64,
}

/// Where stored answers live. Every method is safe to call from many processes at once. A lost
/// entry only costs a recomputation.
pub trait AnswerStore: Send + Sync {
    /// The whole entry, or `None` when absent, truncated, undecodable or of another format.
    /// An unusable entry is removed, best effort. It checks length and header only.
    fn get(&self, view_id: &str) -> Result<Option<StoredAnswer>, StoreError>;
    /// Writes or replaces whole. `TooLarge` over `MAX_ANSWER_BYTES`, with nothing written.
    fn put(&self, answer: &StoredAnswer) -> Result<PutOutcome, StoreError>;
    /// Absent is fine.
    fn remove(&self, view_id: &str) -> Result<(), StoreError>;
    /// Every readable entry's header and size, sorted by view ID.
    fn list(&self) -> Result<Vec<AnswerListing>, StoreError>;
    /// Removes oldest first, then by view ID, until at most `max_bytes` remain.
    fn prune(&self, max_bytes: u64) -> Result<PruneReport, StoreError>;
}

/// `v1-` and 32 lower-case hex digits: the only IDs that name a file.
fn check_id(id: &str) -> Result<(), StoreError> {
    let ok = id.strip_prefix(ID_PREFIX).is_some_and(|h| {
        h.len() == ID_HEX && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    });
    if ok {
        Ok(())
    } else {
        Err(StoreError::Row(format!("{id:?} is not a view ID")))
    }
}

/// One file per entry under `root`, created when first needed.
pub struct DirAnswerStore {
    root: Utf8PathBuf,
}

/// An entry found on disk.
struct Found {
    path: Utf8PathBuf,
    header: AnswerHeader,
    size: u64,
    modified: SystemTime,
}

impl DirAnswerStore {
    pub fn new(root: Utf8PathBuf) -> DirAnswerStore {
        DirAnswerStore { root }
    }

    fn path_of(&self, id: &str) -> Utf8PathBuf {
        self.root
            .join(&id[ID_PREFIX.len()..ID_PREFIX.len() + 2])
            .join(format!("{id}.{EXT}"))
    }

    /// Parses the header line and checks the length against `size`.
    fn decode(bytes: &[u8]) -> Option<(AnswerHeader, usize)> {
        let nl = bytes.iter().position(|b| *b == b'\n')?;
        let header: AnswerHeader = serde_json::from_slice(&bytes[..nl]).ok()?;
        if header.format != ANSWER_FORMAT {
            return None;
        }
        let body = bytes.len() - nl - 1;
        (header.payload_len == body as u64).then_some((header, nl + 1))
    }

    /// Entries under both levels; unreadable ones are left alone.
    fn scan(&self) -> Vec<Found> {
        let mut out = Vec::new();
        let Ok(buckets) = std::fs::read_dir(&self.root) else {
            return out;
        };
        for bucket in buckets.flatten() {
            let Ok(files) = std::fs::read_dir(bucket.path()) else {
                continue;
            };
            for f in files.flatten() {
                let Ok(path) = Utf8PathBuf::from_path_buf(f.path()) else {
                    continue;
                };
                if path.extension() != Some(EXT) {
                    continue;
                }
                let Ok(meta) = f.metadata() else { continue };
                let Some(header) = read_header(&path) else {
                    continue;
                };
                // A header whose ID does not match its name is not this entry's.
                if path.file_stem() != Some(header.view_id.as_str()) {
                    continue;
                }
                out.push(Found {
                    path,
                    header,
                    size: meta.len(),
                    modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                });
            }
        }
        out.sort_by(|a, b| a.header.view_id.cmp(&b.header.view_id));
        out
    }

    fn sweep_temps(&self) {
        let Ok(buckets) = std::fs::read_dir(&self.root) else {
            return;
        };
        for bucket in buckets.flatten() {
            let Ok(files) = std::fs::read_dir(bucket.path()) else {
                continue;
            };
            for f in files.flatten() {
                if !f.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
                    continue;
                }
                let old = f
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|d| d > TEMP_MAX_AGE);
                if old {
                    let _ = std::fs::remove_file(f.path());
                }
            }
        }
    }
}

/// Reads only up to the first newline.
fn read_header(path: &Utf8Path) -> Option<AnswerHeader> {
    use std::io::BufRead;
    let f = std::fs::File::open(path).ok()?;
    let mut line = Vec::new();
    std::io::BufReader::new(f)
        .read_until(b'\n', &mut line)
        .ok()?;
    if line.pop() != Some(b'\n') {
        return None;
    }
    let h: AnswerHeader = serde_json::from_slice(&line).ok()?;
    (h.format == ANSWER_FORMAT).then_some(h)
}

impl AnswerStore for DirAnswerStore {
    fn get(&self, view_id: &str) -> Result<Option<StoredAnswer>, StoreError> {
        check_id(view_id)?;
        let path = self.path_of(view_id);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StoreError::io(&path, e)),
        };
        match Self::decode(&bytes) {
            Some((header, at)) if header.view_id == view_id => Ok(Some(StoredAnswer {
                header,
                payload: bytes[at..].to_vec(),
            })),
            _ => {
                let _ = std::fs::remove_file(&path);
                Ok(None)
            }
        }
    }

    fn put(&self, answer: &StoredAnswer) -> Result<PutOutcome, StoreError> {
        let id = &answer.header.view_id;
        check_id(id)?;
        if answer.payload.len() as u64 > MAX_ANSWER_BYTES {
            return Ok(PutOutcome::TooLarge);
        }
        let mut header = answer.header.clone();
        header.format = ANSWER_FORMAT;
        header.payload_len = answer.payload.len() as u64;
        let mut line = serde_json::to_vec(&header)
            .map_err(|e| StoreError::Row(format!("answer header: {e}")))?;
        line.push(b'\n');
        let path = self.path_of(id);
        let dir = path.parent().expect("an entry has a folder");
        std::fs::create_dir_all(dir).map_err(|e| StoreError::io(dir, e))?;
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let temp = dir.join(format!(
            "{TEMP_PREFIX}{}-{nanos}-{}",
            std::process::id(),
            TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let written = (|| {
            let mut f = std::fs::File::create(&temp)?;
            f.write_all(&line)?;
            f.write_all(&answer.payload)?;
            std::fs::rename(&temp, &path)
        })();
        if let Err(e) = written {
            let _ = std::fs::remove_file(&temp);
            return Err(StoreError::io(&path, e));
        }
        Ok(PutOutcome::Stored)
    }

    fn remove(&self, view_id: &str) -> Result<(), StoreError> {
        check_id(view_id)?;
        let path = self.path_of(view_id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StoreError::io(&path, e)),
        }
    }

    fn list(&self) -> Result<Vec<AnswerListing>, StoreError> {
        Ok(self
            .scan()
            .into_iter()
            .map(|f| AnswerListing {
                header: f.header,
                bytes: f.size,
            })
            .collect())
    }

    fn prune(&self, max_bytes: u64) -> Result<PruneReport, StoreError> {
        self.sweep_temps();
        let mut entries = self.scan();
        let mut total: u64 = entries.iter().map(|e| e.size).sum();
        entries
            .sort_by(|a, b| (a.modified, &a.header.view_id).cmp(&(b.modified, &b.header.view_id)));
        let mut removed = 0;
        for e in entries {
            if total <= max_bytes {
                break;
            }
            match std::fs::remove_file(&e.path) {
                Ok(()) => removed += 1,
                // Another process removed it first.
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(StoreError::io(&e.path, err)),
            }
            total -= e.size;
        }
        Ok(PruneReport {
            removed,
            bytes_left: total,
        })
    }
}
