//! Census of a repository's files on disk: which ones an extension claims (FEAT-062).

use crate::consistency::Engine;
use crate::error::EngineError;
use crate::scan;
use katalogr_model::RelPath;

/// Files of one repository on disk, split by whether a registered extension claims them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Census {
    /// Claimed files and the claiming extension id, sorted by path.
    pub claimed: Vec<(RelPath, &'static str)>,
    /// Files no extension claims, sorted.
    pub unclaimed: Vec<RelPath>,
}

/// Walk `repo`'s root with its include and exclude globs (`scan::list_files`) and ask
/// `registry().for_path` for each file. `Ok(None)` when the repo has no known root on this machine.
pub fn census(engine: &Engine, repo: &str) -> Result<Option<Census>, EngineError> {
    let Some(root) = engine.roots().get(repo) else {
        return Ok(None);
    };
    let (include, exclude) = engine.repo_filters(repo);
    let mut out = Census::default();
    for p in scan::list_files(root.as_std_path(), &include, &exclude)? {
        match engine.registry().for_path(&p) {
            Some(e) => out.claimed.push((p, e.id())),
            None => out.unclaimed.push(p),
        }
    }
    Ok(Some(out))
}
