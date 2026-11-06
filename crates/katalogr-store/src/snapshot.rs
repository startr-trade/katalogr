//! Graph snapshot file (FEAT-097): a two-line text header and one LZ4 frame holding the
//! compacted database file.
//!
//! ```text
//! katalogr-snapshot 1\n
//! {"bytes":..,"cache_schema":..,"digest":"..","facts_version":"..","format":1,"positions":[..]}\n
//! <one LZ4 frame>
//! ```
//!
//! The digest is the SHA-256 of the decoded database. It finds damage, not tampering: it sits in
//! the same file. Nothing here names a language, format or protocol.

use crate::error::StoreError;
use camino::{Utf8Path, Utf8PathBuf};
use lz4_flex::frame::{FrameDecoder, FrameEncoder};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};

pub const SNAPSHOT_MAGIC: &str = "katalogr-snapshot";
pub const SNAPSHOT_FORMAT: u32 = 1;
pub const SNAPSHOT_SUFFIX: &str = ".ksnap";
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

/// File name of the decode target beside `index.redb`.
pub const IMPORT_FILE: &str = "index.redb.import";

const CACHE_FILE: &str = "index.redb";
const CHUNK: usize = 1024 * 1024;

/// One repo's position, in the same shape as `PositionRef` of katalogr-query, which this crate
/// does not depend on. `dirty` is always null in a snapshot.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct SnapshotPosition {
    pub repo: String,
    pub commit: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SnapshotManifest {
    pub format: u32,
    /// Bytes of the decoded database.
    pub bytes: u64,
    /// Sorted by repo.
    pub positions: Vec<SnapshotPosition>,
    pub facts_version: String,
    pub cache_schema: u32,
    /// 64 lower-case hex: the SHA-256 of the decoded database.
    pub digest: String,
}

fn bad(msg: impl Into<String>) -> StoreError {
    StoreError::BadSnapshot(msg.into())
}

fn is_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Compact JSON with every object's keys sorted, whatever the `serde_json` map is.
fn write_compact_sorted(v: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match v {
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_compact_sorted(x, out);
            }
            out.push(']');
        }
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).expect("string"));
                out.push(':');
                write_compact_sorted(&m[k.as_str()], out);
            }
            out.push('}');
        }
        other => out.push_str(&serde_json::to_string(other).expect("value")),
    }
}

/// The two header lines, each ending in `\n`.
fn header_bytes(m: &SnapshotManifest) -> Result<Vec<u8>, StoreError> {
    let v = serde_json::to_value(m).map_err(|e| bad(e.to_string()))?;
    let mut line = String::new();
    write_compact_sorted(&v, &mut line);
    if line.len() > MAX_MANIFEST_BYTES {
        return Err(bad("the manifest is too large"));
    }
    Ok(format!("{SNAPSHOT_MAGIC} {SNAPSHOT_FORMAT}\n{line}\n").into_bytes())
}

/// Reads one `\n`-terminated line of at most `limit` bytes (the terminator excluded).
fn read_line_limited(r: &mut impl BufRead, limit: usize) -> Result<String, StoreError> {
    let mut buf = Vec::new();
    let n = r
        .take(limit as u64 + 1)
        .read_until(b'\n', &mut buf)
        .map_err(|e| bad(format!("unreadable header ({:?})", e.kind())))?;
    if n == 0 || buf.last() != Some(&b'\n') {
        return Err(bad(if buf.len() > limit {
            "a header line is over the limit"
        } else {
            "the header is truncated"
        }));
    }
    buf.pop();
    String::from_utf8(buf).map_err(|_| bad("the header is not text"))
}

/// Parses both header lines from `r`, leaving it at the first payload byte.
fn parse_header(r: &mut impl BufRead) -> Result<SnapshotManifest, StoreError> {
    let first = read_line_limited(r, 64)?;
    let number = first
        .strip_prefix(SNAPSHOT_MAGIC)
        .and_then(|s| s.strip_prefix(' '))
        .ok_or_else(|| bad("wrong magic"))?;
    let format: u32 = number.parse().map_err(|_| bad("wrong magic"))?;
    if format > SNAPSHOT_FORMAT {
        return Err(StoreError::SnapshotFormat(format));
    }
    let line = read_line_limited(r, MAX_MANIFEST_BYTES)?;
    let m: SnapshotManifest =
        serde_json::from_str(&line).map_err(|e| bad(format!("unparsable manifest: {e}")))?;
    if m.format != format {
        return Err(bad("the manifest format differs from the header's"));
    }
    if !is_digest(&m.digest) {
        return Err(bad("the digest is not 64 lower-case hex digits"));
    }
    Ok(m)
}

/// Reads and checks the two header lines. Never reads the payload. Errors: `BadSnapshot` for a
/// wrong magic, a manifest line over the limit, unparsable JSON or a bad digest shape;
/// `SnapshotFormat` for a format above `SNAPSHOT_FORMAT`.
pub fn read_manifest(path: &Utf8Path) -> Result<SnapshotManifest, StoreError> {
    let f = File::open(path).map_err(|e| StoreError::io(path, e))?;
    parse_header(&mut BufReader::new(f))
}

