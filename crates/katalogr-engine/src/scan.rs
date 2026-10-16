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
