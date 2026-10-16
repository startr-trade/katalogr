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
    pub repo_root: Option<Utf8PathBuf>,
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
    let found = find(start)?;
    let (repo, repo_root, mk, mp) = match found {
        Some((dir, m)) => (
            Some(m.repo),
            Some(dir.clone()),
            Some(dir.join(&m.kindex)),
            Some(m.project),
        ),
        None => (None, None, None, None),
    };
    match (kindex.or(mk), project.or(mp)) {
        (Some(kindex), Some(project)) => Ok(Resolved {
            kindex,
            project,
            repo,
            repo_root,
        }),
        _ => Err(StoreError::NotConfigured),
    }
}

fn find(start: &Utf8Path) -> Result<Option<(Utf8PathBuf, Marker)>, StoreError> {
    for dir in start.ancestors() {
        let p = dir.join(MARKER_FILE);
        if p.is_file() {
            let m: Marker =
                serde_yaml_ng::from_str(&read_to_string(&p)?).map_err(|e| StoreError::Parse {
                    path: p.to_string(),
                    message: e.to_string(),
                })?;
            return Ok(Some((dir.to_owned(), m)));
        }
    }
    Ok(None)
}

pub fn write(repo_root: &Utf8Path, m: &Marker) -> Result<(), StoreError> {
    let yaml = serde_yaml_ng::to_string(m).map_err(|e| StoreError::Parse {
        path: MARKER_FILE.into(),
        message: e.to_string(),
    })?;
    write_atomic(&repo_root.join(MARKER_FILE), yaml.as_bytes())
}
