//! Reading a checkout's position: `HEAD` plus the CD-2 hash of its uncommitted changes
//! (FEAT-084; chain decision CD-2 of FEAT-043).
//!
//! The hash covers every path whose worktree bytes, in their repository form, differ from its
//! blob in the `HEAD` tree, plus every untracked path that is not ignored. Each contributes the
//! line `<path>\0<oid>\n`, where `oid` is the git blob id of the repository-form bytes or `-` for
//! a deleted path. Lines are sorted by bytes and hashed with SHA-256. The index and file modes
//! play no part.
//!
//! The repository form (FEAT-141) is what `git add` would store: the to-git conversion of
//! [`katalogr_store::repo_form::RepoForm`] (`core.autocrlf`, `core.eol`, the `text`, `eol` and
//! `crlf` attributes). A file whose raw bytes already equal its blob is unchanged without being
//! converted, so on a checkout with no conversion every id is the raw-bytes id, as before.

use crate::scan::{ScanError, list_files};
use gix::bstr::{BString, ByteSlice};
use katalogr_model::RelPath;
use katalogr_query::PositionRef;
use katalogr_store::blob::git_blob_id;
use katalogr_store::repo_form::{RepoForm, settings_key};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A checkout's position and the paths that make up its dirty part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkingState {
    pub position: PositionRef,
    /// Each uncommitted path with its repository-form blob id, or `None` when it is deleted. Sorted by
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

/// The raw bytes at the path: the file's bytes, or a symbolic link's target (with `true`).
/// `None` when nothing regular is there (missing, a directory, or unreadable as a file).
fn raw_bytes(root: &Path, rela: &[u8]) -> Option<(Vec<u8>, bool)> {
    let rela = gix::path::from_byte_slice(rela).ok()?;
    let full = root.join(rela);
    let meta = std::fs::symlink_metadata(&full).ok()?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(&full).ok()?;
        return Some((gix::path::into_bstr(target).ok()?.as_bytes().to_vec(), true));
    }
    if meta.is_file() {
        return std::fs::read(&full).ok().map(|b| (b, false));
    }
    None
}

/// The blob id git would store for the path: a link's target as it is, a file's bytes in their
/// repository form. `None` when nothing regular is there.
fn repo_blob_id(form: &mut RepoForm<'_>, root: &Path, rela: &str) -> Option<String> {
    let (bytes, link) = raw_bytes(root, rela.as_bytes())?;
    Some(if link {
        git_blob_id(&bytes)
    } else {
        form.blob_id(rela, &bytes)
    })
}

/// The key the stat cache's ids depend on: the conversion settings and the stat of every tracked
/// `.gitattributes` file.
fn filter_key(
    git: &gix::Repository,
    root: &Path,
    tracked: &BTreeMap<String, TrackedRow>,
) -> String {
    let mut key = settings_key(git);
    for path in tracked.keys() {
        if path == ".gitattributes" || path.ends_with("/.gitattributes") {
            let s = stat_of(&root.join(path)).map(|s| (s.size, s.mtime_ns));
            key.push_str(&format!("{path}={s:?}\n"));
        }
    }
    key
}

