//! The doctor report model (FEAT-064 section 3): findings with stable codes, and the report
//! that carries them. Nothing here holds a path, source text or an event payload.

use schemars::JsonSchema;
use serde::Serialize;

/// Version of the `schema/v1` result convention the report follows.
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// How bad a finding is. Ordered so that `Error > Warning > Info`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

/// One problem (or notice) with a stable code and a recommended fix.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Finding {
    /// `KD-<AREA>-<NNN>`: stable, never reused.
    pub code: String,
    pub severity: Severity,
    /// `marker`, `format`, `cache`, `journal`, `shards`, `queue`, `lock`, `parse`, `profile`,
    /// `git` or `roots`.
    pub area: String,
    /// One line, no paths, no source text.
    pub message: String,
    /// A `katalogr` or `git` command, or a one-line instruction such as `edit <file>`.
    pub fix: Option<String>,
    /// How many items the finding covers.
    pub count: Option<u64>,
    /// Repo-relative keys, only with `--verbose`, at most `Thresholds::sample`.
    pub sample: Vec<String>,
}

impl Finding {
    pub fn new(code: &str, severity: Severity, area: &str, message: impl Into<String>) -> Finding {
        Finding {
            code: code.to_string(),
            severity,
            area: area.to_string(),
            message: message.into(),
            fix: None,
            count: None,
            sample: Vec::new(),
        }
    }

    pub fn fix(mut self, fix: impl Into<String>) -> Finding {
        self.fix = Some(fix.into());
        self
    }

    pub fn count(mut self, n: usize) -> Finding {
        self.count = Some(n as u64);
        self
    }

    pub fn severity(mut self, s: Severity) -> Finding {
        self.severity = s;
        self
    }

    /// Records up to `limit` keys, only when `verbose`.
    pub fn sample(mut self, keys: impl IntoIterator<Item = String>, o: &DoctorOptions) -> Finding {
        if o.verbose {
            self.sample = keys.into_iter().take(o.thresholds.sample).collect();
        }
        self
    }
}

/// Whether a check found something.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Findings,
    Skipped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CheckRun {
    pub area: String,
    pub status: CheckStatus,
}

/// The format and schema numbers the report prints.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Versions {
    pub katalogr: String,
    /// The shard format this binary writes and reads.
    pub shard_format: u32,
    /// The shard format named by the kindex's `format.json`, when it could be read.
    pub kindex_shard_format: Option<u32>,
    /// The cache schema this binary uses.
    pub cache_schema: u32,
    /// The cache schema stored in the cache, when there is a readable cache.
    pub stored_cache_schema: Option<u32>,
}

impl Versions {
    /// This binary's numbers, with nothing known about the project yet.
    pub fn of_binary() -> Versions {
        Versions {
            katalogr: env!("CARGO_PKG_VERSION").to_string(),
            shard_format: katalogr_store::SHARD_FORMAT,
            kindex_shard_format: None,
            cache_schema: katalogr_store::CACHE_SCHEMA,
            stored_cache_schema: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Summary {
    pub error: u32,
    pub warning: u32,
    pub info: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Report {
    /// `RESULT_SCHEMA_VERSION`, 1.
    pub schema: u32,
    /// No finding at or above the fail-on level.
    pub healthy: bool,
    pub summary: Summary,
    pub versions: Versions,
    pub watermark: u64,
    /// Queued paths in the cache.
    pub pending: u64,
    /// One entry per check, in the fixed order.
    pub checks: Vec<CheckRun>,
    /// Sorted by severity (worst first), code, message.
    pub findings: Vec<Finding>,
}

/// The checks, in the order they run and are reported.
pub const AREAS: [&str; 11] = [
    "marker", "format", "cache", "lock", "journal", "shards", "queue", "parse", "profile", "git",
    "roots",
];

impl Report {
    /// Sorts the findings and fills the summary and `healthy`.
    pub fn build(
        checks: Vec<CheckRun>,
        mut findings: Vec<Finding>,
        versions: Versions,
        watermark: u64,
        pending: u64,
        fail_on: Severity,
    ) -> Report {
        findings.sort_by(|a, b| {
            b.severity
                .cmp(&a.severity)
                .then_with(|| a.code.cmp(&b.code))
                .then_with(|| a.message.cmp(&b.message))
        });
        let mut summary = Summary::default();
        for f in &findings {
            match f.severity {
                Severity::Error => summary.error += 1,
                Severity::Warning => summary.warning += 1,
                Severity::Info => summary.info += 1,
            }
        }
        Report {
            schema: REPORT_SCHEMA_VERSION,
            healthy: !findings.iter().any(|f| f.severity >= fail_on),
            summary,
            versions,
            watermark,
            pending,
            checks,
            findings,
        }
    }

    /// A report where only the `marker` check ran, with `findings`; every other check is
    /// skipped (the project could not be resolved).
    pub fn unresolved(findings: Vec<Finding>, versions: Versions, fail_on: Severity) -> Report {
        let checks = AREAS
            .iter()
            .map(|a| CheckRun {
                area: a.to_string(),
                status: match (*a, findings.is_empty()) {
                    ("marker", true) => CheckStatus::Ok,
                    ("marker", false) => CheckStatus::Findings,
                    _ => CheckStatus::Skipped,
                },
            })
            .collect();
        Report::build(checks, findings, versions, 0, 0, fail_on)
    }

    /// True when the process should exit 1: some finding is at or above the fail-on level.
    pub fn exit_failing(&self) -> bool {
        !self.healthy
    }
}

/// Limits that turn counts into findings. The defaults are provisional heuristics.
#[derive(Clone, Debug, PartialEq)]
pub struct Thresholds {
    /// Pending plus inbox count that makes the backlog a warning.
    pub backlog_warn: usize,
    /// Share of an extension's files with parse errors that warns.
    pub parse_warn_rate: f64,
    /// The share that is an error.
    pub parse_error_rate: f64,
    /// An extension with fewer files is never reported.
    pub parse_min_files: usize,
    /// Keys listed per finding with `--verbose`.
    pub sample: usize,
}

impl Default for Thresholds {
    fn default() -> Thresholds {
        Thresholds {
            backlog_warn: 1000,
            parse_warn_rate: 0.05,
            parse_error_rate: 0.5,
            parse_min_files: 5,
            sample: 10,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DoctorOptions {
    /// Findings at or above this make the report unhealthy.
    pub fail_on: Severity,
    pub skip_git: bool,
    pub verbose: bool,
    pub thresholds: Thresholds,
}

impl Default for DoctorOptions {
    fn default() -> DoctorOptions {
        DoctorOptions {
            fail_on: Severity::Warning,
            skip_git: false,
            verbose: false,
            thresholds: Thresholds::default(),
        }
    }
}
