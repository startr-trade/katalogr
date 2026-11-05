//! Where the processor reads a repo's files: the working tree, or a commit's tree (FEAT-043).

use crate::error::EngineError;
use crate::scan::{self, DIR_PROBE, ScanError, glob_set};
use camino::Utf8PathBuf;
use katalogr_model::RelPath;

/// Where the processor reads a repo's files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileSource {
    Disk(Utf8PathBuf),
    Commit {
        repo_root: Utf8PathBuf,
        commit: String,
    },
}

fn scan_err(path: &Utf8PathBuf, e: impl std::fmt::Display) -> ScanError {
    ScanError::Io {
        path: path.to_string(),
        message: e.to_string(),
    }
}

fn commit_tree<'r>(repo: &'r gix::Repository, commit: &str) -> Result<gix::Tree<'r>, ScanError> {
    let bad = |e: &dyn std::fmt::Display| ScanError::BadRevision {
        rev: commit.to_string(),
        message: e.to_string(),
    };
    let id = repo.rev_parse_single(commit).map_err(|e| bad(&e))?;
    id.object()
        .map_err(|e| bad(&e))?
        .peel_to_commit()
        .map_err(|e| bad(&e))?
        .tree()
        .map_err(|e| bad(&e))
}

fn open_repo(root: &Utf8PathBuf) -> Result<gix::Repository, ScanError> {
    gix::open(root.as_std_path()).map_err(|e| ScanError::RepoOpen {
        path: root.to_string(),
        message: e.to_string(),
    })
}

impl FileSource {
    /// Of `paths`, those this source does not admit: on disk, paths git ignores and does not
    /// track; a commit's tree and a plain directory admit everything. Also returns warnings
    /// (unreadable ignore data admits everything).
    pub fn not_admitted<'a>(
        &self,
        paths: impl IntoIterator<Item = &'a RelPath>,
    ) -> (std::collections::BTreeSet<RelPath>, Vec<String>) {
        match self {
            FileSource::Disk(root) => scan::ignored_among(root.as_std_path(), paths),
            FileSource::Commit { .. } => Default::default(),
        }
    }

    /// The file's bytes; `None` when it does not exist there.
    pub fn read(&self, path: &RelPath) -> Result<Option<Vec<u8>>, EngineError> {
        match self {
            FileSource::Disk(root) => {
                let file = root.join(path.as_str());
                match std::fs::read(&file) {
                    Ok(b) => Ok(Some(b)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(EngineError::io(&file, e)),
                }
            }
            FileSource::Commit { repo_root, commit } => {
                let repo = open_repo(repo_root)?;
                let tree = commit_tree(&repo, commit)?;
                let entry = tree
                    .lookup_entry_by_path(path.as_str())
                    .map_err(|e| scan_err(repo_root, e))?;
                let Some(entry) = entry else {
                    return Ok(None);
                };
                if !entry.mode().is_blob() {
                    return Ok(None);
                }
                let obj = entry.object().map_err(|e| scan_err(repo_root, e))?;
                Ok(Some(obj.detach().data))
            }
        }
    }

    /// Paths the globs admit, sorted. For `Commit`, a walk of the commit's tree.
    pub fn list(&self, include: &[String], exclude: &[String]) -> Result<Vec<RelPath>, ScanError> {
        match self {
            FileSource::Disk(root) => scan::list_files(root.as_std_path(), include, exclude),
            FileSource::Commit { repo_root, commit } => {
                let inc = glob_set(include)?;
                let exc = glob_set(exclude)?;
                let repo = open_repo(repo_root)?;
                let tree = commit_tree(&repo, commit)?;
                let mut out = Vec::new();
                walk(&repo, repo_root, &tree, "", &inc, &exc, &mut out)?;
                out.sort();
                Ok(out)
            }
        }
    }
}

fn walk(
    repo: &gix::Repository,
    root: &Utf8PathBuf,
    tree: &gix::Tree<'_>,
    prefix: &str,
    include: &globset::GlobSet,
    exclude: &globset::GlobSet,
    out: &mut Vec<RelPath>,
) -> Result<(), ScanError> {
    for entry in tree.iter() {
        let entry = entry.map_err(|e| scan_err(root, e))?;
        let Ok(name) = std::str::from_utf8(entry.filename()) else {
            continue;
        };
        let rel = format!("{prefix}{name}");
        let mode = entry.mode();
        if mode.is_tree() {
            if name != ".git" && !exclude.is_match(format!("{rel}/{DIR_PROBE}")) {
                let sub = repo
                    .find_tree(entry.object_id())
                    .map_err(|e| scan_err(root, e))?;
                walk(repo, root, &sub, &format!("{rel}/"), include, exclude, out)?;
            }
        } else if mode.is_blob()
            && include.is_match(&rel)
            && !exclude.is_match(&rel)
            && let Ok(p) = RelPath::new(&rel)
        {
            out.push(p);
        }
    }
    Ok(())
}
