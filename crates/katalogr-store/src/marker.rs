//! The `.katalogr` marker and kindex/project resolution (R-9).

use crate::error::StoreError;
use crate::kindex::{ProjectName, read_to_string, write_atomic};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

pub const MARKER_FILE: &str = ".katalogr";

/// Fields are declared in key order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    /// Relative to the directory holding the marker.
    pub kindex: Utf8PathBuf,
    pub project: ProjectName,
    pub repo: String,
}

/// Values from command-line flags; the environment fills the gaps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    pub kindex: Option<Utf8PathBuf>,
    pub project: Option<ProjectName>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub kindex: Utf8PathBuf,
    pub project: ProjectName,
    pub repo: Option<String>,
    /// The root of the checkout the command ran in, linked or main.
    pub repo_root: Option<Utf8PathBuf>,
    /// True when the marker was found through the main checkout of a linked worktree.
    pub via_main_checkout: bool,
}

/// Overrides (flags, then `KATALOGR_KINDEX` / `KATALOGR_PROJECT`) win; else the nearest marker
/// found walking up from `start`; else `NotConfigured`.
pub fn resolve(start: &Utf8Path, o: &Overrides) -> Result<Resolved, StoreError> {
    resolve_with_env(start, o, &|k| std::env::var(k).ok())
}

pub fn resolve_with_env(
    start: &Utf8Path,
    o: &Overrides,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Resolved, StoreError> {
    let kindex = o.kindex.clone().or_else(|| {
        env("KATALOGR_KINDEX")
            .filter(|s| !s.is_empty())
            .map(Utf8PathBuf::from)
    });
    let project = match &o.project {
        Some(p) => Some(p.clone()),
        None => env("KATALOGR_PROJECT")
            .filter(|s| !s.is_empty())
            .map(|s| ProjectName::new(&s))
            .transpose()?,
    };
    let mut via_main_checkout = false;
    let found = match find(start)? {
        Some(f) => Some(f),
        None => find_via_main(start)?.inspect(|_| via_main_checkout = true),
    };
    let (repo, repo_root, mk, mp) = match found {
        Some((dir, kindex, m)) => (Some(m.repo), Some(dir), Some(kindex), Some(m.project)),
        None => (None, None, None, None),
    };
    match (kindex.or(mk), project.or(mp)) {
        (Some(kindex), Some(project)) => Ok(Resolved {
            kindex,
            project,
            repo,
            repo_root,
            via_main_checkout,
        }),
        _ => Err(StoreError::NotConfigured),
    }
}

/// The nearest marker walking up: (its directory, its kindex resolved against that directory, marker).
fn find(start: &Utf8Path) -> Result<Option<(Utf8PathBuf, Utf8PathBuf, Marker)>, StoreError> {
    for dir in start.ancestors() {
        let p = dir.join(MARKER_FILE);
        if p.is_file() {
            let m: Marker =
                serde_yaml_ng::from_str(&read_to_string(&p)?).map_err(|e| StoreError::Parse {
                    path: p.to_string(),
                    message: e.to_string(),
                })?;
            let kindex = dir.join(&m.kindex);
            return Ok(Some((dir.to_owned(), kindex, m)));
        }
    }
    Ok(None)
}

/// The marker of the main checkout of the linked worktree containing `start`. The returned
/// directory is the worktree root; the kindex is resolved against the main root, since the
/// path is relative to the marker's own directory.
fn find_via_main(
    start: &Utf8Path,
) -> Result<Option<(Utf8PathBuf, Utf8PathBuf, Marker)>, StoreError> {
    let Some((worktree, main)) = main_checkout_of(start) else {
        return Ok(None);
    };
    Ok(find_in(&main)?.map(|m| {
        let kindex = main.join(&m.kindex);
        (worktree, kindex, m)
    }))
}

/// The marker file directly in `dir`, if any.
fn find_in(dir: &Utf8Path) -> Result<Option<Marker>, StoreError> {
    let p = dir.join(MARKER_FILE);
    if !p.is_file() {
        return Ok(None);
    }
    serde_yaml_ng::from_str(&read_to_string(&p)?)
        .map(Some)
        .map_err(|e| StoreError::Parse {
            path: p.to_string(),
            message: e.to_string(),
        })
}

/// The main checkout of the linked worktree containing `start`, if `start` is inside one.
/// Reads the `.git` file and the `commondir` file with std only. Anything malformed gives
/// `None`, never an error, since a plain directory is valid.
pub fn main_checkout_of(start: &Utf8Path) -> Option<(Utf8PathBuf, Utf8PathBuf)> {
    for dir in start.ancestors() {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            // A main checkout (or a plain repository): not a linked worktree.
            return None;
        }
        if !dot_git.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let line = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
        if line.is_empty() {
            return None;
        }
        let gitdir = Utf8Path::new(line);
        let gitdir = if gitdir.is_absolute() {
            gitdir.to_owned()
        } else {
            dir.join(gitdir)
        };
        let common = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
        let common = Utf8Path::new(common.trim());
        if common.as_str().is_empty() {
            return None;
        }
        let common = if common.is_absolute() {
            common.to_owned()
        } else {
            gitdir.join(common)
        };
        // Without Windows' verbatim prefix: a verbatim root takes `/`-joined repo-relative
        // paths literally, and `join("..")` on it collapses instead of appending.
        let common =
            Utf8PathBuf::from_path_buf(crate::canon::canonicalize(common.as_std_path()).ok()?)
                .ok()?;
        if common.file_name() != Some(".git") {
            return None; // a bare common directory has no main checkout
        }
        let main = common.parent()?.to_owned();
        let worktree =
            Utf8PathBuf::from_path_buf(crate::canon::canonicalize(dir.as_std_path()).ok()?).ok()?;
        return Some((worktree, main));
    }
    None
}

pub fn write(repo_root: &Utf8Path, m: &Marker) -> Result<(), StoreError> {
    let yaml = serde_yaml_ng::to_string(m).map_err(|e| StoreError::Parse {
        path: MARKER_FILE.into(),
        message: e.to_string(),
    })?;
    write_atomic(&repo_root.join(MARKER_FILE), yaml.as_bytes())
}