/// The first, uncached implementation: hashes every tracked file and asks `gix` status for the
/// untracked ones. Kept as the reference the cached reader is tested against.
#[doc(hidden)]
pub fn reference_position(
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

    let mut form = RepoForm::new(&git);
    let mut lines: BTreeMap<Vec<u8>, Option<String>> = BTreeMap::new();
    for (path, blob) in &head {
        match raw_bytes(root, path) {
            Some((bytes, link)) => {
                let mut id = git_blob_id(&bytes);
                if &id != blob
                    && !link
                    && let Ok(rela) = std::str::from_utf8(path)
                {
                    id = form.blob_id(rela, &bytes);
                }
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
        if let Some((bytes, link)) = raw_bytes(root, &path) {
            let id = match std::str::from_utf8(&path) {
                Ok(rela) if !link => form.blob_id(rela, &bytes),
                _ => git_blob_id(&bytes),
            };
            lines.insert(path, Some(id));
        }
    }

    Ok(state(repo, commit, lines))
}

/// FEAT-124. The blob ids of the files of plain folders already read by this process, so a
/// file is read again only when its size or modification time changed. An entry is trusted only
/// when its mtime is older than the moment it was read by `RACY_NS` (a later write within the
/// clock's granularity would leave the stat unchanged).
#[derive(Clone)]
struct PlainRow {
    len: u64,
    mtime_ns: u64,
    read_ns: u64,
    blob: String,
}

type PlainCache = BTreeMap<std::path::PathBuf, BTreeMap<String, PlainRow>>;

fn plain_cache() -> &'static std::sync::Mutex<PlainCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<PlainCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn plain_directory(
    repo: &str,
    root: &Path,
    include: &[String],
    exclude: &[String],
) -> Result<WorkingState, ScanError> {
    let mut held = plain_cache()
        .lock()
        .map(|mut c| c.remove(root).unwrap_or_default())
        .unwrap_or_default();
    let mut next: BTreeMap<String, PlainRow> = BTreeMap::new();
    let mut lines = BTreeMap::new();
    for rel in list_files(root, include, exclude)? {
        let full = root.join(rel.as_str());
        let stat = std::fs::metadata(&full).ok().and_then(|m| {
            let mtime = m
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?;
            Some((m.len(), mtime.as_nanos() as u64))
        });
        let kept = stat.and_then(|(len, mtime_ns)| {
            held.remove(rel.as_str()).filter(|r| {
                r.len == len
                    && r.mtime_ns == mtime_ns
                    && mtime_ns.saturating_add(RACY_NS) < r.read_ns
            })
        });
        let row = match kept {
            Some(r) => r,
            None => {
                let read_ns = now_ns();
                let bytes = std::fs::read(&full).map_err(|e| io_err(&full, e))?;
                let (len, mtime_ns) = stat.unwrap_or((bytes.len() as u64, read_ns));
                PlainRow {
                    len,
                    mtime_ns,
                    read_ns,
                    blob: git_blob_id(&bytes),
                }
            }
        };
        lines.insert(rel.as_str().as_bytes().to_vec(), Some(row.blob.clone()));
        next.insert(rel.as_str().to_string(), row);
    }
    if let Ok(mut c) = plain_cache().lock() {
        c.insert(root.to_path_buf(), next);
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

/// Benchmark comparison only: the number of paths `gix` status reports (tracked changes and
/// untracked files) using the index's stat cache, without hashing anything. It is what a
/// position built on status alone would start from.
#[doc(hidden)]
pub fn status_path_count(root: &Path) -> Result<usize, ScanError> {
    let git = gix::open(root).map_err(|e| ScanError::RepoOpen {
        path: root.display().to_string(),
        message: e.to_string(),
    })?;
    let iter = git
        .status(gix::progress::Discard)
        .map_err(diff_err)?
        .untracked_files(gix::status::UntrackedFiles::Files)
        .index_worktree_rewrites(None)
        .into_index_worktree_iter(Vec::<BString>::new())
        .map_err(diff_err)?;
    let mut n = 0;
    for item in iter {
        item.map_err(diff_err)?;
        n += 1;
    }
    Ok(n)
}

/// Adds `/<rel>` to the checkout's `info/exclude` (the common dir's, for a linked worktree) unless
/// `rel` is tracked or a line for it is already there; other lines are left alone. `Ok(true)`
/// when a line was appended; `Ok(false)` for a plain directory or nothing to do.
pub fn exclude_untracked(root: &Path, rel: &str) -> Result<bool, ScanError> {
    if !root.join(".git").exists() {
        return Ok(false);
    }
    let git = gix::open(root).map_err(|e| ScanError::RepoOpen {
        path: root.display().to_string(),
        message: e.to_string(),
    })?;
    let index = git.index_or_empty().map_err(diff_err)?;
    if index.entry_by_path(rel.into()).is_some() {
        return Ok(false);
    }
    let file = git.common_dir().join("info/exclude");
    let old = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(io_err(&file, e)),
    };
    let anchored = format!("/{rel}");
    if old.lines().any(|l| l.trim() == rel || l.trim() == anchored) {
        return Ok(false);
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    }
    let mut text = old;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&anchored);
    text.push('\n');
    std::fs::write(&file, text).map_err(|e| io_err(&file, e))?;
    Ok(true)
}

/// The ignore stack of a checkout: every `.gitignore` in the tree, `.git/info/exclude` and
/// `core.excludesFile`. Shared by the position's walk and by `scan::list_files`.
pub(crate) fn excludes_stack<'r>(
    git: &'r gix::Repository,
    index: &gix::worktree::Index,
) -> Result<gix::AttributeStack<'r>, ScanError> {
    git.excludes(
        index,
        None,
        gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
    )
    .map_err(diff_err)
}

