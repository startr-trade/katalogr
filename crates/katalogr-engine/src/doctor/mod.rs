//! `katalogr doctor` (FEAT-064): read-only index health checks with recommended fixes.
//!
//! Doctor never replays the journal, rebuilds the cache or quarantines a shard. It opens the
//! cache read-only ([`katalogr_store::Cache::inspect`]) and reads the shard files, the inbox and
//! the kindex git repository. Output carries no absolute path, source text or event payload.

mod checks;
mod model;

pub use checks::format_too_new;
pub use model::{
    AREAS, CheckRun, CheckStatus, DoctorOptions, Finding, REPORT_SCHEMA_VERSION, Report, Severity,
    Summary, Thresholds, Versions,
};

use crate::error::EngineError;
use crate::registry::ExtensionRegistry;
use camino::Utf8PathBuf;
use checks::{CheckResult, Ran};
use katalogr_store::{CACHE_SCHEMA, Cache, CacheInspection, InspectedTables, ProjectDir, ShardKey};
use std::collections::BTreeMap;
use std::fs::File;

/// The JSON Schema of [`Report`].
pub fn schema() -> serde_json::Value {
    schemars::schema_for!(Report).to_value()
}

/// Takes the processor lock without creating it: a project whose lock file does not exist has
/// never had a processor, so nothing can hold it. `Ok(None)` and `held == false` then.
fn acquire(dir: &ProjectDir) -> Result<(Option<File>, bool), EngineError> {
    let cache_dir = dir.cache_dir();
    if !cache_dir.join("processor.lock").exists() {
        return Ok((None, false));
    }
    match crate::queue::lock::try_lock(&cache_dir)? {
        Some(file) => Ok((Some(file), false)),
        None => Ok((None, true)),
    }
}

struct Run {
    checks: Vec<CheckRun>,
    findings: Vec<Finding>,
}

impl Run {
    fn record(&mut self, area: &str, result: CheckResult) {
        let status = match result {
            Ok(Ran::Done(f)) if f.is_empty() => CheckStatus::Ok,
            Ok(Ran::Done(f)) => {
                self.findings.extend(f);
                CheckStatus::Findings
            }
            Ok(Ran::Skipped) => CheckStatus::Skipped,
            Err(kind) => {
                self.findings.push(Finding::new(
                    "KD-CHK-001",
                    Severity::Error,
                    area,
                    format!("check {area} failed: {kind}"),
                ));
                CheckStatus::Findings
            }
        };
        self.checks.push(CheckRun {
            area: area.to_string(),
            status,
        });
    }

    fn skip(&mut self, area: &str) {
        self.record(area, Ok(Ran::Skipped));
    }
}

/// Runs every check on the project. Takes the processor lock for the whole run when it is
/// free and exists, so the journal cannot change underneath it.
pub fn run(
    dir: &ProjectDir,
    registry: &ExtensionRegistry,
    roots: &BTreeMap<String, Utf8PathBuf>,
    marker_repo: Option<&str>,
    opts: &DoctorOptions,
) -> Result<Report, EngineError> {
    let mut versions = Versions::of_binary();
    let mut r = Run {
        checks: Vec::new(),
        findings: Vec::new(),
    };

    r.record("marker", checks::marker(dir, marker_repo));

    let (found, format) = checks::format(dir);
    versions.kindex_shard_format = format;
    r.record("format", Ok(Ran::Done(found)));

    let (_guard, held) = acquire(dir)?;
    r.record("lock", checks::lock(held));

    // The cache.
    let mut tables: Option<InspectedTables> = None;
    let mut cache_findings = Vec::new();
    let cache_fix =
        |code: &str, sev, msg: &str, fix: &str| Finding::new(code, sev, "cache", msg).fix(fix);
    let cache_result: CheckResult = match Cache::inspect(dir) {
        Err(e) => Err(checks::store_kind(&e)),
        Ok(CacheInspection::Missing) => {
            cache_findings.push(cache_fix(
                "KD-CCH-001",
                Severity::Info,
                "there is no cache yet",
                "katalogr index --full",
            ));
            Ok(Ran::Skipped)
        }
        Ok(CacheInspection::InUse) => {
            cache_findings.push(cache_fix(
                "KD-CCH-004",
                Severity::Info,
                "another process holds the cache; the checks that read it were skipped",
                "katalogr doctor",
            ));
            Ok(Ran::Skipped)
        }
        Ok(CacheInspection::Unreadable(why)) => {
            cache_findings.push(cache_fix(
                "KD-CCH-003",
                Severity::Warning,
                &format!("{why}; the next open recreates it"),
                "katalogr index --full",
            ));
            Ok(Ran::Skipped)
        }
        Ok(CacheInspection::Open(o)) => {
            versions.stored_cache_schema = o.stored_schema;
            if o.stored_schema != Some(CACHE_SCHEMA) {
                let stored = o
                    .stored_schema
                    .map_or("unknown".to_string(), |s| s.to_string());
                cache_findings.push(cache_fix(
                    "KD-CCH-002",
                    Severity::Warning,
                    &format!(
                        "the cache schema {stored} differs from the supported schema {CACHE_SCHEMA}; the next open rebuilds it"
                    ),
                    "katalogr index",
                ));
            }
            tables = o.tables;
            Ok(Ran::Done(Vec::new()))
        }
    };
    // A cache that could not be read still gets its findings; the status says findings.
    match cache_result {
        Ok(Ran::Skipped) if !cache_findings.is_empty() => {
            r.findings.extend(cache_findings);
            r.checks.push(CheckRun {
                area: "cache".into(),
                status: CheckStatus::Findings,
            });
        }
        Ok(Ran::Done(_)) => r.record("cache", Ok(Ran::Done(cache_findings))),
        other => r.record("cache", other),
    }

    let files: Result<Vec<ShardKey>, String> =
        dir.list_shards().map_err(|e| checks::store_kind(&e));
    let tref = tables.as_ref();

    r.record("journal", checks::journal(dir, tref, held, opts));
    r.record(
        "shards",
        match &files {
            Ok(f) => checks::shards(dir, f, tref, held, opts),
            Err(k) => Err(k.clone()),
        },
    );
    match tref {
        Some(t) => {
            r.record("queue", checks::queue(dir, t, opts));
            r.record(
                "parse",
                match &files {
                    Ok(f) => checks::parse(registry, f, t, opts),
                    Err(k) => Err(k.clone()),
                },
            );
            r.record("profile", checks::profiles(dir, t));
        }
        None => {
            r.skip("queue");
            r.skip("parse");
            r.skip("profile");
        }
    }
    r.record("git", checks::git(dir, opts));
    r.record("roots", checks::roots(dir, roots));

    let (watermark, pending) = tref.map_or((0, 0), |t| (t.watermark, t.pending.len() as u64));
    Ok(Report::build(
        r.checks,
        r.findings,
        versions,
        watermark,
        pending,
        opts.fail_on,
    ))
}
