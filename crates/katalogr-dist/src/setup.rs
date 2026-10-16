//! Creating a kindex and a project, and marking repositories.

use crate::error::DistError;
use crate::roots;
use camino::{Utf8Path, Utf8PathBuf};
use katalogr_store::marker::{self, Marker};
use katalogr_store::{Kindex, ProjectConfig, ProjectName, RepoConfig};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What `project_init` created.
#[derive(Debug, Clone)]
pub struct ProjectCreated {
    pub project_dir: PathBuf,
    /// Marker files written (empty unless `mark`).
    pub markers: Vec<PathBuf>,
}

pub(crate) fn utf8(p: &Path) -> Result<Utf8PathBuf, DistError> {
    Utf8PathBuf::from_path_buf(p.to_path_buf())
        .map_err(|p| DistError::usage(format!("{}: path is not UTF-8", p.display())))
}

/// Absolute form against the current directory (no symlink resolution).
pub(crate) fn absolute(p: &Path) -> Result<Utf8PathBuf, DistError> {
    utf8(&std::path::absolute(p).map_err(|e| DistError::io(p.display(), e))?)
}

fn canonical(p: &Utf8Path) -> Result<Utf8PathBuf, DistError> {
    let c = std::fs::canonicalize(p).map_err(|e| DistError::io(p, e))?;
    utf8(&c)
}

/// `target` expressed relative to `base`; both absolute and canonical.
fn relative_to(base: &Utf8Path, target: &Utf8Path) -> Utf8PathBuf {
    let b: Vec<_> = base.components().collect();
    let t: Vec<_> = target.components().collect();
    let common = b.iter().zip(&t).take_while(|(x, y)| x == y).count();
    let mut out = Utf8PathBuf::new();
    for _ in common..b.len() {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c.as_str());
    }
    out
}

/// Create (or confirm) a kindex repository at `dir`. Idempotent.
pub fn kindex_init(dir: &Path) -> Result<PathBuf, DistError> {
    let d = absolute(dir)?;
    Kindex::init(&d)?;
    Ok(d.into_std_path_buf())
}

/// Create a project with one `RepoConfig` per repo, record the roots machine-locally and,
/// with `mark`, write a `.katalogr` marker into each repo root.
pub fn project_init(
    kindex: &Path,
    name: &str,
    repos: Vec<(String, PathBuf)>,
    mark: bool,
) -> Result<ProjectCreated, DistError> {
    let kx = Kindex::open(&absolute(kindex)?)?;
    let pname = ProjectName::new(name)?;
    let mut abs: BTreeMap<String, Utf8PathBuf> = BTreeMap::new();
    for (repo, path) in &repos {
        let root = absolute(path)?;
        if !root.is_dir() {
            return Err(DistError::usage(format!(
                "repo {repo}: {root} is not a directory"
            )));
        }
        if abs.insert(repo.clone(), canonical(&root)?).is_some() {
            return Err(DistError::usage(format!("repo {repo} given twice")));
        }
    }
    let config = ProjectConfig {
        repos: abs.keys().map(|n| RepoConfig::new(n)).collect(),
        ..ProjectConfig::default()
    };
    let dir = kx.create_project(&pname, config)?;
    roots::write(&dir.cache_dir(), &abs)?;
    let mut markers = Vec::new();
    if mark {
        let kx_root = canonical(kx.root())?;
        for (repo, root) in &abs {
            let m = Marker {
                kindex: relative_to(root, &kx_root),
                project: pname.clone(),
                repo: repo.clone(),
            };
            marker::write(root, &m)?;
            markers.push(root.join(marker::MARKER_FILE).into_std_path_buf());
        }
    }
    Ok(ProjectCreated {
        project_dir: dir.root().to_path_buf().into_std_path_buf(),
        markers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        let r = |a: &str, b: &str| relative_to(Utf8Path::new(a), Utf8Path::new(b)).to_string();
        assert_eq!(r("/a/b/repo", "/a/b/kx"), "../kx");
        assert_eq!(r("/a/repo", "/a/repo/kx"), "kx");
        assert_eq!(r("/a/b/c", "/x"), "../../../x");
    }
}
