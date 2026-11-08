//! One function per area (FEAT-064 section 4). A check reads; it never writes.

use super::model::{DoctorOptions, Finding, Severity};
use crate::profiles::ProfileError;
use crate::queue::{Inbox, find_orphan_temps};
use crate::registry::ExtensionRegistry;
use camino::{Utf8Path, Utf8PathBuf};
use katalogr_store::{
    GitState, InspectedTables, ProjectDir, SHARD_FORMAT, ShardKey, ShardStat, StoreError,
};
use sha2::{Digest, Sha256};
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

fn key_str(k: &ShardKey) -> String {
    format!("{}:{}", k.0, k.1.as_str())
}

const COMMIT_FIX: &str = "git -C <kindex-dir> add projects/<project>/shards && git -C <kindex-dir> commit -m \"index update\"";

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

/// KD-FMT-001/002. Returns the findings and the format number read.
pub(crate) fn format(dir: &ProjectDir) -> (Vec<Finding>, Option<u32>) {
    match dir.format_file() {
        Ok(f) => {
            let mut out = Vec::new();
            if f.shard_format > SHARD_FORMAT {
                out.push(format_too_new(f.shard_format, &f.min_katalogr));
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

/// KD-LCK-001 and the journal checks need to know whether a processor runs.
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

fn orphan_temps(dir: &ProjectDir) -> Result<Vec<Utf8PathBuf>, String> {
    find_orphan_temps(&dir.root().join("shards")).map_err(|e| engine_kind(&e))
}

/// `p` relative to `base`, always `/`-separated (reports read the same on every system).
fn rel_to(base: &Utf8Path, p: &Utf8Path) -> String {
    let rel = p.strip_prefix(base).unwrap_or(p);
    rel.components()
        .map(|c| c.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

/// KD-JNL-001 to 003. `tables` is None when the cache could not be read: only the orphan-temp
/// part can run then.
pub(crate) fn journal(
    dir: &ProjectDir,
    tables: Option<&InspectedTables>,
    held: bool,
    o: &DoctorOptions,
) -> CheckResult {
    let mut out = Vec::new();
    let entries = tables.map(|t| t.journal.as_slice()).unwrap_or(&[]);
    if !entries.is_empty() {
        let key = |fin: &str| {
            fin.strip_prefix("shards/")
                .and_then(|s| s.strip_suffix(katalogr_store::shard::SUFFIX))
                .map(|s| s.replacen('/', ":", 1))
                .unwrap_or_else(|| fin.to_string())
        };
        let sample = entries.iter().map(|(_, fin)| key(fin));
        if held {
            out.push(
                Finding::new(
                    "KD-JNL-002",
                    Severity::Info,
                    "journal",
                    format!(
                        "a publish is in flight: {} journal entries while a processor is running",
                        entries.len()
                    ),
                )
                .count(entries.len())
                .sample(sample, o),
            );
        } else {
            let temps = entries
                .iter()
                .filter(|(tmp, _)| !tmp.is_empty() && dir.root().join(tmp).exists())
                .count();
            out.push(
                Finding::new(
                    "KD-JNL-001",
                    Severity::Error,
                    "journal",
                    format!(
                        "interrupted publish: {} journal entries, no processor running ({temps} temp files still present)",
                        entries.len()
                    ),
                )
                .fix("katalogr index")
                .count(entries.len())
                .sample(sample, o),
            );
        }
    } else if !held {
        let temps = orphan_temps(dir)?;
        if !temps.is_empty() {
            let base = dir.root().join("shards");
            out.push(
                Finding::new(
                    "KD-JNL-003",
                    Severity::Warning,
                    "journal",
                    format!(
                        "{} orphan temp files in the shards folder, no processor running",
                        temps.len()
                    ),
                )
                .fix("katalogr index")
                .count(temps.len())
                .sample(temps.iter().map(|p| rel_to(&base, p)), o),
            );
        }
    }
    done(out)
}

fn quarantined(dir: &ProjectDir) -> Result<Vec<String>, String> {
    fn walk(dir: &Utf8Path, base: &Utf8Path, out: &mut Vec<String>) -> Result<(), String> {
        let rd = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.kind().to_string()),
        };
        for e in rd {
            let e = e.map_err(|e| e.kind().to_string())?;
            let Some(name) = e.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let path = dir.join(name);
            if e.file_type().map_err(|e| e.kind().to_string())?.is_dir() {
                walk(&path, base, out)?;
            } else {
                out.push(rel_to(base, &path).replacen('/', ":", 1));
            }
        }
        Ok(())
    }
    let base = dir.cache_dir().join("quarantine");
    let mut out = Vec::new();
    walk(&base, &base, &mut out)?;
    out.sort();
    Ok(out)
}

/// KD-SHD-001 to 003. `tables` is None when the cache could not be read: only quarantine runs.
pub(crate) fn shards(
    dir: &ProjectDir,
    files: &[ShardKey],
    tables: Option<&InspectedTables>,
    held: bool,
    o: &DoctorOptions,
) -> CheckResult {
    let mut out = Vec::new();
    // A publish in flight makes the mismatches expected.
    let sev = if held {
        Severity::Info
    } else {
        Severity::Warning
    };
    if let Some(t) = tables {
        let rows: BTreeMap<&ShardKey, (&[u8; 32], &Option<ShardStat>)> =
            t.shard_hashes.iter().map(|(k, h, s)| (k, (h, s))).collect();
        let mut on_disk: BTreeSet<&ShardKey> = BTreeSet::new();
        let (mut new, mut changed): (Vec<&ShardKey>, Vec<&ShardKey>) = (Vec::new(), Vec::new());
        for key in files {
            on_disk.insert(key);
            let Some((hash, stat)) = rows.get(key) else {
                new.push(key);
                continue;
            };
            let file = dir.shard_path(&key.0, &key.1);
            let meta = std::fs::metadata(&file).map_err(|e| e.kind().to_string())?;
            let now = ShardStat::of(&meta);
            // An unchanged size and mtime are trusted, as the cache's own sync does.
            if stat.is_some() && **stat == now {
                continue;
            }
            let bytes = std::fs::read(&file).map_err(|e| e.kind().to_string())?;
            let digest: [u8; 32] = Sha256::digest(&bytes).into();
            if &digest != *hash {
                changed.push(key);
            }
        }
        let gone: Vec<&ShardKey> = rows
            .keys()
            .filter(|k| !on_disk.contains(*k))
            .copied()
            .collect();
        if !new.is_empty() || !changed.is_empty() {
            let n = new.len() + changed.len();
            out.push(
                Finding::new(
                    "KD-SHD-001",
                    sev,
                    "shards",
                    format!(
                        "{n} shard files are new or differ from the cache ({} new, {} changed)",
                        new.len(),
                        changed.len()
                    ),
                )
                .fix("katalogr index")
                .count(n)
                .sample(new.iter().chain(changed.iter()).map(|k| key_str(k)), o),
            );
        }
        if !gone.is_empty() {
            out.push(
                Finding::new(
                    "KD-SHD-002",
                    sev,
                    "shards",
                    format!("{} cache hash rows have no shard file", gone.len()),
                )
                .fix("katalogr index")
                .count(gone.len())
                .sample(gone.iter().map(|k| key_str(k)), o),
            );
        }
    }
    let q = quarantined(dir)?;
    if !q.is_empty() {
        out.push(
            Finding::new(
                "KD-SHD-003",
                Severity::Warning,
                "shards",
                format!(
                    "{} shard files in quarantine (they could not be decoded)",
                    q.len()
                ),
            )
            .fix("katalogr index --full")
            .count(q.len())
            .sample(q, o),
        );
    }
    done(out)
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
    files: &[ShardKey],
    tables: &InspectedTables,
    o: &DoctorOptions,
) -> CheckResult {
    let mut total: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (_, path) in files {
        if let Some(e) = registry.for_path(path) {
            *total.entry(e.id()).or_default() += 1;
        }
    }
    let mut bad: BTreeMap<&'static str, Vec<&ShardKey>> = BTreeMap::new();
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
        GitState::NoCommits { shard_files } => out.push(
            Finding::new(
                "KD-GIT-003",
                Severity::Info,
                "git",
                format!("the kindex repository has no commit ({shard_files} shard files)"),
            )
            .fix(commit_fix(dir))
            .count(shard_files),
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
                            "{n} shard files differ from HEAD in the kindex ({} modified, {} added, {} deleted)",
                            modified.len(),
                            added.len(),
                            deleted.len()
                        ),
                    )
                    .fix(commit_fix(dir))
                    .count(n)
                    .sample(
                        modified.iter().chain(&added).chain(&deleted).map(key_str),
                        o,
                    ),
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
