//! `snapshot import` and `snapshot show` (FEAT-097). Import installs a checked snapshot as the
//! base's database; the next read reconciles it with the checkouts, extracting only what differs.

use crate::error::DistError;
use crate::snapshot::{absolute, refused};
use crate::{Katalogr, Options};
use camino::Utf8Path;
use katalogr_engine::NewEvent;
use katalogr_engine::queue::lock::lock_until;
use katalogr_store::{
    CACHE_SCHEMA, SnapshotManifest, StoreError, decode_snapshot, install_snapshot, read_manifest,
};
use schemars::JsonSchema;
use serde::Serialize;
use std::time::Instant;

/// What `snapshot import` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SnapshotImport {
    pub schema: u32,
    pub manifest: SnapshotManifest,
    /// False when this binary computes another facts version over the imported rows.
    pub facts_match: bool,
    /// Repos a rescan was queued for because `facts_match` is false.
    pub rescans: Vec<String>,
    /// Repos with no root here, which answer from these rows alone.
    pub no_root: Vec<String>,
    pub warnings: Vec<String>,
}

/// File name of the verify copy, in the cache folder.
const VERIFY_FILE: &str = "snapshot-verify.tmp";

impl Katalogr {
    /// Install `file` as the base's database. `expect_digest` is compared before any decoding.
    pub fn snapshot_import(
        &self,
        file: &Utf8Path,
        replace: bool,
        expect_digest: Option<&str>,
        o: &Options,
    ) -> Result<SnapshotImport, DistError> {
        let deadline = Instant::now() + o.timeout()?;
        let file = absolute(file)?;
        // Everything that can be checked from the header is checked before a byte is decoded.
        let manifest = read_manifest(&file)?;
        if let Some(want) = expect_digest
            && !want.eq_ignore_ascii_case(&manifest.digest)
        {
            return Err(StoreError::BadSnapshot(
                "the snapshot's digest differs from the one given with --digest".into(),
            )
            .into());
        }
        if manifest.cache_schema != CACHE_SCHEMA {
            return Err(refused(format!(
                "the snapshot is for cache schema {}, this binary uses {CACHE_SCHEMA}: use the binary that wrote it, or index from scratch",
                manifest.cache_schema
            )));
        }
        let listed: Vec<&str> = self
            .project
            .config()
            .repos
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        if let Some(p) = manifest
            .positions
            .iter()
            .find(|p| !listed.contains(&p.repo.as_str()))
        {
            return Err(refused(format!(
                "the snapshot holds repo {}, which this project does not list",
                p.repo
            )));
        }
        let cache_dir = self.project.cache_dir();
        std::fs::create_dir_all(&cache_dir).map_err(|e| DistError::io(&cache_dir, e))?;
        let Some(lock) = lock_until(&cache_dir, deadline)? else {
            return Err(refused("the index is busy: another process holds its lock"));
        };
        if cache_dir.join("index.redb").exists() && !replace {
            return Err(refused(
                "an index already exists here: pass --replace to install the snapshot over it",
            ));
        }
        install_snapshot(&file, &manifest, &cache_dir)?;
        let result = self.after_import(&manifest, deadline);
        drop(lock);
        result
    }

    /// Opens the new base, compares facts versions and queues what a mismatch needs.
    fn after_import(
        &self,
        manifest: &SnapshotManifest,
        deadline: Instant,
    ) -> Result<SnapshotImport, DistError> {
        let engine = self.engine_on(self.project.clone(), deadline)?;
        let facts = {
            let snap = engine.cache().read()?;
            engine.facts_version(&snap)?
        };
        let facts_match = facts == manifest.facts_version;
        let mut rescans = Vec::new();
        let mut warnings = Vec::new();
        if !facts_match {
            for r in &self.project.config().repos {
                if engine.roots().contains_key(&r.name) {
                    engine.inbox().append(&NewEvent::rescan(&r.name))?;
                    rescans.push(r.name.clone());
                } else {
                    warnings.push(format!(
                        "repo {} has no root here, so its rows cannot be re-extracted for this binary's facts version; add it with katalogr project add-root",
                        r.name
                    ));
                }
            }
        }
        let no_root = manifest
            .positions
            .iter()
            .filter(|p| !engine.roots().contains_key(&p.repo))
            .map(|p| p.repo.clone())
            .collect();
        warnings.extend(self.take_notes());
        Ok(SnapshotImport {
            schema: 1,
            manifest: manifest.clone(),
            facts_match,
            rescans,
            no_root,
            warnings,
        })
    }

    /// The manifest; with `verify`, decodes to a temp file in the cache folder, then removes it.
    pub fn snapshot_show(
        &self,
        file: &Utf8Path,
        verify: bool,
    ) -> Result<SnapshotManifest, DistError> {
        let file = absolute(file)?;
        let manifest = read_manifest(&file)?;
        if verify {
            let cache_dir = self.project.cache_dir();
            std::fs::create_dir_all(&cache_dir).map_err(|e| DistError::io(&cache_dir, e))?;
            let tmp = cache_dir.join(VERIFY_FILE);
            let r = decode_snapshot(&file, &manifest, &tmp);
            let _ = std::fs::remove_file(&tmp);
            r?;
        }
        Ok(manifest)
    }
}