/// Whether git ignores `rel` (repo-relative, '/'-separated) according to `stack`.
pub(crate) fn stack_excluded(
    stack: &mut gix::AttributeStack<'_>,
    rel: &str,
    is_dir: bool,
) -> Result<bool, ScanError> {
    let mode = if is_dir {
        gix::index::entry::Mode::DIR
    } else {
        gix::index::entry::Mode::FILE
    };
    let platform = stack
        .at_path(rel, Some(mode))
        .map_err(|e| io_err(Path::new(rel), e))?;
    Ok(platform.is_excluded())
}

// ---------------------------------------------------------------------------------------------
// The cached reader (FEAT-084 revision: a stat cache for the position).
// ---------------------------------------------------------------------------------------------

/// Files hashed by the cached reader since the process began; for tests.
static HASHED: AtomicUsize = AtomicUsize::new(0);

/// How many files the cached reader has read and hashed in this process; for tests.
#[doc(hidden)]
pub fn hashed_files() -> usize {
    HASHED.load(Ordering::Relaxed)
}

/// Format version of the stat cache file; a different number discards it.
pub const STAT_CACHE_VERSION: u32 = 1;
/// A stat older than this, before the cache was written, can be trusted (git's racy rule).
const RACY_NS: u64 = 2_000_000_000;

/// `<cache dir>/position/<repo>.stat`.
pub fn stat_cache_path(cache_dir: &Path, repo: &str) -> std::path::PathBuf {
    cache_dir.join("position").join(format!("{repo}.stat"))
}

/// What `lstat` says about a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[doc(hidden)]
pub struct Stat {
    pub size: u64,
    pub mtime_ns: u64,
    pub ctime_ns: u64,
    pub ino: u64,
    pub mode: u32,
}

#[doc(hidden)]
pub fn stat_of(path: &Path) -> Option<Stat> {
    let m = std::fs::symlink_metadata(path).ok()?;
    Some(stat_from(&m))
}

#[cfg(unix)]
fn stat_from(m: &std::fs::Metadata) -> Stat {
    use std::os::unix::fs::MetadataExt;
    let ns = |s: i64, n: i64| (s.max(0) as u64) * 1_000_000_000 + n.max(0) as u64;
    Stat {
        size: m.size(),
        mtime_ns: ns(m.mtime(), m.mtime_nsec()),
        ctime_ns: ns(m.ctime(), m.ctime_nsec()),
        ino: m.ino(),
        mode: m.mode(),
    }
}

#[cfg(not(unix))]
fn stat_from(m: &std::fs::Metadata) -> Stat {
    let mtime_ns = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64);
    Stat {
        size: m.len(),
        mtime_ns,
        ctime_ns: 0,
        ino: 0,
        mode: u32::from(m.is_dir()),
    }
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}

/// The result of comparing one tracked path's repository-form bytes with its `HEAD` blob.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[doc(hidden)]
pub enum Raw {
    Same,
    /// Differs; the repository-form git blob id.
    Differs(String),
    /// Nothing regular is there.
    Deleted,
}

/// One tracked path. `blob` is empty for a path staged as new (no `HEAD` blob).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[doc(hidden)]
pub struct TrackedRow {
    pub blob: String,
    /// `None` until the file has been looked at, and while it is deleted.
    pub stat: Option<Stat>,
    pub raw: Raw,
}

/// An untracked, not ignored file directly in a directory.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[doc(hidden)]
pub struct UntrackedRow {
    pub name: String,
    pub stat: Stat,
    pub id: String,
}

/// One directory of the worktree that is not ignored.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[doc(hidden)]
pub struct DirRow {
    pub mtime_ns: u64,
    pub subdirs: Vec<String>,
    pub files: Vec<UntrackedRow>,
}

/// An ignore input: path (absolute for files outside the worktree), size and mtime; missing is
/// `None`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[doc(hidden)]
pub struct IgnoreInput {
    pub path: String,
    pub stat: Option<(u64, u64)>,
}

