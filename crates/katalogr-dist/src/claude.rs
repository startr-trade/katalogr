//! Claude Code wiring (FEAT-024): the generated `CLAUDE.md` block, the embedded skill and the
//! SessionStart project map.

use crate::{Consistency, DistError, Katalogr, Options, Outcome, Overrides};
use katalogr_query::budget::estimate_tokens;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The skill, embedded so it always matches the program.
pub const SKILL_MD: &str = include_str!("../../../skill/katalogr/SKILL.md");

/// Marker lines delimiting the generated block in `CLAUDE.md`.
pub const BEGIN_MARKER: &str = "<!-- katalogr:begin -->";
pub const END_MARKER: &str = "<!-- katalogr:end -->";

/// Token budget of `session-start` when neither `--budget` nor `claude.session_budget` is set
/// (provisional; FEAT-036 re-sets it).
pub const DEFAULT_SESSION_BUDGET: u64 = 1500;
/// The project setting that overrides [`DEFAULT_SESSION_BUDGET`].
pub const SESSION_BUDGET_KEY: &str = "claude.session_budget";
/// Seconds `session-start` waits for the index before printing what it has.
pub const SESSION_TIMEOUT_SECS: f64 = 1.0;

/// What `claude sync` did to `CLAUDE.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutcome {
    Created,
    Updated,
    Unchanged,
}

impl SyncOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            SyncOutcome::Created => "created",
            SyncOutcome::Updated => "updated",
            SyncOutcome::Unchanged => "unchanged",
        }
    }
}

/// What `skill install` did to the skill file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallOutcome {
    Installed,
    Updated,
    Unchanged,
}

impl InstallOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            InstallOutcome::Installed => "installed",
            InstallOutcome::Updated => "updated",
            InstallOutcome::Unchanged => "unchanged",
        }
    }
}

/// The generated block, markers included, without a trailing newline. It holds no project data.
pub fn claude_block() -> String {
    format!(
        "{BEGIN_MARKER}\n\
## katalogr\n\
\n\
katalogr answers where code and docs are, who uses them, why they exist and what a change touches, from a prebuilt graph of this project.\n\
\n\
Reach for these before reading whole files:\n\
\n\
- `katalogr find <name>`: symbols whose name matches\n\
- `katalogr explain <symbol>`: signature, doc, links and rationale\n\
- `katalogr show <symbol>`: just that symbol's lines\n\
- `katalogr refs <symbol>`: what references it\n\
- `katalogr why <file>:<line>`: why that line exists\n\
- `katalogr doctor`: check the index and print fixes\n\
\n\
The katalogr skill has the query craft and playbooks. Install it with `katalogr skill install`. Regenerate this block with `katalogr claude sync`.\n\
{END_MARKER}"
    )
}

/// The text of `existing` with the block refreshed or appended, or an error naming the problem.
fn merged(existing: &str, block: &str, path: &Path) -> Result<String, DistError> {
    let bad = |msg: &str| DistError::Io {
        path: path.display().to_string(),
        message: msg.to_string(),
    };
    let begins = existing.matches(BEGIN_MARKER).count();
    let ends = existing.matches(END_MARKER).count();
    match (begins, ends) {
        (0, 0) => {
            let mut out = existing.to_string();
            if !out.is_empty() {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push('\n');
            }
            out.push_str(block);
            out.push('\n');
            Ok(out)
        }
        (1, 1) => {
            let b = existing.find(BEGIN_MARKER).expect("counted");
            let e = existing.find(END_MARKER).expect("counted");
            if e < b {
                return Err(bad(
                    "the katalogr end marker comes before the begin marker; fix it by hand",
                ));
            }
            let e_end = e + END_MARKER.len();
            Ok(format!("{}{block}{}", &existing[..b], &existing[e_end..]))
        }
        (0, _) | (_, 0) => Err(bad(
            "found only one of the katalogr begin/end markers; add the missing one or remove the other, then rerun",
        )),
        _ => Err(bad(
            "found more than one katalogr begin/end marker; leave exactly one pair, then rerun",
        )),
    }
}

/// Write `contents` to `path` via a temp file in the same directory and a rename.
fn write_atomic(path: &Path, contents: &str) -> Result<(), DistError> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| DistError::io(dir.display(), e))?;
    let mut t =
        tempfile::NamedTempFile::new_in(dir).map_err(|e| DistError::io(dir.display(), e))?;
    t.write_all(contents.as_bytes())
        .map_err(|e| DistError::io(path.display(), e))?;
    t.persist(path)
        .map_err(|e| DistError::io(path.display(), e.error))?;
    Ok(())
}

/// Create or refresh the katalogr block in `<root>/CLAUDE.md`. Text outside the markers is never
/// changed; a lone marker is refused. With `show`, nothing is written. Returns the outcome and
/// the file's path.
pub fn claude_sync(root: &Path, show: bool) -> Result<(SyncOutcome, PathBuf), DistError> {
    let path = root.join("CLAUDE.md");
    let block = claude_block();
    let (existing, existed) = match std::fs::read_to_string(&path) {
        Ok(s) => (s, true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), false),
        Err(e) => return Err(DistError::io(path.display(), e)),
    };
    let next = merged(&existing, &block, &path)?;
    let outcome = if !existed {
        SyncOutcome::Created
    } else if next == existing {
        SyncOutcome::Unchanged
    } else {
        SyncOutcome::Updated
    };
    if !show && outcome != SyncOutcome::Unchanged {
        write_atomic(&path, &next)?;
    }
    Ok((outcome, path))
}

