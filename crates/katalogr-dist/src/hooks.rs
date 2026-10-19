//! Claude Code hooks and git hooks (FEAT-026).
//!
//! `pre_read` blocks a whole-file read of a large, freshly indexed file; `post_search` adds
//! symbol briefs to Grep and Glob results; `notify_from_git` queues what a git event changed;
//! `install`, `uninstall` and `status` manage the settings entries and git hook scripts.
//!
//! Every hook path fails open: any error, panic or timeout gives no output (the tool call goes
//! on). Details go to `<project>/.cache/hooks.log` (paths and decisions only; rotated at 1 MB,
//! one old file kept).

use crate::error::DistError;
use crate::{Katalogr, Overrides};
use katalogr_engine::scan::{PathChange, changed_paths};
use katalogr_engine::{
    Consistency as EngineConsistency, EventKind, NewEvent, QueryOptions, Scope as EngineScope,
};
use katalogr_model::{ArtifactName, Name, RelPath};
use katalogr_query::show::blob_of;
use katalogr_query::{Explain, SymbolHit};
use katalogr_store::blob::git_blob_id;
use katalogr_store::{Reader, StoreError, SymbolRow};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Whole-file reads of files larger than this many bytes are blocked (provisional; FEAT-036).
pub const DEFAULT_READ_BLOCK_BYTES: u64 = 32768;
/// Milliseconds a hook may take before it fails open.
pub const DEFAULT_TIMEOUT_MS: u64 = 800;
const LOG_ROTATE_BYTES: u64 = 1_000_000;
const MAX_REASON_SYMBOLS: usize = 10;
const MAX_BRIEFS: usize = 3;
const MAX_IDENTS: usize = 8;

/// The marker that tells `uninstall` which lines of a git hook are katalogr's.
const MARKER: &str = "# katalogr hook";
const GIT_HOOKS: [&str; 3] = ["post-commit", "post-checkout", "post-merge"];

/// The Claude Code settings entries: `(event, matcher, command)`.
const SETTINGS_ENTRIES: [(&str, Option<&str>, &str); 3] = [
    ("PreToolUse", Some("Read"), "katalogr hook pre-read"),
    (
        "PostToolUse",
        Some("Grep|Glob"),
        "katalogr hook post-search",
    ),
    ("SessionStart", None, "katalogr session-start"),
];

/// Which Claude Code hook to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKind {
    PreRead,
    PostSearch,
}

impl HookKind {
    fn name(self) -> &'static str {
        match self {
            HookKind::PreRead => "pre-read",
            HookKind::PostSearch => "post-search",
        }
    }
}

struct Config {
    read_block_bytes: u64,
    timeout: Duration,
}

impl Config {
    fn of(k: &Katalogr) -> Config {
        let get = |key: &str, default: u64| {
            k.project
                .config()
                .settings
                .get(key)
                .and_then(serde_yaml_ng::Value::as_u64)
                .unwrap_or(default)
        };
        Config {
            read_block_bytes: get("hooks.read_block_bytes", DEFAULT_READ_BLOCK_BYTES),
            timeout: Duration::from_millis(get("hooks.timeout_ms", DEFAULT_TIMEOUT_MS)),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Log

fn log(cache_dir: &Path, line: &str) {
    let _ = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(cache_dir)?;
        let file = cache_dir.join("hooks.log");
        if std::fs::metadata(&file).is_ok_and(|m| m.len() >= LOG_ROTATE_BYTES) {
            std::fs::rename(&file, cache_dir.join("hooks.log.1"))?;
        }
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)?;
        writeln!(f, "{secs} {line}")
    })();
}

// ---------------------------------------------------------------------------------------------
// Claude Code hooks

/// Run a Claude Code hook on its stdin JSON. Returns the stdout JSON to print, if any.
/// Never fails: any error, panic or timeout gives `None`.
pub fn run_hook(
    kind: HookKind,
    input: &str,
    fallback_cwd: &Path,
    overrides: &Overrides,
) -> Option<String> {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        run_inner(kind, input, fallback_cwd, overrides)
    }));
    result.ok().and_then(Result::ok).flatten()
}