/// The stat cache file (see the FEAT-084 revision).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[doc(hidden)]
pub struct StatCache {
    pub version: u32,
    pub root: String,
    pub commit: Option<String>,
    pub written_ns: u64,
    /// The index file as the staged-as-new rows were computed from it.
    pub index_input: Option<(u64, u64)>,
    pub ignore: Vec<IgnoreInput>,
    pub tracked: BTreeMap<String, TrackedRow>,
    pub dirs: BTreeMap<String, DirRow>,
    /// FEAT-141. The conversion settings the ids were computed under (see `filter_key`); a
    /// different key means every file is read again. Absent in older files, which then rebuild.
    #[serde(default)]
    pub filter_key: String,
}

/// Loads the cache; anything unreadable, corrupt or of another version or root is `None`.
#[doc(hidden)]
pub fn load_stat_cache(path: &Path, root: &str) -> Option<StatCache> {
    let bytes = std::fs::read(path).ok()?;
    let c: StatCache = serde_json::from_slice(&bytes).ok()?;
    (c.version == STAT_CACHE_VERSION && c.root == root).then_some(c)
}

fn store_stat_cache(path: &Path, c: &StatCache) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(&serde_json::to_vec(c).map_err(std::io::Error::other)?)?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// The raw-bytes blob id of the path, and whether it is a symbolic link.
fn hash_file(root: &Path, rel: &str) -> Option<(String, bool)> {
    HASHED.fetch_add(1, Ordering::Relaxed);
    raw_bytes(root, rel.as_bytes()).map(|(b, link)| (git_blob_id(&b), link))
}

/// Timings of the steps of one cached read, for the benches.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default)]
pub struct StepTimes {
    pub cache_load: std::time::Duration,
    pub tracked_stats: std::time::Duration,
    pub dir_stats: std::time::Duration,
    pub walk: std::time::Duration,
    pub hash: std::time::Duration,
    pub total: std::time::Duration,
}

/// `HEAD` plus the CD-2 hash of the checkout at `root`. A directory with no repository hashes
/// every file the repo's globs admit, with commit `None`. An unborn `HEAD` counts as commit
/// `None`, with every file that is not ignored counted as untracked.
///
/// This reads with no cache. Use [`read_position_cached`] to keep one.
pub fn read_position(
    repo: &str,
    root: &Path,
    include: &[String],
    exclude: &[String],
) -> Result<WorkingState, ScanError> {
    read_position_cached(repo, root, include, exclude, None)
}

/// Like [`read_position`], remembering the result of each raw-bytes comparison in the stat cache
/// file at `cache` (see [`stat_cache_path`]), so a file is read again only when its stat changes.
/// The cache is written only when something in it changed; a failed write is ignored.
pub fn read_position_cached(
    repo: &str,
    root: &Path,
    include: &[String],
    exclude: &[String],
    cache: Option<&Path>,
) -> Result<WorkingState, ScanError> {
    read_position_timed(repo, root, include, exclude, cache).map(|(s, _)| s)
}

/// [`read_position_cached`] with the time each step took.
#[doc(hidden)]
pub fn read_position_timed(
    repo: &str,
    root: &Path,
    include: &[String],
    exclude: &[String],
    cache: Option<&Path>,
) -> Result<(WorkingState, StepTimes), ScanError> {
    if !root.join(".git").exists() {
        let t = std::time::Instant::now();
        let s = plain_directory(repo, root, include, exclude)?;
        return Ok((
            s,
            StepTimes {
                total: t.elapsed(),
                ..StepTimes::default()
            },
        ));
    }
    let git = gix::open(root).map_err(|e| ScanError::RepoOpen {
        path: root.display().to_string(),
        message: e.to_string(),
    })?;
    let canon = std::fs::canonicalize(root)
        .map_err(|e| io_err(root, e))?
        .display()
        .to_string();
    Reading::new(repo, root, &git, canon, cache).run()
}

struct Reading<'a> {
    repo: &'a str,
    root: &'a Path,
    git: &'a gix::Repository,
    canon: String,
    cache_path: Option<&'a Path>,
    old: Option<StatCache>,
    times: StepTimes,
    changed: bool,
}

impl<'a> Reading<'a> {
    fn new(
        repo: &'a str,
        root: &'a Path,
        git: &'a gix::Repository,
        canon: String,
        cache_path: Option<&'a Path>,
    ) -> Self {
        let t = std::time::Instant::now();
        let old = cache_path.and_then(|p| load_stat_cache(p, &canon));
        let times = StepTimes {
            cache_load: t.elapsed(),
            ..StepTimes::default()
        };
        Reading {
            repo,
            root,
            git,
            canon,
            cache_path,
            old,
            times,
            changed: false,
        }
    }

