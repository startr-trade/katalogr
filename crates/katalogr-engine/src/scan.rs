//! Diff-range scanning: which repo-relative paths changed between two revisions (FEAT-001 §4, §6).

use gix::object::tree::diff::Change;
use katalogr_model::RelPath;

/// One changed path between two commits. Paths are repo-relative and '/'-separated.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum PathChange {
    Added(RelPath),
    Modified(RelPath),
    Deleted(RelPath),
    /// Not produced yet (rename tracking is off); reserved for later use.
    Renamed {
        from: RelPath,
        to: RelPath,
    },
}

impl PathChange {
    fn sort_path(&self) -> &str {
        match self {
            Self::Added(p) | Self::Modified(p) | Self::Deleted(p) => p.as_str(),
            Self::Renamed { to, .. } => to.as_str(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("cannot open repository at {path}: {message}")]
    RepoOpen { path: String, message: String },
    #[error("bad revision `{rev}`: {message}")]
    BadRevision { rev: String, message: String },
    #[error("diff failed: {0}")]
    Diff(String),
    #[error("path `{path}` in the diff is not a valid repo-relative path: {message}")]
    BadPath { path: String, message: String },
    #[error("cannot read {path}: {message}")]
    Io { path: String, message: String },
    #[error("bad glob `{glob}`: {message}")]
    BadGlob { glob: String, message: String },
}

/// Paths changed in `range`: either `a..b`, or a single `<rev>` meaning `<rev>..HEAD`.
/// Rename tracking is off; output is sorted by path.
pub fn changed_paths(
    repo_root: &std::path::Path,
    range: &str,
) -> Result<Vec<PathChange>, ScanError> {
    let repo = gix::open(repo_root).map_err(|e| ScanError::RepoOpen {
        path: repo_root.display().to_string(),
        message: e.to_string(),
    })?;
    let (from, to) = match range.split_once("..") {
        Some((a, b)) => (a, if b.is_empty() { "HEAD" } else { b }),
        None => (range, "HEAD"),
    };
    let from_tree = tree_of(&repo, from)?;
    let to_tree = tree_of(&repo, to)?;

    let mut raw: Vec<(u8, String)> = Vec::new(); // 0 added, 1 deleted, 2 modified
    let mut platform = from_tree.changes().map_err(diff_err)?;
    platform.options(|o| {
        o.track_rewrites(None);
    });
    platform
        .for_each_to_obtain_tree(&to_tree, |change| {
            match change {
                Change::Addition {
                    location,
                    entry_mode,
                    ..
                } => {
                    if !entry_mode.is_tree() {
                        raw.push((0, location.to_string()));
                    }
                }
                Change::Deletion {
                    location,
                    entry_mode,
                    ..
                } => {
                    if !entry_mode.is_tree() {
                        raw.push((1, location.to_string()));
                    }
                }
                Change::Modification {
                    location,
                    entry_mode,
                    ..
                } => {
                    if !entry_mode.is_tree() {
                        raw.push((2, location.to_string()));
                    }
                }
                Change::Rewrite { .. } => {}
            }
            Ok(std::ops::ControlFlow::Continue(()))
        })
        .map_err(diff_err)?;
    let mut out = raw
        .into_iter()
        .map(|(k, p)| {
            let p = rel(&p)?;
            Ok(match k {
                0 => PathChange::Added(p),
                1 => PathChange::Deleted(p),
                _ => PathChange::Modified(p),
            })
        })
        .collect::<Result<Vec<_>, ScanError>>()?;
    out.sort_by(|a, b| a.sort_path().cmp(b.sort_path()).then_with(|| a.cmp(b)));
    Ok(out)
}

fn rel(p: &str) -> Result<RelPath, ScanError> {
    RelPath::new(p).map_err(|e| ScanError::BadPath {
        path: p.to_string(),
        message: e.to_string(),
    })
}

fn diff_err(e: impl std::fmt::Display) -> ScanError {
    ScanError::Diff(e.to_string())
}

fn tree_of<'r>(repo: &'r gix::Repository, rev: &str) -> Result<gix::Tree<'r>, ScanError> {
    let bad = |e: &dyn std::fmt::Display| ScanError::BadRevision {
        rev: rev.to_string(),
        message: e.to_string(),
    };
    let id = repo.rev_parse_single(rev).map_err(|e| bad(&e))?;
    let commit = id
        .object()
        .map_err(|e| bad(&e))?
        .peel_to_commit()
        .map_err(|e| bad(&e))?;
    commit.tree().map_err(|e| bad(&e))
}

/// How a file differs between two versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

/// One changed file, with line ranges in the new version (1-based, inclusive); empty ranges for
/// an added or deleted file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileChange {
    pub path: RelPath,
    pub kind: ChangeKind,
    pub ranges: Vec<(u32, u32)>,
}