fn run_inner(
    kind: HookKind,
    input: &str,
    fallback_cwd: &Path,
    overrides: &Overrides,
) -> Result<Option<String>, DistError> {
    let v: Value = serde_json::from_str(input).map_err(|e| DistError::usage(e.to_string()))?;
    let cwd = v
        .get("cwd")
        .and_then(Value::as_str)
        .map_or_else(|| fallback_cwd.to_path_buf(), PathBuf::from);
    let k = Katalogr::resolve(&cwd, overrides)?;
    let cfg = Config::of(&k);
    let cache = k.project.cache_dir().into_std_path_buf();
    let timeout = cfg.timeout;
    let deadline = Instant::now() + timeout;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = std::panic::catch_unwind(AssertUnwindSafe(|| match kind {
            HookKind::PreRead => pre_read(&k, &v, &cwd, &cfg, deadline),
            HookKind::PostSearch => post_search(&k, &v, &cfg, deadline),
        }));
        let _ = tx.send(r.unwrap_or_else(|_| Err(DistError::usage("hook panicked"))));
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(Decision { output, note })) => {
            log(&cache, &format!("{} {note}", kind.name()));
            Ok(output)
        }
        Ok(Err(e)) => {
            log(&cache, &format!("{} error: {e}", kind.name()));
            Ok(None)
        }
        Err(_) => {
            log(&cache, &format!("{} timeout", kind.name()));
            Ok(None)
        }
    }
}

/// What a hook decided: stdout to print (if any) and a log note (decision and path only).
struct Decision {
    output: Option<String>,
    note: String,
}

fn allow(note: impl Into<String>) -> Result<Decision, DistError> {
    Ok(Decision {
        output: None,
        note: note.into(),
    })
}

impl Katalogr {
    /// The repo whose root contains `file`, with the root-relative path.
    fn locate(&self, file: &Path) -> Option<(String, RelPath, PathBuf)> {
        let canon = std::fs::canonicalize(file).ok()?;
        let mut best: Option<(String, RelPath, PathBuf, usize)> = None;
        for (name, root) in &self.roots {
            let Ok(root) = std::fs::canonicalize(root) else {
                continue;
            };
            let Ok(rel) = canon.strip_prefix(&root) else {
                continue;
            };
            let Some(rel) = rel.to_str().and_then(|s| RelPath::new(s).ok()) else {
                continue;
            };
            let depth = root.components().count();
            if best.as_ref().is_none_or(|b| depth > b.3) {
                best = Some((name.clone(), rel, canon.clone(), depth));
            }
        }
        best.map(|(n, r, c, _)| (n, r, c))
    }
}

struct FileFacts {
    revision: String,
    symbols: Vec<(SymbolRow, f64)>,
}