fn sync_dir(dir: &Utf8Path) -> Result<(), StoreError> {
    let d = if dir.as_str().is_empty() {
        Utf8Path::new(".")
    } else {
        dir
    };
    File::open(d)
        .and_then(|f| f.sync_all())
        .map_err(|e| StoreError::io(d, e))
}

/// Hashes `database` to fill `digest` and `bytes`, then writes header and payload to a temp file
/// beside `out`, syncs it and renames it. `manifest.digest` and `.bytes` are overwritten.
/// Returns the finished manifest. On any error no file is left at `out`.
pub fn write_snapshot(
    database: &Utf8Path,
    mut manifest: SnapshotManifest,
    out: &Utf8Path,
) -> Result<SnapshotManifest, StoreError> {
    manifest.format = SNAPSHOT_FORMAT;
    manifest.positions.sort();
    let (digest, bytes) = hash_file(database)?;
    manifest.digest = digest;
    manifest.bytes = bytes;
    let header = header_bytes(&manifest)?;
    let tmp = Utf8PathBuf::from(format!("{out}.part"));
    let result = (|| -> Result<(), StoreError> {
        let mut src = BufReader::with_capacity(
            CHUNK,
            File::open(database).map_err(|e| StoreError::io(database, e))?,
        );
        let file = File::create(&tmp).map_err(|e| StoreError::io(&tmp, e))?;
        let mut w = BufWriter::with_capacity(CHUNK, file);
        w.write_all(&header).map_err(|e| StoreError::io(&tmp, e))?;
        let mut enc = FrameEncoder::new(w);
        std::io::copy(&mut src, &mut enc).map_err(|e| StoreError::io(&tmp, e))?;
        let w = enc
            .finish()
            .map_err(|e| bad(format!("compression failed: {e}")))?;
        let file = w
            .into_inner()
            .map_err(|e| StoreError::io(&tmp, e.into_error()))?;
        file.sync_all().map_err(|e| StoreError::io(&tmp, e))?;
        std::fs::rename(&tmp, out).map_err(|e| StoreError::io(out, e))?;
        if let Some(dir) = out.parent() {
            sync_dir(dir)?;
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(manifest)
}

fn hash_file(path: &Utf8Path) -> Result<(String, u64), StoreError> {
    let mut f = File::open(path).map_err(|e| StoreError::io(path, e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(|e| StoreError::io(path, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hex(&h.finalize()), total))
}

/// Decodes the payload of `path` to `target`, hashing as it goes. Stops with `BadSnapshot` as soon
/// as output passes `manifest.bytes`, and on a short result, a bad frame or a wrong digest;
/// `target` is removed then. Syncs `target` before returning.
pub fn decode_snapshot(
    path: &Utf8Path,
    manifest: &SnapshotManifest,
    target: &Utf8Path,
) -> Result<(), StoreError> {
    let result = decode_inner(path, manifest, target);
    if result.is_err() {
        let _ = std::fs::remove_file(target);
    }
    result
}

fn decode_inner(
    path: &Utf8Path,
    manifest: &SnapshotManifest,
    target: &Utf8Path,
) -> Result<(), StoreError> {
    let f = File::open(path).map_err(|e| StoreError::io(path, e))?;
    let mut reader = BufReader::with_capacity(CHUNK, f);
    // The payload follows the header this manifest came from; the header is read again so a
    // manifest handed in by a caller cannot move the start.
    parse_header(&mut reader)?;
    let mut dec = FrameDecoder::new(reader);
    let out = File::create(target).map_err(|e| StoreError::io(target, e))?;
    let mut w = BufWriter::with_capacity(CHUNK, out);
    let mut h = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        let n = dec
            .read(&mut buf)
            .map_err(|e| bad(format!("damaged payload: {e}")))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > manifest.bytes {
            return Err(bad("the payload is larger than the manifest says"));
        }
        h.update(&buf[..n]);
        w.write_all(&buf[..n])
            .map_err(|e| StoreError::io(target, e))?;
    }
    if total != manifest.bytes {
        return Err(bad("the payload is shorter than the manifest says"));
    }
    if hex(&h.finalize()) != manifest.digest {
        return Err(bad("the digest does not match the payload"));
    }
    let file = w
        .into_inner()
        .map_err(|e| StoreError::io(target, e.into_error()))?;
    file.sync_all().map_err(|e| StoreError::io(target, e))
}

/// Removes a leftover `index.redb.import` in `cache_dir`, decodes to it with `decode_snapshot`,
/// then renames it over `index.redb` and syncs the folder. The caller holds the processor lock.
/// Failpoints `snapshot_before_rename` and `snapshot_after_rename` sit either side of the rename.
pub fn install_snapshot(
    path: &Utf8Path,
    manifest: &SnapshotManifest,
    cache_dir: &Utf8Path,
) -> Result<(), StoreError> {
    let tmp = cache_dir.join(IMPORT_FILE);
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(StoreError::io(&tmp, e)),
    }
    decode_snapshot(path, manifest, &tmp)?;
    #[cfg(feature = "failpoints")]
    crate::failpoint::hit("snapshot_before_rename");
    if let Err(e) = crate::compact::swap(&tmp, &cache_dir.join(CACHE_FILE)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(StoreError::io(cache_dir, e));
    }
    #[cfg(feature = "failpoints")]
    crate::failpoint::hit("snapshot_after_rename");
    Ok(())
}