    fn run(mut self) -> Result<(WorkingState, StepTimes), ScanError> {
        let start = std::time::Instant::now();
        let old_written = self.old.as_ref().map_or(0, |c| c.written_ns);
        let trusted = |mtime_ns: u64| mtime_ns.saturating_add(RACY_NS) < old_written;

        // Step 1: HEAD, and the tracked table.
        let commit = self
            .git
            .head_commit()
            .ok()
            .map(|c| c.id.to_hex().to_string());
        let same_head = self.old.as_ref().is_some_and(|c| c.commit == commit);
        let mut tracked: BTreeMap<String, TrackedRow> = BTreeMap::new();
        if same_head {
            tracked = self
                .old
                .as_ref()
                .map(|c| c.tracked.clone())
                .unwrap_or_default();
        } else {
            self.changed = true;
            let mut carry = self.old.as_mut().map(|c| std::mem::take(&mut c.tracked));
            if let Ok(c) = self.git.head_commit() {
                let tree = c.tree().map_err(diff_err)?;
                let mut rec = gix::traverse::tree::Recorder::default();
                tree.traverse().breadthfirst(&mut rec).map_err(diff_err)?;
                for e in rec.records {
                    if !(e.mode.is_blob() || e.mode.is_link()) {
                        continue;
                    }
                    let Ok(path) = std::str::from_utf8(&e.filepath) else {
                        continue;
                    };
                    let blob = e.oid.to_hex().to_string();
                    let row = match carry.as_mut().and_then(|m| m.remove(path)) {
                        Some(r) if r.blob == blob => r,
                        _ => TrackedRow {
                            blob,
                            stat: None,
                            raw: Raw::Same,
                        },
                    };
                    tracked.insert(path.to_string(), row);
                }
            }
        }

        // Step 3: paths staged as new. Recomputed when HEAD moved or the index file changed.
        let index_path = self.git.index_path();
        let index_input = stat_of(&index_path).map(|s| (s.size, s.mtime_ns));
        let index_same = self
            .old
            .as_ref()
            .is_some_and(|c| c.index_input == index_input);
        if !(same_head && index_same) {
            self.changed = true;
            tracked.retain(|_, r| !r.blob.is_empty());
            let index = self.git.index_or_empty().map_err(diff_err)?;
            for e in index.entries() {
                if e.mode.is_submodule() {
                    continue;
                }
                let Ok(path) = std::str::from_utf8(e.path(&index)) else {
                    continue;
                };
                if !tracked.contains_key(path) {
                    tracked.insert(
                        path.to_string(),
                        TrackedRow {
                            blob: String::new(),
                            stat: None,
                            raw: Raw::Same,
                        },
                    );
                }
            }
        }

        // FEAT-141. Ids computed under other conversion settings are read again.
        let key = filter_key(self.git, self.root, &tracked);
        let key_same = self.old.as_ref().is_some_and(|c| c.filter_key == key);
        if !key_same {
            self.changed = true;
            for row in tracked.values_mut() {
                row.stat = None;
            }
        }

        // Step 2: stat every tracked path; read and hash only what changed or is racy.
        let t = std::time::Instant::now();
        let mut todo: Vec<(String, Option<Stat>)> = Vec::new();
        {
            use rayon::prelude::*;
            let root = self.root;
            // Stats are independent system calls; they run on the pool. `None` below means the
            // row is fresh and needs nothing.
            let looked: Vec<(String, Option<Stat>)> = tracked
                .par_iter()
                .filter_map(|(path, row)| match stat_of(&root.join(path)) {
                    Some(cur) if cur.mode & 0o170000 != 0o040000 => {
                        let fresh = row.stat == Some(cur)
                            && trusted(cur.mtime_ns)
                            && row.raw != Raw::Deleted;
                        (!fresh).then(|| (path.clone(), Some(cur)))
                    }
                    _ => {
                        let already = row.raw == Raw::Deleted && row.stat.is_none();
                        (!already).then(|| (path.clone(), None))
                    }
                })
                .collect();
            for (path, cur) in looked {
                if cur.is_some() {
                    todo.push((path, cur));
                } else {
                    self.changed = true;
                    let row = tracked.get_mut(&path).expect("row");
                    row.stat = None;
                    row.raw = Raw::Deleted;
                }
            }
        }
        self.times.tracked_stats = t.elapsed();
        let t = std::time::Instant::now();
        {
            use rayon::prelude::*;
            let root = self.root;
            let results: Vec<(String, Option<Stat>, Option<(String, bool)>)> = todo
                .into_par_iter()
                .map(|(p, st)| {
                    let id = hash_file(root, &p);
                    (p, st, id)
                })
                .collect();
            // A file whose raw bytes differ from its blob is compared in its repository form; the
            // conversion needs the repository, which is not shared across threads.
            let mut form = RepoForm::new(self.git);
            for (path, st, raw) in results {
                self.changed = true;
                let row = tracked.get_mut(&path).expect("row");
                let id = match raw {
                    Some((id, false)) if id != row.blob => repo_blob_id(&mut form, root, &path),
                    other => other.map(|(id, _)| id),
                };
                match id {
                    None => {
                        row.stat = None;
                        row.raw = Raw::Deleted;
                    }
                    Some(id) => {
                        row.stat = st;
                        row.raw = if id == row.blob {
                            Raw::Same
                        } else {
                            Raw::Differs(id)
                        };
                    }
                }
            }
        }
        self.times.hash += t.elapsed();

        // Step 4: untracked files, through the directory table.
        let old = self.old.take();
        let mut walker = Walker::new(self.git, self.root, old.as_ref(), &tracked, old_written)?;
        walker.rehash = !key_same;
        let t_walk = std::time::Instant::now();
        let hash_before = self.times.hash;
        let (dirs, ignore) = walker.run()?;
        self.times.dir_stats = walker.dir_stat_time;
        self.times.hash += walker.hash_time;
        self.times.walk = t_walk
            .elapsed()
            .saturating_sub(walker.dir_stat_time + walker.hash_time);
        let _ = hash_before;
        if walker.changed {
            self.changed = true;
        }

        // Step 5: the dirty hash from the rows.
        let mut lines: BTreeMap<Vec<u8>, Option<String>> = BTreeMap::new();
        for (path, row) in &tracked {
            match &row.raw {
                Raw::Same => {}
                Raw::Differs(id) => {
                    lines.insert(path.as_bytes().to_vec(), Some(id.clone()));
                }
                Raw::Deleted => {
                    // A path staged as new and then removed is simply absent.
                    if !row.blob.is_empty() {
                        lines.insert(path.as_bytes().to_vec(), None);
                    }
                }
            }
        }
        for (dir, row) in &dirs {
            for f in &row.files {
                let rel = if dir.is_empty() {
                    f.name.clone()
                } else {
                    format!("{dir}/{}", f.name)
                };
                lines.insert(rel.into_bytes(), Some(f.id.clone()));
            }
        }
        if self.changed
            && let Some(p) = self.cache_path
        {
            let new = StatCache {
                version: STAT_CACHE_VERSION,
                root: self.canon.clone(),
                commit: commit.clone(),
                written_ns: now_ns(),
                index_input,
                ignore,
                tracked,
                dirs,
                filter_key: key,
            };
            // A failed write only costs the next read its savings.
            let _ = store_stat_cache(p, &new);
        }
        let mut times = self.times;
        times.total = start.elapsed() + times.cache_load;
        Ok((state(self.repo, commit, lines), times))
    }
}