fn pre_read(
    k: &Katalogr,
    v: &Value,
    cwd: &Path,
    cfg: &Config,
    deadline: Instant,
) -> Result<Decision, DistError> {
    if v.get("tool_name").and_then(Value::as_str) != Some("Read") {
        return allow("not a Read");
    }
    let input = v.get("tool_input").unwrap_or(&Value::Null);
    let Some(file) = input.get("file_path").and_then(Value::as_str) else {
        return allow("no file_path");
    };
    if ["offset", "limit"]
        .iter()
        .any(|f| input.get(f).is_some_and(|x| !x.is_null()))
    {
        return allow(format!("allow {file}: ranged read"));
    }
    let abs = cwd.join(file);
    let Some((repo, rel, abs)) = k.locate(&abs) else {
        return allow(format!("allow {file}: not in an indexed repo"));
    };
    let size = std::fs::metadata(&abs)
        .map_err(|e| DistError::io(abs.display(), e))?
        .len();
    if size <= cfg.read_block_bytes {
        return allow(format!("allow {rel}: {size} bytes is small"));
    }
    let bytes = std::fs::read(&abs).map_err(|e| DistError::io(abs.display(), e))?;
    let blob = git_blob_id(&bytes);
    let engine = k.engine(deadline)?;
    let artifact = ArtifactName {
        repo: repo.clone(),
        path: rel.clone(),
    };
    let scope = EngineScope::Paths(BTreeSet::from([(repo.clone(), rel.clone())]));
    let answer = engine.with_consistency(
        &QueryOptions {
            consistency: EngineConsistency::StaleOk,
            timeout: cfg.timeout,
            scope,
        },
        |tx| -> Result<Option<FileFacts>, StoreError> {
            if tx.shard_hash(&repo, &rel)?.is_none() {
                return Ok(None);
            }
            let Some(row) = tx.artifact(artifact.id())? else {
                return Ok(None);
            };
            let ranks: BTreeMap<String, f64> = katalogr_query::rank(tx)?
                .into_iter()
                .map(|(s, r)| (s.name, r))
                .collect();
            let mut symbols = Vec::new();
            for id in &row.symbols {
                if let Some(s) = tx.symbol(*id)? {
                    let r = ranks.get(&s.name.canonical()).copied().unwrap_or(0.0);
                    symbols.push((s, r));
                }
            }
            Ok(Some(FileFacts {
                revision: row.provenance.revision,
                symbols,
            }))
        },
    )?;
    if answer.pending > 0 {
        return allow(format!("allow {rel}: change pending"));
    }
    let Some(mut facts) = answer.value? else {
        return allow(format!("allow {rel}: no shard"));
    };
    if blob != blob_of(&facts.revision) {
        return allow(format!("allow {rel}: index is stale"));
    }
    facts.symbols.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| a.0.name.canonical().cmp(&b.0.name.canonical()))
    });
    let mut reason = format!(
        "katalogr: {rel} is {size} bytes and indexed (fresh). Read the symbols you need instead of the whole file"
    );
    if facts.symbols.is_empty() {
        reason.push('.');
    } else {
        reason.push_str(", top symbols by rank:");
    }
    for (s, _) in facts.symbols.iter().take(MAX_REASON_SYMBOLS) {
        let name = s.name.canonical();
        reason.push_str(&format!(
            "\n- {} ({}, lines {}-{}): `katalogr show {name}` or `katalogr explain {name}`",
            plain_name(s),
            s.kind,
            s.span.start_line,
            s.span.end_line
        ));
    }
    reason.push_str("\nOr read with offset/limit to fetch a line range.");
    Ok(Decision {
        output: Some(
            json!({"hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }})
            .to_string(),
        ),
        note: format!("deny {rel}"),
    })
}

fn plain_name(row: &SymbolRow) -> &str {
    row.name.segments.last().map_or("", |s| s.name.as_str())
}

fn symbol_hit(row: &SymbolRow) -> SymbolHit {
    SymbolHit {
        name: row.name.canonical(),
        kind: row.kind.clone(),
        path: row.name.artifact.path.as_str().to_string(),
        span: row.span,
    }
}

/// Identifier-like pieces of a search pattern, in order, without repeats. The pattern is split
/// at regex metacharacters and whitespace; a backslash escape (`\b`, `\w`, `\.`) is a separator.
fn identifiers(pattern: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        let ok = cur.len() >= 3
            && cur
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
        if ok && !out.contains(cur) {
            out.push(cur.clone());
        }
        cur.clear();
    };
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            flush(&mut cur, &mut out);
            chars.next();
        } else if c.is_ascii_alphanumeric() || c == '_' {
            cur.push(c);
        } else {
            flush(&mut cur, &mut out);
        }
    }
    flush(&mut cur, &mut out);
    out.truncate(MAX_IDENTS);
    out
}

fn brief(e: &Explain) -> String {
    let s = &e.symbol;
    let mut lines = vec![format!(
        "{} ({}) {}:{}-{}",
        s.name, s.kind, s.path, s.span.start_line, s.span.end_line
    )];
    if let Some(sig) = &e.signature {
        lines.push(format!("  signature: {sig}"));
    }
    if let Some(doc) = &e.doc {
        lines.push(format!("  doc: {}", doc.lines().next().unwrap_or("")));
    }
    lines.push(format!(
        "  links: {} outgoing, {} incoming",
        e.outgoing_total, e.incoming_total
    ));
    for l in e.outgoing.iter().chain(&e.incoming).take(3) {
        lines.push(format!("    {} {}", l.kind, l.other));
    }
    if !e.items.is_empty() {
        lines.push(format!("  rationale: {}", e.items.join(", ")));
    }
    lines.push(format!(
        "  next: `katalogr show {0}` | `katalogr explain {0}`",
        s.name
    ));
    lines.join("\n")
}

