//! Census of a repository's files on disk: which ones an extension claims (FEAT-062).

use crate::consistency::Engine;
use crate::error::EngineError;
use crate::profiles::ProfileSet;
use crate::registry::ExtensionRegistry;
use crate::scan;
use katalogr_model::{Extractor, RelPath};

/// Files of one repository on disk, split by whether a registered extension claims them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Census {
    /// Claimed files and the claiming extension id, sorted by path.
    pub claimed: Vec<(RelPath, &'static str)>,
    /// Files no extension claims, sorted.
    pub unclaimed: Vec<RelPath>,
}

/// The extractor for `path`: the profile route when it names a registered extension, else the
/// first registered extractor that handles the path.
pub fn extractor_for<'r>(
    registry: &'r ExtensionRegistry,
    profiles: &ProfileSet,
    path: &RelPath,
) -> Option<&'r dyn Extractor> {
    profiles
        .route(path)
        .and_then(|id| registry.by_id(id))
        .or_else(|| registry.for_path(path))
}

/// Walk `repo`'s root with its include and exclude globs (`scan::list_files`) and route each
/// file as the indexer does (`extractor_for`). `Ok(None)` when the repo has no known root on
/// this machine. Loads the profiles with `Engine::routing_profiles`.
pub fn census(engine: &Engine, repo: &str) -> Result<Option<Census>, EngineError> {
    let profiles = engine.routing_profiles()?;
    census_with(engine, repo, &profiles)
}

/// As `census`, routing with `profiles`. `Ok(None)` when the repo has no known root here.
pub fn census_with(
    engine: &Engine,
    repo: &str,
    profiles: &ProfileSet,
) -> Result<Option<Census>, EngineError> {
    let Some(root) = engine.roots().get(repo) else {
        return Ok(None);
    };
    let (include, exclude) = engine.repo_filters(repo);
    let mut out = Census::default();
    for p in scan::list_files(root.as_std_path(), &include, &exclude)? {
        match extractor_for(engine.registry(), profiles, &p) {
            Some(e) => out.claimed.push((p, e.id())),
            None => out.unclaimed.push(p),
        }
    }
    Ok(Some(out))
}
