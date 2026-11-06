//! One function per area (FEAT-064 section 4). A check reads; it never writes.

use super::model::{DoctorOptions, Finding, Severity};
use crate::profiles::ProfileError;
use crate::queue::Inbox;
use crate::registry::ExtensionRegistry;
use camino::Utf8PathBuf;
use katalogr_store::{GitState, InspectedTables, PathKey, ProjectDir, SHARD_FORMAT, StoreError};
use std::collections::{BTreeMap, BTreeSet};

/// What a check did: findings (possibly none), or it did not apply.
pub(crate) enum Ran {
    Done(Vec<Finding>),
    Skipped,
}

/// A failed check: the io kind (never a path).
pub(crate) type CheckResult = Result<Ran, String>;

pub(crate) fn store_kind(e: &StoreError) -> String {
    match e {
        StoreError::Io { source, .. } => source.kind().to_string(),
        _ => "error".to_string(),
    }
}

pub(crate) fn engine_kind(e: &crate::EngineError) -> String {
    match e {
        crate::EngineError::Io { source, .. } => source.kind().to_string(),
        crate::EngineError::Store(s) => store_kind(s),
        _ => "error".to_string(),
    }
}

fn done(v: Vec<Finding>) -> CheckResult {
    Ok(Ran::Done(v))
}

fn key_str(k: &PathKey) -> String {
    format!("{}:{}", k.0, k.1.as_str())
}

const COMMIT_FIX: &str = "git -C <kindex-dir> add projects/<project> && git -C <kindex-dir> commit -m \"update project\"";

const REMOVE_SHARDS_FIX: &str = "git -C <kindex-dir> rm -r -q projects/<project>/shards && git -C <kindex-dir> commit -m \"remove shards\"";

fn project_name(dir: &ProjectDir) -> String {
    dir.root().file_name().unwrap_or_default().to_string()
}

fn commit_fix(dir: &ProjectDir) -> String {
    COMMIT_FIX.replace("<project>", &project_name(dir))
}

/// KD-MRK-004: the marker names a repository the project does not have.
pub(crate) fn marker(dir: &ProjectDir, marker_repo: Option<&str>) -> CheckResult {
    let mut out = Vec::new();
    if let Some(repo) = marker_repo
        && !dir.config().repos.iter().any(|r| r.name == repo)
    {
        out.push(
            Finding::new(
                "KD-MRK-004",
                Severity::Warning,
                "marker",
                format!(
                    "the .katalogr marker names repository {repo}, which the project does not have"
                ),
            )
            .fix("edit .katalogr"),
        );
    }
    done(out)
}

/// The finding for a shard format newer than this binary supports.
pub fn format_too_new(found: u32, min_katalogr: &str) -> Finding {
    Finding::new(
        "KD-FMT-002",
        Severity::Error,
        "format",
        format!(
            "the kindex shard format {found} is newer than the supported format {SHARD_FORMAT}"
        ),
    )
    .fix(format!("install katalogr >= {min_katalogr}"))
}

/// KD-FMT-001/002 and KD-SHD-004. Returns the findings and the format number read.
pub(crate) fn format(dir: &ProjectDir) -> (Vec<Finding>, Option<u32>) {
    match dir.format_file() {
        Ok(f) => {
            let mut out = Vec::new();
            if f.shard_format > SHARD_FORMAT {
                out.push(format_too_new(f.shard_format, &f.min_katalogr));
            }
            if dir.leftover_shards() {
                out.push(
                    Finding::new(
                        "KD-SHD-004",
                        Severity::Warning,
                        "format",
                        "the project has a shards folder from an earlier release; nothing reads it now",
                    )
                    .fix(REMOVE_SHARDS_FIX.replace("<project>", &project_name(dir))),
                );
            }
            (out, Some(f.shard_format))
        }
        Err(_) => (
            vec![
                Finding::new(
                    "KD-FMT-001",
                    Severity::Error,
                    "format",
                    "format.json is unreadable or invalid",
                )
                .fix("git -C <kindex-dir> checkout -- format.json"),
            ],
            None,
        ),
    }
}