fn post_search(
    k: &Katalogr,
    v: &Value,
    cfg: &Config,
    deadline: Instant,
) -> Result<Decision, DistError> {
    let tool = v.get("tool_name").and_then(Value::as_str);
    if !matches!(tool, Some("Grep" | "Glob")) {
        return allow("not a search");
    }
    let pattern = v
        .pointer("/tool_input/pattern")
        .and_then(Value::as_str)
        .unwrap_or("");
    let idents = identifiers(pattern);
    if idents.is_empty() {
        return allow("search: no identifiers");
    }
    let engine = k.engine(deadline)?;
    let answer = engine.with_consistency(
        &QueryOptions {
            consistency: EngineConsistency::StaleOk,
            timeout: cfg.timeout,
            scope: EngineScope::Project,
        },
        |tx| -> Result<(Vec<String>, Vec<String>), StoreError> {
            let symbols = tx.symbols()?;
            let (mut briefs, mut ambiguous) = (Vec::new(), Vec::new());
            for ident in &idents {
                let mut found: Vec<&SymbolRow> = symbols
                    .iter()
                    .map(|(_, r)| r)
                    .filter(|r| plain_name(r) == ident)
                    .collect();
                found.sort_by_key(|r| r.name.canonical());
                match found.len() {
                    0 => {}
                    1 => {
                        if briefs.len() < MAX_BRIEFS {
                            let e = katalogr_query::explain(tx, &symbol_hit(found[0]), 5)?;
                            briefs.push(brief(&e));
                        }
                    }
                    n => ambiguous.push(format!(
                        "{ident}: {n} symbols have this name (`katalogr find {ident}`)"
                    )),
                }
            }
            Ok((briefs, ambiguous))
        },
    )?;
    let (briefs, ambiguous) = answer.value?;
    if briefs.is_empty() && ambiguous.is_empty() {
        return allow("search: no indexed symbol");
    }
    let mut text = String::from("katalogr index facts for names in this search:\n");
    text.push_str(&briefs.join("\n"));
    for a in &ambiguous {
        text.push_str(&format!("\n{a}"));
    }
    Ok(Decision {
        output: Some(
            json!({"hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "additionalContext": text.trim_end(),
            }})
            .to_string(),
        ),
        note: format!(
            "enrich {} brief(s), {} ambiguous",
            briefs.len(),
            ambiguous.len()
        ),
    })
}

// ---------------------------------------------------------------------------------------------
// Git events

impl Katalogr {
    /// Queue the paths a git hook event changed. `hook` is `post-commit`, `post-checkout` or
    /// `post-merge`; `args` are the hook's own arguments. Returns the number of events queued.
    /// Only appends to the inbox.
    pub fn notify_from_git(&self, hook: &str, args: &[String]) -> Result<usize, DistError> {
        let (repo, root) = self.current()?;
        let range = match hook {
            "post-commit" => {
                if changed_paths(root.as_std_path(), "HEAD~1..HEAD").is_ok() {
                    Some("HEAD~1..HEAD".to_string())
                } else {
                    None // first commit: the whole tree
                }
            }
            "post-checkout" => {
                // <old> <new> <flag>; only branch checkouts (flag 1) change many files.
                let [old, new, flag] = args else {
                    return Err(DistError::usage("post-checkout needs <old> <new> <flag>"));
                };
                if flag != "1" {
                    return Ok(0);
                }
                if old.bytes().all(|b| b == b'0') {
                    None
                } else {
                    Some(format!("{old}..{new}"))
                }
            }
            "post-merge" => Some("ORIG_HEAD..HEAD".to_string()),
            other => return Err(DistError::usage(format!("unknown git hook {other:?}"))),
        };
        let mut events = Vec::new();
        match range {
            None => events.push(NewEvent::rescan(repo)),
            Some(range) => {
                for change in changed_paths(root.as_std_path(), &range)? {
                    match change {
                        PathChange::Added(p) | PathChange::Modified(p) => {
                            events.push(NewEvent::file(repo, &p, EventKind::Changed));
                        }
                        PathChange::Deleted(p) => {
                            events.push(NewEvent::file(repo, &p, EventKind::Deleted));
                        }
                        PathChange::Renamed { from, to } => {
                            events.push(NewEvent::file(repo, &from, EventKind::Deleted));
                            events.push(NewEvent::file(repo, &to, EventKind::Changed));
                        }
                    }
                }
            }
        }
        let inbox = katalogr_engine::queue::Inbox::new(&self.project);
        for ev in &events {
            inbox.append(ev)?;
        }
        Ok(events.len())
    }
}

