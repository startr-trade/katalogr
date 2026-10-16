//! Machine-specific repo roots: `<project>/.cache/roots.yaml`, never part of the kindex.

use crate::error::DistError;
use camino::{Utf8Path, Utf8PathBuf};
use std::collections::BTreeMap;
use std::io::Write;

const FILE: &str = "roots.yaml";

/// Repo name to absolute root. Missing file means no roots.
pub(crate) fn read(cache_dir: &Utf8Path) -> Result<BTreeMap<String, Utf8PathBuf>, DistError> {
    let p = cache_dir.join(FILE);
    match std::fs::read_to_string(&p) {
        Ok(text) => serde_yaml_ng::from_str(&text).map_err(|e| DistError::io(&p, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(DistError::io(&p, e)),
    }
}

/// Atomic replace; the map is sorted by construction.
pub(crate) fn write(
    cache_dir: &Utf8Path,
    roots: &BTreeMap<String, Utf8PathBuf>,
) -> Result<(), DistError> {
    let p = cache_dir.join(FILE);
    let io = |e: std::io::Error| DistError::io(&p, e);
    std::fs::create_dir_all(cache_dir).map_err(io)?;
    let yaml = serde_yaml_ng::to_string(roots).map_err(|e| DistError::io(&p, e))?;
    let mut t = tempfile::NamedTempFile::new_in(cache_dir).map_err(io)?;
    t.write_all(yaml.as_bytes()).map_err(io)?;
    t.as_file().sync_all().map_err(io)?;
    t.persist(&p).map_err(|e| io(e.error))?;
    Ok(())
}
