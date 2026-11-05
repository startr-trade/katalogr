//! Reading a checkout's position: `HEAD` plus the CD-2 hash of its uncommitted changes
//! (FEAT-084; chain decision CD-2 of FEAT-043).
//!
//! The hash covers every path whose raw worktree bytes differ from its blob in the `HEAD` tree,
//! plus every untracked path that is not ignored. Each contributes the line `<path>\0<oid>\n`,
//! where `oid` is the raw-bytes git blob id or `-` for a deleted path. Lines are sorted by bytes
//! and hashed with SHA-256. The index and file modes play no part.

use crate::scan::{ScanError, list_files};
use gix::bstr::{BString, ByteSlice};
use katalogr_model::RelPath;
use katalogr_query::PositionRef;
use katalogr_store::blob::git_blob_id;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// A checkout's position and the paths that make up its dirty part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkingState {
    pub position: PositionRef,
    /// Each uncommitted path with its raw-bytes blob id, or `None` when it is deleted. Sorted by
    /// path. Paths that are not valid [`RelPath`]s count in the hash but are not listed.
    pub changed: Vec<(RelPath, Option<String>)>,
}

fn io_err(path: &Path, e: impl std::fmt::Display) -> ScanError {
    ScanError::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    }
}

fn diff_err(e: impl std::fmt::Display) -> ScanError {
    ScanError::Diff(e.to_string())
}

/// The raw bytes git would store for the path as a blob: the file's bytes, or a symbolic link's
/// target. `None` when nothing regular is there (missing, a directory, or unreadable as a file).
fn raw_bytes(root: &Path, rela: &[u8]) -> Option<Vec<u8>> {
    let rela = gix::path::from_byte_slice(rela).ok()?;
    let full = root.join(rela);
    let meta = std::fs::symlink_metadata(&full).ok()?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(&full).ok()?;
        return Some(gix::path::into_bstr(target).ok()?.as_bytes().to_vec());
    }
    if meta.is_file() {
        return std::fs::read(&full).ok();
    }
    None
}

/// `HEAD` plus the CD-2 hash of the checkout at `root`. A directory with no repository hashes
/// every file the repo's globs admit, with commit `None`. An unborn `HEAD` counts as commit
/// `None`, with every file that is not ignored counted as untracked.
pub fn read_position(
    repo: &str,
    root: &Path,
    include: &[String],
    exclude: &[String],
) -> Result<WorkingState, ScanError> {
    // `.git` is a directory, or a file in a linked worktree; without it this is a plain directory.
    if !root.join(".git").exists() {
        return plain_directory(repo, root, include, exclude);
    }
    let git = gix::open(root).map_err(|e| ScanError::RepoOpen {
        path: root.display().to_string(),
        message: e.to_string(),
    })?;

    // The HEAD tree: path -> blob id (hex), for regular files and symbolic links.
    let mut head: BTreeMap<Vec<u8>, String> = BTreeMap::new();
    let commit = match git.head_commit() {
        Ok(c) => {
            let tree = c.tree().map_err(diff_err)?;
            let mut rec = gix::traverse::tree::Recorder::default();
            tree.traverse().breadthfirst(&mut rec).map_err(diff_err)?;
            for e in rec.records {
                // Trees need no entry; a submodule is only its gitlink and is not hashed.
                if e.mode.is_blob() || e.mode.is_link() {
                    head.insert(e.filepath.to_vec(), e.oid.to_hex().to_string());
                }
            }
            Some(c.id.to_hex().to_string())
        }
        Err(_) => None,
    };

    let mut lines: BTreeMap<Vec<u8>, Option<String>> = BTreeMap::new();
    for (path, blob) in &head {
        match raw_bytes(root, path) {
            Some(bytes) => {
                let id = git_blob_id(&bytes);
                if &id != blob {
                    lines.insert(path.clone(), Some(id));
                }
            }
            None => {
                lines.insert(path.clone(), None);
            }
        }
    }

    // Untracked and not ignored, as the status walk reports them.
    let mut candidates: BTreeSet<Vec<u8>> = BTreeSet::new();
    let platform = git
        .status(gix::progress::Discard)
        .map_err(diff_err)?
        .untracked_files(gix::status::UntrackedFiles::Files)
        .index_worktree_rewrites(None);
    let iter = platform
        .into_index_worktree_iter(Vec::<BString>::new())
        .map_err(diff_err)?;
    for item in iter {
        let item = item.map_err(diff_err)?;
        if let gix::status::index_worktree::Item::DirectoryContents { entry, .. } = item
            && entry.status == gix::dir::entry::Status::Untracked
        {
            candidates.insert(entry.rela_path.to_vec());
        }
    }
    // A path staged as new is in the index but not in HEAD: its bytes differ from "no blob".
    let index = git.index_or_empty().map_err(diff_err)?;
    for e in index.entries() {
        let path = e.path(&index).to_vec();
        if !head.contains_key(&path) && !e.mode.is_submodule() {
            candidates.insert(path);
        }
    }
    for path in candidates {
        if head.contains_key(&path) {
            continue;
        }
        if let Some(bytes) = raw_bytes(root, &path) {
            lines.insert(path, Some(git_blob_id(&bytes)));
        }
    }

    Ok(state(repo, commit, lines))
}

fn plain_directory(
    repo: &str,
    root: &Path,
    include: &[String],
    exclude: &[String],
) -> Result<WorkingState, ScanError> {
    let mut lines = BTreeMap::new();
    for rel in list_files(root, include, exclude)? {
        let full = root.join(rel.as_str());
        let bytes = std::fs::read(&full).map_err(|e| io_err(&full, e))?;
        lines.insert(rel.as_str().as_bytes().to_vec(), Some(git_blob_id(&bytes)));
    }
    Ok(state(repo, None, lines))
}

fn state(
    repo: &str,
    commit: Option<String>,
    lines: BTreeMap<Vec<u8>, Option<String>>,
) -> WorkingState {
    let dirty = dirty_hash(lines.iter().map(|(p, o)| (p.as_slice(), o.as_deref())));
    let changed = lines
        .into_iter()
        .filter_map(|(p, o)| {
            let rel = RelPath::new(std::str::from_utf8(&p).ok()?).ok()?;
            Some((rel, o))
        })
        .collect();
    WorkingState {
        position: PositionRef {
            repo: repo.to_string(),
            commit,
            dirty,
        },
        changed,
    }
}

/// The CD-2 hash of `(path, blob id or None for deleted)` pairs, 64 lower-case hex, or `None`
/// for an empty set. The input order does not matter.
pub fn dirty_hash<'a>(
    entries: impl IntoIterator<Item = (&'a [u8], Option<&'a str>)>,
) -> Option<String> {
    let mut lines: Vec<Vec<u8>> = entries
        .into_iter()
        .map(|(path, oid)| {
            let mut l = path.to_vec();
            l.push(0);
            l.extend_from_slice(oid.unwrap_or("-").as_bytes());
            l.push(b'\n');
            l
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    lines.sort();
    let mut h = Sha256::new();
    for l in &lines {
        h.update(l);
    }
    Some(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