/// `notify --from-git`: queue the event's paths, never fail, never block git. Errors go to the
/// hooks log.
pub fn notify_from_git(start: &Path, overrides: &Overrides, hook: &str, args: &[String]) -> usize {
    let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let k = Katalogr::resolve(start, overrides)?;
        let cache = k.project.cache_dir().into_std_path_buf();
        match k.notify_from_git(hook, args) {
            Ok(n) => {
                log(&cache, &format!("git {hook} queued {n}"));
                Ok(n)
            }
            Err(e) => {
                log(&cache, &format!("git {hook} error: {e}"));
                Err(e)
            }
        }
    }));
    r.ok().and_then(Result::ok).unwrap_or(0)
}

// ---------------------------------------------------------------------------------------------
// Install, uninstall, status

/// Where the Claude settings live: `<repo>/.claude/settings.json` or `~/.claude/settings.json`.
fn settings_path(k: Option<&Katalogr>, global: bool) -> Result<PathBuf, DistError> {
    if global {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .ok_or_else(|| DistError::usage("HOME is not set"))?;
        Ok(PathBuf::from(home).join(".claude/settings.json"))
    } else {
        let k = k.ok_or_else(|| {
            DistError::usage(
                "not inside a marked repository: run from a directory with a .katalogr marker",
            )
        })?;
        let (_, root) = k.current()?;
        Ok(root.as_std_path().join(".claude/settings.json"))
    }
}

fn read_settings(path: &Path) -> Result<Value, DistError> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(json!({})),
        Ok(s) => serde_json::from_str(&s)
            .map_err(|e| DistError::usage(format!("{}: not valid JSON: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(DistError::io(path.display(), e)),
    }
}

fn write_settings(path: &Path, v: &Value) -> Result<(), DistError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let io = |e| DistError::io(path.display(), e);
    std::fs::create_dir_all(dir).map_err(io)?;
    let mut t = tempfile::NamedTempFile::new_in(dir).map_err(io)?;
    let text = serde_json::to_string_pretty(v).expect("serialise settings");
    t.write_all(format!("{text}\n").as_bytes()).map_err(io)?;
    t.persist(path).map_err(|e| io(e.error))?;
    Ok(())
}

fn matcher_of(entry: &Value) -> Option<&str> {
    entry.get("matcher").and_then(Value::as_str)
}

fn entry_has_command(entry: &Value, command: &str) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hs| {
            hs.iter()
                .any(|h| h.get("command").and_then(Value::as_str) == Some(command))
        })
}

/// Add the entry unless a hook with the same command exists under that event and matcher.
/// Returns true when something was added.
fn merge_entry(
    settings: &mut Value,
    event: &str,
    matcher: Option<&str>,
    command: &str,
) -> Result<bool, DistError> {
    let bad = || DistError::usage("settings.json: unexpected shape under \"hooks\"");
    let root = settings.as_object_mut().ok_or_else(bad)?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(bad)?;
    let list = hooks
        .entry(event)
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(bad)?;
    if list
        .iter()
        .any(|e| matcher_of(e) == matcher && entry_has_command(e, command))
    {
        return Ok(false);
    }
    let hook = json!({"type": "command", "command": command});
    if let Some(e) = list.iter_mut().find(|e| matcher_of(e) == matcher)
        && let Some(hs) = e.get_mut("hooks").and_then(Value::as_array_mut)
    {
        hs.push(hook);
        return Ok(true);
    }
    let mut entry = json!({"hooks": [hook]});
    if let Some(m) = matcher {
        entry["matcher"] = json!(m);
    }
    list.push(entry);
    Ok(true)
}

/// Remove katalogr's commands from the settings; drops entries and events left empty.
/// Returns how many hooks were removed.
fn remove_entries(settings: &mut Value) -> usize {
    let mut removed = 0;
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return 0;
    };
    for (event, _, command) in SETTINGS_ENTRIES {
        let Some(list) = hooks.get_mut(event).and_then(Value::as_array_mut) else {
            continue;
        };
        for e in list.iter_mut() {
            if let Some(hs) = e.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = hs.len();
                hs.retain(|h| h.get("command").and_then(Value::as_str) != Some(command));
                removed += before - hs.len();
            }
        }
        list.retain(|e| {
            e.get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|hs| !hs.is_empty())
        });
        if list.is_empty() {
            hooks.remove(event);
        }
    }
    if hooks.is_empty()
        && let Some(root) = settings.as_object_mut()
    {
        root.remove("hooks");
    }
    removed
}