/// KD-LCK-001.
pub(crate) fn lock(held: bool) -> CheckResult {
    done(if held {
        vec![Finding::new(
            "KD-LCK-001",
            Severity::Info,
            "lock",
            "a processor is running (the processor lock is held)",
        )]
    } else {
        Vec::new()
    })
}

/// KD-REV-001: the file revisions and the artifacts disagree. Counts only, no keys.
pub(crate) fn revisions(tables: &InspectedTables) -> CheckResult {
    let revs: BTreeSet<&PathKey> = tables.file_revisions.iter().map(|(k, _)| k).collect();
    let arts: BTreeSet<&PathKey> = tables.artifact_keys.iter().collect();
    let without_artifact = revs.difference(&arts).count();
    let without_revision = arts.difference(&revs).count();
    let n = without_artifact + without_revision;
    done(if n == 0 {
        Vec::new()
    } else {
        vec![
            Finding::new(
                "KD-REV-001",
                Severity::Warning,
                "revisions",
                format!(
                    "{n} files disagree between the file revisions and the artifacts ({without_artifact} revisions without an artifact, {without_revision} artifacts without a revision)"
                ),
            )
            .fix("katalogr index --full")
            .count(n),
        ]
    })
}

/// KD-QUE-001 to 003.
pub(crate) fn queue(dir: &ProjectDir, tables: &InspectedTables, o: &DoctorOptions) -> CheckResult {
    let inbox = Inbox::new(dir);
    let queued = inbox.list().map_err(|e| engine_kind(&e))?.len();
    let bad = match std::fs::read_dir(inbox.dir()) {
        Ok(rd) => {
            let mut n = 0;
            for e in rd {
                let e = e.map_err(|e| e.kind().to_string())?;
                if e.file_name().to_string_lossy().ends_with(".bad") {
                    n += 1;
                }
            }
            n
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e.kind().to_string()),
    };
    let total = tables.pending.len() + queued;
    let mut out = Vec::new();
    if total > 0 {
        let message = format!(
            "{total} changes are waiting ({} pending paths, {queued} inbox files)",
            tables.pending.len()
        );
        let sev = if total >= o.thresholds.backlog_warn {
            Severity::Warning
        } else {
            Severity::Info
        };
        let code = if sev == Severity::Warning {
            "KD-QUE-001"
        } else {
            "KD-QUE-002"
        };
        out.push(
            Finding::new(code, sev, "queue", message)
                .fix("katalogr index")
                .count(total)
                .sample(
                    tables
                        .pending
                        .iter()
                        .map(|(k, _)| format!("{}:{}", k.repo, k.path.as_str())),
                    o,
                ),
        );
    }
    if bad > 0 {
        out.push(
            Finding::new(
                "KD-QUE-003",
                Severity::Warning,
                "queue",
                format!("{bad} inbox events were set aside because they could not be read"),
            )
            .fix("remove the *.bad files from the project's inbox directory after checking them")
            .count(bad),
        );
    }
    done(out)
}