/// `<home>/.claude/skills/katalogr/SKILL.md` for the `HOME` environment variable.
pub fn user_skill_path() -> Result<PathBuf, DistError> {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .ok_or_else(|| DistError::usage("HOME is not set: cannot find ~/.claude"))?;
    Ok(skill_path(Path::new(&home)))
}

/// `<base>/.claude/skills/katalogr/SKILL.md`.
pub fn skill_path(base: &Path) -> PathBuf {
    base.join(".claude/skills/katalogr/SKILL.md")
}

/// Write the embedded skill to `target`, only when the content differs.
pub fn skill_install(target: &Path) -> Result<InstallOutcome, DistError> {
    let outcome = match std::fs::read_to_string(target) {
        Ok(s) if s == SKILL_MD => return Ok(InstallOutcome::Unchanged),
        Ok(_) => InstallOutcome::Updated,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => InstallOutcome::Installed,
        Err(e) => return Err(DistError::io(target.display(), e)),
    };
    write_atomic(target, SKILL_MD)?;
    Ok(outcome)
}

impl Katalogr {
    /// The root of the current repository on this machine (the nearest `.katalogr` marker).
    pub fn repo_root(&self) -> Result<&camino::Utf8Path, DistError> {
        Ok(self.current()?.1)
    }

    /// The session budget: the project's `claude.session_budget` setting, else the default.
    pub fn session_budget(&self) -> u64 {
        self.project
            .config()
            .settings
            .get(SESSION_BUDGET_KEY)
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_SESSION_BUDGET)
    }

    /// The compact project map, or `None` when there is no index yet.
    fn session_map(&self, budget: u64, timeout: f64) -> Result<Option<String>, DistError> {
        let opts = Options {
            consistency: Some(Consistency::StaleOk),
            timeout: Some(timeout),
        };
        let reply = self.query(&opts, |tx| Ok(Outcome::Found(katalogr_query::map(tx)?)))?;
        let Outcome::Found(entries) = reply.outcome else {
            return Ok(None);
        };
        if entries.is_empty() {
            return Ok(None);
        }
        let name = self.project.root().file_name().unwrap_or("");
        let repos: Vec<&str> = self
            .project
            .config()
            .repos
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        let header = format!(
            "katalogr: project {name} (repos: {}); index watermark {}, {} pending{}. Top symbols by PageRank:",
            repos.join(", "),
            reply.report.watermark,
            reply.report.pending,
            if reply.report.stale { ", stale" } else { "" },
        );
        let footer = "Ask the graph before reading files: use the katalogr skill (find, search, explain, show, refs, why).";
        let mut lines = vec![header];
        let mut tail = vec![footer.to_string()];
        // Header and footer first; symbols fill what remains. If even those do not fit, the
        // footer is dropped, then the header is cut to the budget.
        let fixed = |lines: &[String], tail: &[String]| {
            estimate_tokens(&format!("{}\n{}\n", lines.join("\n"), tail.join("\n")))
        };
        if fixed(&lines, &tail) > budget {
            tail.clear();
        }
        for e in &entries {
            let line = format!(
                "{}  {}  {}:{}",
                e.symbol.kind, e.symbol.name, e.symbol.path, e.symbol.span.start_line
            );
            let mut trial = lines.clone();
            trial.push(line.clone());
            if estimate_tokens(&format!(
                "{}\n{}",
                trial.join("\n"),
                tail.iter().map(|t| format!("{t}\n")).collect::<String>()
            )) > budget
            {
                break;
            }
            lines.push(line);
        }
        lines.extend(tail);
        let mut text = lines.join("\n");
        text.push('\n');
        // A budget below one line: cut the text to the budget.
        let max_chars = usize::try_from(budget.saturating_mul(4)).unwrap_or(usize::MAX);
        if text.chars().count() > max_chars {
            text = text.chars().take(max_chars).collect();
        }
        Ok(Some(text))
    }
}

/// The SessionStart map for the project at `start_dir`. Never fails: with no project, no index
/// or any error it returns a single line naming the setup command.
pub fn session_start(
    start_dir: &Path,
    overrides: &Overrides,
    budget: Option<u64>,
    timeout: Option<f64>,
) -> String {
    let none = |why: &str| {
        format!(
            "katalogr: {why}; set up with `katalogr project init <name> --repo <repo>=<path> --mark` (after `katalogr kindex init <dir>`), then run `katalogr index --full`.\n"
        )
    };
    let Ok(k) = Katalogr::resolve(start_dir, overrides) else {
        return none("no project found for this directory");
    };
    let budget = budget.unwrap_or_else(|| k.session_budget());
    let timeout = timeout
        .filter(|t| Duration::try_from_secs_f64(*t).is_ok())
        .unwrap_or(SESSION_TIMEOUT_SECS);
    match k.session_map(budget, timeout) {
        Ok(Some(text)) => text,
        Ok(None) => none("the project has no index yet"),
        Err(_) => none("the project index could not be read"),
    }
}