/// The directory walk of step 4: only directories whose mtime moved are listed again.
struct Walker<'r> {
    git: &'r gix::Repository,
    root: &'r Path,
    old: Option<&'r StatCache>,
    tracked: &'r BTreeMap<String, TrackedRow>,
    old_written: u64,
    /// Every ignore input must be unchanged for a directory to be skipped.
    full: bool,
    stack: Option<gix::AttributeStack<'r>>,
    index: Option<gix::worktree::Index>,
    ignore_seen: Vec<IgnoreInput>,
    dir_stat_time: std::time::Duration,
    hash_time: std::time::Duration,
    changed: bool,
    /// Every untracked file is read again (the conversion settings changed).
    rehash: bool,
    form: RepoForm<'r>,
}

type WalkOut = (BTreeMap<String, DirRow>, Vec<IgnoreInput>);

impl<'r> Walker<'r> {
    fn new(
        git: &'r gix::Repository,
        root: &'r Path,
        old: Option<&'r StatCache>,
        tracked: &'r BTreeMap<String, TrackedRow>,
        old_written: u64,
    ) -> Result<Self, ScanError> {
        // The ignore inputs recorded last time must be unchanged, or everything is walked.
        let full = match old {
            None => true,
            Some(o) => {
                o.dirs.is_empty()
                    || o.ignore
                        .iter()
                        .any(|i| ignore_stat(root, &i.path) != i.stat)
            }
        };
        Ok(Walker {
            git,
            root,
            old,
            tracked,
            old_written,
            full,
            stack: None,
            index: None,
            ignore_seen: Vec::new(),
            dir_stat_time: Default::default(),
            hash_time: Default::default(),
            changed: false,
            rehash: false,
            form: RepoForm::new(git),
        })
    }