/// KD-PRS-001/002.
pub(crate) fn parse(
    registry: &ExtensionRegistry,
    files: &[PathKey],
    tables: &InspectedTables,
    o: &DoctorOptions,
) -> CheckResult {
    let mut total: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (_, path) in files {
        if let Some(e) = registry.for_path(path) {
            *total.entry(e.id()).or_default() += 1;
        }
    }
    let mut bad: BTreeMap<&'static str, Vec<&PathKey>> = BTreeMap::new();
    for (key, _) in &tables.error_files {
        if let Some(e) = registry.for_path(&key.1) {
            bad.entry(e.id()).or_default().push(key);
            // A file with errors but no shard (quarantined) still belongs to the extension.
            if files.binary_search(key).is_err() {
                *total.entry(e.id()).or_default() += 1;
            }
        }
    }
    let mut out = Vec::new();
    for (ext, keys) in bad {
        let n = total.get(ext).copied().unwrap_or(0);
        if n < o.thresholds.parse_min_files || n == 0 {
            continue;
        }
        let rate = keys.len() as f64 / n as f64;
        let sev = if rate >= o.thresholds.parse_error_rate {
            Severity::Error
        } else if rate >= o.thresholds.parse_warn_rate {
            Severity::Warning
        } else {
            continue;
        };
        let code = if sev == Severity::Error {
            "KD-PRS-002"
        } else {
            "KD-PRS-001"
        };
        out.push(
            Finding::new(
                code,
                sev,
                "parse",
                format!(
                    "extension {ext}: {} of {n} files have parse errors ({:.1}%)",
                    keys.len(),
                    rate * 100.0
                ),
            )
            .fix("katalogr doctor --verbose")
            .count(keys.len())
            .sample(keys.iter().map(|k| key_str(k)), o),
        );
    }
    done(out)
}

/// KD-PRF-001: one finding per refused profile file.
pub(crate) fn profiles(dir: &ProjectDir, tables: &InspectedTables) -> CheckResult {
    let errors: Vec<ProfileError> = tables
        .profile_errors_json
        .as_deref()
        .and_then(|b| serde_json::from_slice(b).ok())
        .unwrap_or_default();
    let project = project_name(dir);
    let out = errors
        .iter()
        .map(|e| {
            let text = e.to_string();
            let message: String = text.chars().take(200).collect();
            let f = Finding::new("KD-PRF-001", Severity::Warning, "profile", message);
            if e.file.starts_with("profiles/") {
                f.fix(format!("edit projects/{project}/{}", e.file))
            } else {
                f
            }
        })
        .collect();
    done(out)
}

/// KD-GIT-001 to 003. Skipped with `--skip-git`.
pub(crate) fn git(dir: &ProjectDir, o: &DoctorOptions) -> CheckResult {
    if o.skip_git {
        return Ok(Ran::Skipped);
    }
    let state = dir.git_state().map_err(|e| store_kind(&e))?;
    let mut out = Vec::new();
    match state {
        GitState::NotARepo => out.push(
            Finding::new(
                "KD-GIT-002",
                Severity::Warning,
                "git",
                "the kindex is not a git repository",
            )
            .fix("katalogr kindex init <kindex-dir>"),
        ),
        GitState::NoCommits => out.push(
            Finding::new(
                "KD-GIT-003",
                Severity::Info,
                "git",
                "the kindex repository has no commit yet",
            )
            .fix(commit_fix(dir)),
        ),
        GitState::Repo {
            modified,
            added,
            deleted,
            ..
        } => {
            let n = modified.len() + added.len() + deleted.len();
            if n > 0 {
                out.push(
                    Finding::new(
                        "KD-GIT-001",
                        Severity::Warning,
                        "git",
                        format!(
                            "{n} files people write differ from HEAD in the kindex ({} modified, {} added, {} deleted)",
                            modified.len(),
                            added.len(),
                            deleted.len()
                        ),
                    )
                    .fix(commit_fix(dir))
                    .count(n)
                    .sample(modified.iter().chain(&added).chain(&deleted).cloned(), o),
                );
            }
        }
    }
    done(out)
}

/// KD-ROOT-001: one finding per project repository with no root on this machine.
pub(crate) fn roots(dir: &ProjectDir, roots: &BTreeMap<String, Utf8PathBuf>) -> CheckResult {
    done(
        dir.config()
            .repos
            .iter()
            .filter(|r| !roots.contains_key(&r.name))
            .map(|r| {
                Finding::new(
                    "KD-ROOT-001",
                    Severity::Warning,
                    "roots",
                    format!("repository {} has no root on this machine", r.name),
                )
                .fix(format!("katalogr project add-root {} <path>", r.name))
            })
            .collect(),
    )
}
