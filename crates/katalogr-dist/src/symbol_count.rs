//! `symbol_count`: for the load-test harness, count symbols of one kind from the artifacts of
//! one extension, reading the graph database and nothing else (FEAT-094 T10).

use crate::error::DistError;
use camino::Utf8Path;
use katalogr_store::{Cache, Kindex, ProjectName, Reader};
use std::collections::BTreeSet;

/// Count the symbols of `kind` whose artifact came from `extension`, read-only. An error when
/// the project has no readable graph database (none yet, another schema, or no shared locks).
pub fn symbol_count(
    kindex: &Utf8Path,
    project: &str,
    extension: &str,
    kind: &str,
) -> Result<u64, DistError> {
    let name = ProjectName::new(project).map_err(|e| DistError::usage(format!("{project}: {e}")))?;
    let dir = Kindex::open(kindex)?.project(&name)?;
    let Some(cache) = Cache::open_reader(&dir)? else {
        return Err(DistError::Refused {
            exit: 4,
            message: "the graph database cannot be read".to_string(),
        });
    };
    let rx = cache.read()?;
    let artifacts: BTreeSet<_> = rx
        .artifacts()?
        .into_iter()
        .filter(|(_, row)| row.extension == extension)
        .map(|(id, _)| id)
        .collect();
    Ok(rx
        .symbols()?
        .into_iter()
        .filter(|(_, s)| s.kind == kind && artifacts.contains(&s.artifact))
        .count() as u64)
}