/// Files changed from revision `from` to revision `to`, with the changed line ranges of each
/// (histogram diff, as `history` does). A `to` of `None` reads the working tree, so uncommitted
/// changes of tracked files count. Rename tracking is off; output is sorted by path.
pub fn changed_hunks(
    repo_root: &std::path::Path,
    from: &str,
    to: Option<&str>,
) -> Result<Vec<FileChange>, ScanError> {
    let repo = gix::open(repo_root).map_err(|e| ScanError::RepoOpen {
        path: repo_root.display().to_string(),
        message: e.to_string(),
    })?;
    let from_tree = tree_of(&repo, from)?;
    let to_tree = tree_of(&repo, to.unwrap_or("HEAD"))?;
    let mut paths: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for c in changed_paths_between(&from_tree, &to_tree)? {
        paths.insert(c);
    }
    if to.is_none() {
        paths.extend(dirty_paths(repo_root)?);
    }
    let mut out = Vec::new();
    for path in paths {
        let old = blob_at(&from_tree, &path)?;
        let new = match to {
            Some(_) => blob_at(&to_tree, &path)?,
            None => match std::fs::read(repo_root.join(&path)) {
                Ok(b) => Some(b),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => {
                    return Err(ScanError::Io {
                        path,
                        message: e.to_string(),
                    });
                }
            },
        };
        let (kind, ranges) = match (old, new) {
            (None, None) => continue,
            (None, Some(_)) => (ChangeKind::Added, Vec::new()),
            (Some(_), None) => (ChangeKind::Deleted, Vec::new()),
            (Some(o), Some(n)) => {
                if o == n {
                    continue;
                }
                (ChangeKind::Modified, crate::history::line_ranges(&o, &n))
            }
        };
        out.push(FileChange {
            path: rel(&path)?,
            kind,
            ranges,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn changed_paths_between(
    from: &gix::Tree<'_>,
    to: &gix::Tree<'_>,
) -> Result<Vec<String>, ScanError> {
    let mut out = Vec::new();
    let mut platform = from.changes().map_err(diff_err)?;
    platform.options(|o| {
        o.track_rewrites(None);
    });
    platform
        .for_each_to_obtain_tree(to, |change| {
            match change {
                Change::Addition {
                    location,
                    entry_mode,
                    ..
                }
                | Change::Deletion {
                    location,
                    entry_mode,
                    ..
                }
                | Change::Modification {
                    location,
                    entry_mode,
                    ..
                } if !entry_mode.is_tree() => out.push(location.to_string()),
                _ => {}
            }
            Ok(std::ops::ControlFlow::Continue(()))
        })
        .map_err(diff_err)?;
    Ok(out)
}

/// The content of the blob at `path` in `tree`, if there is one.
fn blob_at(tree: &gix::Tree<'_>, path: &str) -> Result<Option<Vec<u8>>, ScanError> {
    let Some(entry) = tree.clone().lookup_entry_by_path(path).map_err(diff_err)? else {
        return Ok(None);
    };
    if !entry.mode().is_blob() {
        return Ok(None);
    }
    let obj = entry.object().map_err(diff_err)?;
    Ok(Some(obj.data.clone()))
}

/// Tracked paths whose working-tree or index state differs from HEAD, from gix status. Untracked
/// files are left out.
fn dirty_paths(repo_root: &std::path::Path) -> Result<Vec<String>, ScanError> {
    use gix::bstr::BString;
    let repo = gix::open(repo_root).map_err(|e| ScanError::RepoOpen {
        path: repo_root.display().to_string(),
        message: e.to_string(),
    })?;
    let iter = repo
        .status(gix::progress::Discard)
        .map_err(diff_err)?
        .untracked_files(gix::status::UntrackedFiles::None)
        .index_worktree_rewrites(None)
        .tree_index_track_renames(gix::status::tree_index::TrackRenames::Disabled)
        .into_iter(Vec::<BString>::new())
        .map_err(diff_err)?;
    let mut paths = Vec::new();
    for item in iter {
        match item.map_err(diff_err)? {
            gix::status::Item::IndexWorktree(gix::status::index_worktree::Item::Modification {
                rela_path,
                ..
            }) => paths.push(rela_path.to_string()),
            gix::status::Item::TreeIndex(change) => {
                paths.push(change.location().to_string());
            }
            _ => {}
        }
    }
    Ok(paths)
}

/// True when `path`'s folder is in no git work tree, or git ignores `path` and does not track it.
/// `path` need not exist; the nearest existing folder above it decides which work tree it is in.
pub fn output_outside_git(path: &std::path::Path) -> Result<bool, ScanError> {
    use std::path::{Component, PathBuf};
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| ScanError::Io {
                path: path.display().to_string(),
                message: e.to_string(),
            })?
            .join(path)
    };
    // Fold `.` and `..` by hand: the file may not exist, so `canonicalize` cannot do it.
    let mut clean = PathBuf::new();
    for c in absolute.components() {
        match c {
            Component::ParentDir => {
                clean.pop();
            }
            Component::CurDir => {}
            other => clean.push(other.as_os_str()),
        }
    }
    // The nearest existing folder above the path, with the names below it kept.
    let mut existing = clean.parent().map(PathBuf::from).unwrap_or_default();
    let mut below: Vec<std::ffi::OsString> = vec![clean.file_name().unwrap_or_default().into()];
    while !existing.is_dir() {
        let Some(name) = existing.file_name() else {
            break;
        };
        below.push(name.to_os_string());
        if !existing.pop() {
            break;
        }
    }
    below.reverse();
    let existing = std::fs::canonicalize(&existing).map_err(|e| ScanError::Io {
        path: existing.display().to_string(),
        message: e.to_string(),
    })?;
    let Some(root) = crate::position::checkout_root(&existing) else {
        return Ok(true);
    };
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    let rel_dir = existing.strip_prefix(&root).map_err(|_| ScanError::Io {
        path: existing.display().to_string(),
        message: "the folder is outside its own work tree".into(),
    })?;
    let mut parts: Vec<String> = rel_dir
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.extend(below.iter().map(|n| n.to_string_lossy().into_owned()));
    let git = gix::open(&root).map_err(|e| ScanError::RepoOpen {
        path: root.display().to_string(),
        message: e.to_string(),
    })?;
    let index = git.index_or_empty().map_err(diff_err)?;
    let rel = parts.join("/");
    if index.entry_by_path(rel.as_str().into()).is_some() {
        return Ok(false);
    }
    let mut stack = crate::position::excludes_stack(&git, &index)?;
    // Ignored itself, or inside an ignored folder.
    for n in 1..=parts.len() {
        let prefix = parts[..n].join("/");
        let is_dir = n < parts.len();
        if crate::position::stack_excluded(&mut stack, &prefix, is_dir)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Every regular file under `repo_root` (the `.git` directory is never entered, symbolic links
/// are skipped) whose repo-relative path matches an `include` glob and no `exclude` glob.
/// In a root with a `.git` entry, paths git ignores are dropped (a tracked file stays); if git's
/// ignore data cannot be read, the warning goes to stderr and the walk lists without it.
/// Paths that are not UTF-8 or not valid [`RelPath`]s are skipped. Output is sorted by path.
pub fn list_files(
    repo_root: &std::path::Path,
    include: &[String],
    exclude: &[String],
) -> Result<Vec<RelPath>, ScanError> {
    let (files, warnings) = list_files_warned(repo_root, include, exclude)?;
    for w in warnings {
        eprintln!("warning: {w}");
    }
    Ok(files)
}

/// As [`list_files`], returning the warnings instead of printing them.
pub fn list_files_warned(
    repo_root: &std::path::Path,
    include: &[String],
    exclude: &[String],
) -> Result<(Vec<RelPath>, Vec<String>), ScanError> {
    let include = glob_set(include)?;
    let exclude = glob_set(exclude)?;
    let mut warnings = Vec::new();
    let out = with_filter(repo_root, &mut warnings, |filter, warnings| {
        let mut out = Vec::new();
        walk(
            repo_root, "", &include, &exclude, filter, warnings, &mut out,
        )?;
        out.sort();
        Ok::<_, ScanError>(out)
    })?;
    Ok((out, warnings))
}

/// Runs `f` with git's ignore filter for `repo_root`: `None` for a root with no `.git` entry, or
/// when the ignore data cannot be read (a warning is added then).
fn with_filter<R>(
    repo_root: &std::path::Path,
    warnings: &mut Vec<String>,
    f: impl FnOnce(Option<&mut IgnoreFilter<'_>>, &mut Vec<String>) -> R,
) -> R {
    // `.git` is a directory, or a file in a linked worktree; without it nothing is ignored.
    let git = if repo_root.join(".git").exists() {
        match gix::open(repo_root) {
            Ok(g) => Some(g),
            Err(e) => {
                warnings.push(format!(
                    "git ignore data of {} unreadable, listing without it: {e}",
                    repo_root.display()
                ));
                None
            }
        }
    } else {
        None
    };
    let index = git.as_ref().and_then(|g| match g.index_or_empty() {
        Ok(i) => Some(i),
        Err(e) => {
            warnings.push(format!(
                "git index of {} unreadable, listing without ignore rules: {e}",
                repo_root.display()
            ));
            None
        }
    });
    let mut filter = match (&git, &index) {
        (Some(g), Some(i)) => match crate::position::excludes_stack(g, i) {
            Ok(stack) => {
                let tracked = i
                    .entries()
                    .iter()
                    .map(|e| e.path(i).to_string())
                    .collect::<std::collections::BTreeSet<_>>();
                Some(IgnoreFilter { stack, tracked })
            }
            Err(e) => {
                warnings.push(format!(
                    "git ignore data of {} unreadable, listing without it: {e}",
                    repo_root.display()
                ));
                None
            }
        },
        _ => None,
    };
    f(filter.as_mut(), warnings)
}

/// Of `paths`, those git ignores and does not track under `repo_root`; none for a root with no
/// `.git` entry. Unreadable ignore data admits everything and adds a warning.
pub(crate) fn ignored_among<'a>(
    repo_root: &std::path::Path,
    paths: impl IntoIterator<Item = &'a RelPath>,
) -> (std::collections::BTreeSet<RelPath>, Vec<String>) {
    let mut warnings = Vec::new();
    let set = with_filter(repo_root, &mut warnings, |filter, warnings| {
        let mut out = std::collections::BTreeSet::new();
        if let Some(f) = filter {
            for p in paths {
                if f.skip_file(p.as_str(), warnings) {
                    out.insert(p.clone());
                }
            }
        }
        out
    });
    (set, warnings)
}

/// Git's ignore rules for a walk, and the index paths (a tracked file is never dropped).
struct IgnoreFilter<'r> {
    stack: gix::AttributeStack<'r>,
    tracked: std::collections::BTreeSet<String>,
}

impl IgnoreFilter<'_> {
    fn has_tracked_under(&self, dir: &str) -> bool {
        let prefix = format!("{dir}/");
        self.tracked
            .range(prefix.clone()..)
            .next()
            .is_some_and(|p| p.starts_with(&prefix))
    }

    /// An ignored directory is skipped unless it holds a tracked path.
    fn skip_dir(&mut self, rel: &str, warnings: &mut Vec<String>) -> bool {
        match crate::position::stack_excluded(&mut self.stack, rel, true) {
            Ok(ignored) => ignored && !self.has_tracked_under(rel),
            Err(e) => {
                warnings.push(format!("ignore check of {rel} failed, listing it: {e}"));
                false
            }
        }
    }

    fn skip_file(&mut self, rel: &str, warnings: &mut Vec<String>) -> bool {
        if self.tracked.contains(rel) {
            return false;
        }
        match crate::position::stack_excluded(&mut self.stack, rel, false) {
            Ok(ignored) => ignored,
            Err(e) => {
                warnings.push(format!("ignore check of {rel} failed, listing it: {e}"));
                false
            }
        }
    }
}

pub(crate) fn glob_set(globs: &[String]) -> Result<globset::GlobSet, ScanError> {
    let bad = |glob: &str, e: globset::Error| ScanError::BadGlob {
        glob: glob.to_string(),
        message: e.to_string(),
    };
    let mut b = globset::GlobSetBuilder::new();
    for g in globs {
        b.add(globset::Glob::new(g).map_err(|e| bad(g, e))?);
    }
    b.build().map_err(|e| bad("", e))
}

/// A name no real file is likely to have; used to ask whether a glob excludes a whole directory.
pub(crate) const DIR_PROBE: &str = "__katalogr_dir_probe__";

fn walk(
    dir: &std::path::Path,
    prefix: &str,
    include: &globset::GlobSet,
    exclude: &globset::GlobSet,
    mut filter: Option<&mut IgnoreFilter<'_>>,
    warnings: &mut Vec<String>,
    out: &mut Vec<RelPath>,
) -> Result<(), ScanError> {
    let io = |e: std::io::Error| ScanError::Io {
        path: dir.display().to_string(),
        message: e.to_string(),
    };
    for entry in std::fs::read_dir(dir).map_err(io)? {
        let entry = entry.map_err(io)?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let ft = entry.file_type().map_err(io)?;
        let rel = format!("{prefix}{name}");
        if ft.is_dir() {
            if name != ".git"
                && !exclude.is_match(format!("{rel}/{DIR_PROBE}"))
                && !filter.as_mut().is_some_and(|f| f.skip_dir(&rel, warnings))
            {
                walk(
                    &entry.path(),
                    &format!("{rel}/"),
                    include,
                    exclude,
                    filter.as_deref_mut(),
                    warnings,
                    out,
                )?;
            }
        } else if ft.is_file()
            && include.is_match(&rel)
            && !exclude.is_match(&rel)
            && !filter.as_mut().is_some_and(|f| f.skip_file(&rel, warnings))
            && let Ok(p) = RelPath::new(&rel)
        {
            out.push(p);
        }
    }
    Ok(())
}
