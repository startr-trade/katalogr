//! `katalogr doctor` entry point (FEAT-064): resolves the project the way every command does,
//! but turns each resolution failure into a finding instead of a usage error, then runs the
//! engine's read-only checks.

use crate::error::DistError;
use crate::{build_registry, roots, setup};
use katalogr_engine::doctor::format_too_new;
pub use katalogr_engine::doctor::{
    CheckRun, CheckStatus, DoctorOptions, Finding, Report, Severity, Summary, Thresholds, Versions,
    schema,
};
use katalogr_store::marker::{self, Overrides as StoreOverrides};
use katalogr_store::{Kindex, ProjectName, StoreError};
use std::path::Path;

/// Settings keys that override the thresholds (`config.yaml`, `settings`).
pub const BACKLOG_WARN_KEY: &str = "doctor.backlog_warn";
pub const PARSE_WARN_RATE_KEY: &str = "doctor.parse_warn_rate";
pub const PARSE_ERROR_RATE_KEY: &str = "doctor.parse_error_rate";
pub const PARSE_MIN_FILES_KEY: &str = "doctor.parse_min_files";
pub const SAMPLE_KEY: &str = "doctor.sample";

fn unresolved(finding: Finding, versions: Versions, opts: &DoctorOptions) -> Report {
    Report::unresolved(vec![finding], versions, opts.fail_on)
}

fn marker_finding(code: &str, severity: Severity, message: &str, fix: &str) -> Finding {
    Finding::new(code, severity, "marker", message).fix(fix)
}

/// The thresholds of `opts` with the project's `doctor.*` settings applied.
fn with_settings(
    opts: &DoctorOptions,
    settings: &std::collections::BTreeMap<String, serde_yaml_ng::Value>,
) -> DoctorOptions {
    let mut o = opts.clone();
    let t = &mut o.thresholds;
    let uint = |k: &str| settings.get(k).and_then(|v| v.as_u64()).map(|n| n as usize);
    let rate = |k: &str| settings.get(k).and_then(|v| v.as_f64());
    t.backlog_warn = uint(BACKLOG_WARN_KEY).unwrap_or(t.backlog_warn);
    t.parse_warn_rate = rate(PARSE_WARN_RATE_KEY).unwrap_or(t.parse_warn_rate);
    t.parse_error_rate = rate(PARSE_ERROR_RATE_KEY).unwrap_or(t.parse_error_rate);
    t.parse_min_files = uint(PARSE_MIN_FILES_KEY).unwrap_or(t.parse_min_files);
    t.sample = uint(SAMPLE_KEY).unwrap_or(t.sample);
    o
}

/// Checks the index of the project found from the flags, the environment and the `.katalogr`
/// marker above `start_dir`. Never returns a usage error for an unresolvable project: that is
/// a `KD-MRK` finding. Writes nothing.
pub fn doctor(
    start_dir: &Path,
    overrides: &crate::Overrides,
    opts: &DoctorOptions,
) -> Result<Report, DistError> {
    let start = setup::absolute(start_dir)?;
    let kindex = overrides
        .kindex
        .as_deref()
        .map(|k| setup::absolute(Path::new(k)))
        .transpose()?;
    let versions = Versions::of_binary;
    let project = match overrides
        .project
        .as_deref()
        .map(ProjectName::new)
        .transpose()
    {
        Ok(p) => p,
        Err(_) => {
            return Ok(unresolved(
                marker_finding(
                    "KD-MRK-003",
                    Severity::Error,
                    "the project name is not valid",
                    "katalogr project init <name> --repo <repo>=<path>",
                ),
                versions(),
                opts,
            ));
        }
    };
    let resolved = match marker::resolve(&start, &StoreOverrides { kindex, project }) {
        Ok(r) => r,
        Err(StoreError::NotConfigured) => {
            return Ok(unresolved(
                marker_finding(
                    "KD-MRK-001",
                    Severity::Error,
                    "no kindex or project is configured (no flags, environment or .katalogr marker)",
                    "katalogr project init <name> --repo <repo>=<path> --mark",
                ),
                versions(),
                opts,
            ));
        }
        Err(_) => {
            return Ok(unresolved(
                marker_finding(
                    "KD-MRK-001",
                    Severity::Error,
                    "the .katalogr marker or the environment cannot be read",
                    "edit .katalogr",
                ),
                versions(),
                opts,
            ));
        }
    };
    let kx = match Kindex::open(&resolved.kindex) {
        Ok(k) => k,
        Err(_) => {
            return Ok(unresolved(
                marker_finding(
                    "KD-MRK-002",
                    Severity::Error,
                    "the kindex directory is missing or is not a kindex",
                    "katalogr kindex init <kindex-dir>",
                ),
                versions(),
                opts,
            ));
        }
    };
    let dir = match kx.project(&resolved.project) {
        Ok(d) => d,
        Err(StoreError::FormatTooNew {
            found,
            min_katalogr,
            ..
        }) => {
            let mut v = versions();
            v.kindex_shard_format = Some(found);
            return Ok(unresolved(format_too_new(found, &min_katalogr), v, opts));
        }
        Err(StoreError::NotAProject(_)) => {
            return Ok(unresolved(
                Finding::new(
                    "KD-FMT-001",
                    Severity::Error,
                    "format",
                    "format.json is unreadable or invalid",
                )
                .fix("git -C <kindex-dir> checkout -- format.json"),
                versions(),
                opts,
            ));
        }
        Err(StoreError::ProjectNotFound(_)) | Err(StoreError::Parse { .. }) | Err(_) => {
            return Ok(unresolved(
                marker_finding(
                    "KD-MRK-003",
                    Severity::Error,
                    "the project is not in the kindex or its configuration cannot be read",
                    "katalogr project init <name> --repo <repo>=<path>",
                ),
                versions(),
                opts,
            ));
        }
    };
    let mut roots = roots::read(&dir.cache_dir())?;
    if let (Some(repo), Some(root)) = (&resolved.repo, &resolved.repo_root) {
        roots.insert(repo.clone(), root.clone());
    }
    let opts = with_settings(opts, &dir.config().settings);
    Ok(katalogr_engine::doctor::run(
        &dir,
        &build_registry(),
        &roots,
        resolved.repo.as_deref(),
        &opts,
    )?)
}
