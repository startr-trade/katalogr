//! `snapshot export` (FEAT-097): a compacted, checked copy of the graph with a manifest of
//! positions, written outside git. Import is in [`crate::snapshot_import`].

use crate::error::DistError;
use crate::{Consistency, Katalogr, Options};
use camino::{Utf8Path, Utf8PathBuf};
use katalogr_engine::queue::lock::lock_until;
use katalogr_engine::scan::output_outside_git;
use katalogr_engine::{Engine, QueryOptions, Scope};
use katalogr_store::{
    CACHE_SCHEMA, ExportPlan, ReadTx, SNAPSHOT_FORMAT, SnapshotManifest, SnapshotPosition, TableId,
    write_snapshot,
};
use schemars::JsonSchema;
use serde::Serialize;
use std::time::Instant;

/// File name of the export's temp copy, beside `index.redb`.
pub const EXPORT_FILE: &str = "index.redb.export";

/// What `snapshot export` wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SnapshotExport {
    pub schema: u32,
    pub manifest: SnapshotManifest,
    pub file_bytes: u64,
    /// Repos left out because they have no root here and no rows.
    pub warnings: Vec<String>,
}

pub(crate) fn refused(message: impl Into<String>) -> DistError {
    DistError::Refused {
        exit: 5,
        message: message.into(),
    }
}

/// Builds the manifest's positions and the export's plan from one read transaction: skip Pending
/// and Events, and for each position set `base_commit.<repo>` to its commit, `position.<repo>` to
/// its clean JSON and `position_changed.<repo>` to the empty set. Drops `layer_seed_commit` and
/// `layer_base_errors`. Refuses a dirty or missing commit. A repo with no record and no root is
/// left out with a warning; one with a root and no record is refused.
fn snapshot_plan(
    engine: &Engine,
    snap: &ReadTx,
    repos: &[String],
    warnings: &mut Vec<String>,
) -> Result<(Vec<SnapshotPosition>, ExportPlan), DistError> {
    let mut plan = ExportPlan {
        skip: vec![TableId::Pending, TableId::Events],
        ..Default::default()
    };
    plan.meta.insert("layer_seed_commit".into(), None);
    plan.meta.insert("layer_base_errors".into(), None);
    let mut positions = Vec::new();
    for repo in repos {
        let Some(p) = engine.effective_position(snap, repo)? else {
            if engine.is_pinned(repo) {
                return Err(refused(format!(
                    "repo {repo} is pinned with no commit: run `katalogr index`, which advances the base to the default branch of the repo"
                )));
            }
            if engine.roots().contains_key(repo) {
                return Err(refused(format!(
                    "repo {repo} has no recorded position: read it first, with `katalogr index`"
                )));
            }
            warnings.push(format!(
                "repo {repo} has no root here and no rows, so it is left out of the snapshot"
            ));
            continue;
        };
        if p.dirty.is_some() {
            return Err(refused(format!(
                "repo {repo} has uncommitted changes: a snapshot holds a commit, so commit or stash first"
            )));
        }
        let Some(commit) = p.commit.filter(|c| !c.is_empty()) else {
            return Err(refused(format!(
                "repo {repo} has no commit: a snapshot holds a commit"
            )));
        };
        let json = serde_json::json!({"repo": repo, "commit": commit, "dirty": null});
        plan.meta.insert(
            katalogr_engine::consistency::POSITION_META.to_string() + repo,
            Some(json.to_string().into_bytes()),
        );
        plan.meta.insert(
            katalogr_engine::consistency::POSITION_CHANGED_META.to_string() + repo,
            Some(br#"{"over":false,"paths":[]}"#.to_vec()),
        );
        plan.meta.insert(
            crate::contexts::base_commit_key(repo),
            Some(commit.clone().into_bytes()),
        );
        positions.push(SnapshotPosition {
            repo: repo.clone(),
            commit,
        });
    }
    positions.sort();
    if positions.is_empty() {
        return Err(refused("no repo has rows to export".to_string()));
    }
    Ok((positions, plan))
}

impl Katalogr {
    /// Export the context this invocation reads. Refusals use `DistError::Refused`, exit 5, and a
    /// tracked output path `DistError::Usage`, exit 2.
    pub fn snapshot_export(
        &self,
        out: &Utf8Path,
        o: &Options,
    ) -> Result<SnapshotExport, DistError> {
        let timeout = o.timeout()?;
        let deadline = Instant::now() + timeout;
        let out = absolute(out)?;
        if !output_outside_git(out.as_std_path())? {
            return Err(DistError::usage(
                "the output path is inside a git work tree and not ignored: a snapshot must never be committed",
            ));
        }
        if out.exists() {
            return Err(refused(
                "the output file exists: remove it or choose another",
            ));
        }
        let engine = self.engine(deadline)?;
        let a = engine.with_consistency(
            &QueryOptions {
                consistency: Consistency::WaitAll.into(),
                timeout,
                scope: Scope::Project,
            },
            |_| (),
        )?;
        // Events still in the inbox are not pending yet: nobody has ingested them.
        if a.stale || a.pending > 0 || !engine.inbox().list()?.is_empty() {
            return Err(refused(
                "the index is not current: run `katalogr index`, or raise --timeout",
            ));
        }
        let cache_dir = engine.dir().cache_dir();
        let Some(lock) = lock_until(&cache_dir, deadline)? else {
            return Err(refused("the index is busy: another process holds its lock"));
        };
        let tmp = cache_dir.join(EXPORT_FILE);
        let _ = std::fs::remove_file(&tmp);
        let result = (|| -> Result<SnapshotExport, DistError> {
            let repos: Vec<String> = self
                .project
                .config()
                .repos
                .iter()
                .map(|r| r.name.clone())
                .collect();
            let mut warnings = Vec::new();
            let (positions, plan, facts) = {
                let snap = engine.cache().read()?;
                let (positions, plan) = snapshot_plan(&engine, &snap, &repos, &mut warnings)?;
                (positions, plan, engine.facts_version(&snap)?)
            };
            engine.cache().export_copy(&tmp, &plan, deadline)?;
            let manifest = SnapshotManifest {
                format: SNAPSHOT_FORMAT,
                bytes: 0,
                positions,
                facts_version: facts,
                cache_schema: CACHE_SCHEMA,
                digest: String::new(),
            };
            let manifest = write_snapshot(&tmp, manifest, &out)?;
            let file_bytes = std::fs::metadata(&out)
                .map_err(|e| DistError::io(&out, e))?
                .len();
            Ok(SnapshotExport {
                schema: 1,
                manifest,
                file_bytes,
                warnings,
            })
        })();
        let _ = std::fs::remove_file(&tmp);
        drop(lock);
        result
    }
}

pub(crate) fn absolute(p: &Utf8Path) -> Result<Utf8PathBuf, DistError> {
    crate::setup::absolute(p.as_std_path())
}

/// The JSON Schema of a [`SnapshotManifest`] (`snapshot show --json`).
pub fn manifest_schema() -> serde_json::Value {
    schemars::schema_for!(SnapshotManifest).to_value()
}