fn git_hooks_dir(root: &Path) -> Result<PathBuf, DistError> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--git-path", "hooks"])
        .output()
        .map_err(|e| DistError::io(root.display(), e))?;
    if !out.status.success() {
        return Err(DistError::usage(format!(
            "{}: not a git repository",
            root.display()
        )));
    }
    let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    Ok(if p.is_absolute() { p } else { root.join(p) })
}

fn is_sh_shebang(first_line: &str) -> bool {
    let l = first_line.trim();
    l.starts_with("#!")
        && ["sh", "bash", "dash", "zsh", "ksh"]
            .iter()
            .any(|s| l.ends_with(&format!("/{s}")) || l.ends_with(&format!(" {s}")))
}

fn katalogr_git_lines(hook: &str) -> String {
    format!("{MARKER}\nkatalogr notify --from-git {hook} \"$@\" || true\n")
}

/// Install one git hook script: create it, or append katalogr's lines to an existing sh script
/// (the existing commands run first). Returns a note.
fn install_git_hook(dir: &Path, hook: &str) -> Result<String, DistError> {
    let path = dir.join(hook);
    let io = |e| DistError::io(path.display(), e);
    std::fs::create_dir_all(dir).map_err(io)?;
    let existing = match std::fs::read_to_string(&path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(io(e)),
    };
    let text = match existing {
        None => format!("#!/bin/sh\n{}", katalogr_git_lines(hook)),
        Some(s) if s.contains(MARKER) => return Ok(format!("{hook}: already installed")),
        Some(s) => {
            let first = s.lines().next().unwrap_or("");
            if !is_sh_shebang(first) {
                return Ok(format!(
                    "{hook}: existing hook is not a shell script; left untouched, not chained"
                ));
            }
            let sep = if s.ends_with('\n') { "" } else { "\n" };
            format!("{s}{sep}{}", katalogr_git_lines(hook))
        }
    };
    std::fs::write(&path, text).map_err(io)?;
    set_executable(&path)?;
    Ok(format!("{hook}: installed"))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), DistError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(path)
        .map_err(|e| DistError::io(path.display(), e))?
        .permissions();
    perm.set_mode(perm.mode() | 0o755);
    std::fs::set_permissions(path, perm).map_err(|e| DistError::io(path.display(), e))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), DistError> {
    Ok(())
}