    fn global_inputs(&self) -> Vec<IgnoreInput> {
        let mut paths = vec![self.git.common_dir().join("info/exclude")];
        let configured = self.git.config_snapshot().trusted_path("core.excludesFile");
        match configured {
            Ok(Some(p)) => paths.push(p),
            _ => {
                let base = std::env::var_os("XDG_CONFIG_HOME")
                    .map(std::path::PathBuf::from)
                    .or_else(|| {
                        std::env::var_os("HOME")
                            .map(|h| std::path::PathBuf::from(h).join(".config"))
                    });
                if let Some(b) = base {
                    paths.push(b.join("git/ignore"));
                }
            }
        }
        paths
            .into_iter()
            .map(|p| {
                let path = p.display().to_string();
                let stat = ignore_stat(self.root, &path);
                IgnoreInput { path, stat }
            })
            .collect()
    }

    fn run(&mut self) -> Result<WalkOut, ScanError> {
        loop {
            self.ignore_seen = self.global_inputs();
            // An ignore input that changed since last time means a full walk.
            if !self.full
                && let Some(old) = self.old
            {
                let globals_same = self
                    .ignore_seen
                    .iter()
                    .all(|g| old.ignore.iter().any(|o| o == g));
                if !globals_same {
                    self.full = true;
                }
            }
            let mut dirs = BTreeMap::new();
            match self.walk("", &mut dirs)? {
                true => return Ok((dirs, std::mem::take(&mut self.ignore_seen))),
                false => {
                    // A `.gitignore` nobody knew of appeared: everything is walked again.
                    self.full = true;
                    self.changed = true;
                    self.stack = None;
                }
            }
        }
    }

    fn excluded(&mut self, rel: &str, is_dir: bool) -> Result<bool, ScanError> {
        if self.stack.is_none() {
            if self.index.is_none() {
                self.index = Some(self.git.index_or_empty().map_err(diff_err)?);
            }
            let index = self.index.as_ref().expect("index");
            self.stack = Some(excludes_stack(self.git, index)?);
        }
        stack_excluded(self.stack.as_mut().expect("stack"), rel, is_dir)
    }

    /// Walks `dir` (repo-relative, empty for the root). `Ok(false)` means a new ignore file was
    /// met during a partial walk and the caller must start again with `full`.
    fn walk(&mut self, dir: &str, out: &mut BTreeMap<String, DirRow>) -> Result<bool, ScanError> {
        let full_path = if dir.is_empty() {
            self.root().to_path_buf()
        } else {
            self.root().join(dir)
        };
        let t = std::time::Instant::now();
        let mtime = stat_of(&full_path).map_or(0, |s| s.mtime_ns);
        self.dir_stat_time += t.elapsed();
        let cached = if self.full {
            None
        } else {
            self.old
                .as_ref()
                .and_then(|o| o.dirs.get(dir))
                .filter(|r| r.mtime_ns == mtime && mtime.saturating_add(RACY_NS) < self.old_written)
        };
        let (subdirs, mut files): (Vec<String>, Vec<UntrackedRow>) = match cached {
            Some(r) => (r.subdirs.clone(), r.files.clone()),
            None => {
                self.changed = true;
                let mut subdirs = Vec::new();
                let mut names = Vec::new();
                let rd = std::fs::read_dir(&full_path).map_err(|e| io_err(&full_path, e))?;
                for entry in rd {
                    let entry = entry.map_err(|e| io_err(&full_path, e))?;
                    let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                        continue;
                    };
                    if name == ".git" {
                        continue;
                    }
                    let ft = entry.file_type().map_err(|e| io_err(&full_path, e))?;
                    let rel = if dir.is_empty() {
                        name.clone()
                    } else {
                        format!("{dir}/{name}")
                    };
                    if ft.is_dir() {
                        // A nested repository is its own checkout.
                        if entry.path().join(".git").exists() || self.excluded(&rel, true)? {
                            continue;
                        }
                        subdirs.push(name);
                        continue;
                    }
                    if name == ".gitignore" {
                        let stat = ignore_stat(self.root(), &rel);
                        let input = IgnoreInput {
                            path: rel.clone(),
                            stat,
                        };
                        let known = self.old.as_ref().is_some_and(|o| o.ignore.contains(&input));
                        if !self.full && !known {
                            return Ok(false);
                        }
                        self.ignore_seen.push(input);
                    }
                    if self.tracked.contains_key(&rel) || self.excluded(&rel, false)? {
                        continue;
                    }
                    names.push(name);
                }
                subdirs.sort();
                names.sort();
                let files = names
                    .into_iter()
                    .map(|name| UntrackedRow {
                        name,
                        stat: Stat::default(),
                        id: String::new(),
                    })
                    .collect();
                (subdirs, files)
            }
        };
        // A directory that was not listed again still has its `.gitignore` inputs recorded.
        if cached.is_some()
            && let Some(old) = &self.old
        {
            let prefix = if dir.is_empty() {
                String::new()
            } else {
                format!("{dir}/")
            };
            for i in &old.ignore {
                if i.path.starts_with(&prefix)
                    && i.path.ends_with(".gitignore")
                    && !i.path[prefix.len()..].contains('/')
                {
                    self.ignore_seen.push(i.clone());
                }
            }
        }
        // Stat each untracked file; read it only when its stat is new or racy.
        let mut kept = Vec::with_capacity(files.len());
        for mut f in files.drain(..) {
            let rel = if dir.is_empty() {
                f.name.clone()
            } else {
                format!("{dir}/{}", f.name)
            };
            let t = std::time::Instant::now();
            let cur = stat_of(&self.root().join(&rel));
            self.dir_stat_time += t.elapsed();
            let Some(cur) = cur else {
                self.changed = true;
                continue;
            };
            let fresh = !self.rehash
                && !f.id.is_empty()
                && f.stat == cur
                && cur.mtime_ns.saturating_add(RACY_NS) < self.old_written;
            if !fresh {
                let t = std::time::Instant::now();
                HASHED.fetch_add(1, Ordering::Relaxed);
                let id = repo_blob_id(&mut self.form, self.root, &rel);
                self.hash_time += t.elapsed();
                self.changed = true;
                let Some(id) = id else { continue };
                f.stat = cur;
                f.id = id;
            }
            kept.push(f);
        }
        out.insert(
            dir.to_string(),
            DirRow {
                mtime_ns: mtime,
                subdirs: subdirs.clone(),
                files: kept,
            },
        );
        for sub in subdirs {
            let rel = if dir.is_empty() {
                sub
            } else {
                format!("{dir}/{sub}")
            };
            if !self.walk(&rel, out)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn root(&self) -> &Path {
        self.root
    }
}

/// Size and mtime of an ignore input: relative to the worktree, or absolute.
fn ignore_stat(root: &Path, path: &str) -> Option<(u64, u64)> {
    let p = Path::new(path);
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    };
    stat_of(&full).map(|s| (s.size, s.mtime_ns))
}

impl crate::consistency::Engine {
    /// The facts part of a view ID (chain decision CD-3): the SHA-256, in hex, of the sorted
    /// extractor ids with their revision salts, and the profile hash each extension had when
    /// the snapshot was published. Binary versions and `CACHE_SCHEMA` take no part.
    pub fn facts_version(
        &self,
        snap: &impl katalogr_store::Reader,
    ) -> Result<String, crate::error::EngineError> {
        let mut rows: Vec<(&str, &str)> = self
            .registry()
            .extractors()
            .map(|e| (e.id(), e.revision_salt()))
            .collect();
        rows.sort();
        let mut h = Sha256::new();
        for (id, salt) in rows {
            h.update(id.as_bytes());
            h.update([0u8]);
            h.update(salt.as_bytes());
            h.update([0u8]);
            let profile = snap
                .meta(&format!("profile_hash:{id}"))?
                .unwrap_or_default();
            h.update(profile.len().to_le_bytes());
            h.update(&profile);
        }
        Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
    }
}

/// The working-tree root of the git repository that holds `dir`, if there is one.
pub fn checkout_root(dir: &Path) -> Option<std::path::PathBuf> {
    gix::discover(dir)
        .ok()
        .and_then(|r| r.workdir().map(Path::to_path_buf))
}