/// Remove only katalogr's lines from a git hook; delete the file when nothing else is left.
fn uninstall_git_hook(dir: &Path, hook: &str) -> Result<String, DistError> {
    let path = dir.join(hook);
    let Ok(s) = std::fs::read_to_string(&path) else {
        return Ok(format!("{hook}: not installed"));
    };
    if !s.contains(MARKER) {
        return Ok(format!("{hook}: not installed"));
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut lines = s.lines();
    while let Some(l) = lines.next() {
        if l.trim() == MARKER {
            lines.next(); // the command line that follows the marker
        } else {
            kept.push(l);
        }
    }
    let only_shebang = kept
        .iter()
        .all(|l| l.trim().is_empty() || l.starts_with("#!"));
    if only_shebang {
        std::fs::remove_file(&path).map_err(|e| DistError::io(path.display(), e))?;
        Ok(format!("{hook}: removed"))
    } else {
        std::fs::write(&path, format!("{}\n", kept.join("\n")))
            .map_err(|e| DistError::io(path.display(), e))?;
        Ok(format!("{hook}: katalogr lines removed, the rest kept"))
    }
}

/// The marked repo of `start`, if any (errors count as none).
fn try_resolve(start: &Path, overrides: &Overrides) -> Option<Katalogr> {
    Katalogr::resolve(start, overrides).ok()
}

fn repo_root_of(k: &Katalogr) -> Option<PathBuf> {
    k.current().ok().map(|(_, r)| r.as_std_path().to_path_buf())
}

/// Install the Claude settings entries (project scope, or `~/.claude/settings.json` with
/// `global`) and, inside a marked git repository, the three git hooks. Idempotent. Returns one
/// report line per action.
pub fn install(
    start: &Path,
    overrides: &Overrides,
    global: bool,
) -> Result<Vec<String>, DistError> {
    let k = try_resolve(start, overrides);
    let path = settings_path(k.as_ref(), global)?;
    let mut settings = read_settings(&path)?;
    let mut lines = Vec::new();
    for (event, matcher, command) in SETTINGS_ENTRIES {
        let added = merge_entry(&mut settings, event, matcher, command)?;
        lines.push(format!(
            "{}: {event} {command}: {}",
            path.display(),
            if added { "added" } else { "already present" }
        ));
    }
    write_settings(&path, &settings)?;
    if let Some(root) = k.as_ref().and_then(repo_root_of) {
        let dir = git_hooks_dir(&root)?;
        for hook in GIT_HOOKS {
            lines.push(install_git_hook(&dir, hook)?);
        }
    }
    Ok(lines)
}

/// Remove katalogr's settings entries and git hook lines; nothing else is touched.
pub fn uninstall(
    start: &Path,
    overrides: &Overrides,
    global: bool,
) -> Result<Vec<String>, DistError> {
    let k = try_resolve(start, overrides);
    let path = settings_path(k.as_ref(), global)?;
    let mut lines = Vec::new();
    if path.exists() {
        let mut settings = read_settings(&path)?;
        let n = remove_entries(&mut settings);
        if n > 0 {
            write_settings(&path, &settings)?;
        }
        lines.push(format!("{}: removed {n} hook(s)", path.display()));
    } else {
        lines.push(format!("{}: no settings file", path.display()));
    }
    if let Some(root) = k.as_ref().and_then(repo_root_of) {
        let dir = git_hooks_dir(&root)?;
        for hook in GIT_HOOKS {
            lines.push(uninstall_git_hook(&dir, hook)?);
        }
    }
    Ok(lines)
}

/// Where `katalogr` resolves on PATH, and whether that is the running binary.
fn binary_state() -> &'static str {
    let found = std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join("katalogr"))
            .find(|c| c.is_file())
    });
    let current = std::env::current_exe().ok();
    match (found, current) {
        (None, _) => "katalogr is not on PATH",
        (Some(f), Some(c)) => {
            if std::fs::canonicalize(f).ok() == std::fs::canonicalize(c).ok() {
                "points at the current binary"
            } else {
                "PATH has a different katalogr"
            }
        }
        (Some(_), None) => "current binary unknown",
    }
}

/// What is installed where, and whether each entry points at the current binary.
pub fn status(start: &Path, overrides: &Overrides) -> Result<Vec<String>, DistError> {
    let k = try_resolve(start, overrides);
    let mut lines = Vec::new();
    let mut scopes = vec![("project", settings_path(k.as_ref(), false).ok())];
    scopes.push(("global", settings_path(None, true).ok()));
    for (scope, path) in scopes {
        let Some(path) = path else {
            lines.push(format!("{scope}: no settings location"));
            continue;
        };
        let settings = read_settings(&path).unwrap_or_else(|_| json!({}));
        for (event, matcher, command) in SETTINGS_ENTRIES {
            let present = settings
                .pointer(&format!("/hooks/{event}"))
                .and_then(Value::as_array)
                .is_some_and(|l| {
                    l.iter()
                        .any(|e| matcher_of(e) == matcher && entry_has_command(e, command))
                });
            lines.push(format!(
                "{scope} {event} {command}: {}{}",
                if present { "installed" } else { "missing" },
                if present {
                    format!(" ({})", binary_state())
                } else {
                    String::new()
                }
            ));
        }
    }
    if let Some(root) = k.as_ref().and_then(repo_root_of)
        && let Ok(dir) = git_hooks_dir(&root)
    {
        for hook in GIT_HOOKS {
            let ours = std::fs::read_to_string(dir.join(hook)).is_ok_and(|s| s.contains(MARKER));
            lines.push(format!(
                "git {hook}: {}",
                if ours { "installed" } else { "missing" }
            ));
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::identifiers;

    #[test]
    fn identifiers_split_on_regex_syntax() {
        assert_eq!(
            identifiers(r"\bfn\s+parse_args\(.*FooBar[0-9]+"),
            ["parse_args", "FooBar"]
        );
        assert!(identifiers("**/*.ts").is_empty());
    }
}
