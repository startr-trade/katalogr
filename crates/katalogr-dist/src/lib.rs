//! Composition root: the Katalogr facade with extensions registered (ADR-0011).
//!
//! All behaviour of the command-line wrapper lives here; the wrapper parses, calls and renders.

mod error;
mod roots;
mod setup;
mod span;
#[cfg(feature = "test-extractor")]
mod test_extractor;

pub use error::DistError;
pub use katalogr_query as query;
use katalogr_query::{
    CallerHit, Explain, MapEntry, PathStep, RefHit, Resolution, SearchHit, SymbolHit, WhyResult,
};
pub use katalogr_query::{ReplaceResult, ShowResult};
pub use setup::{ProjectCreated, kindex_init, project_init};

use camino::{Utf8Path, Utf8PathBuf};
use katalogr_engine::scan::{PathChange, changed_paths};
use katalogr_engine::{
    Consistency as EngineConsistency, Engine, EventKind, NewEvent, QueryOptions, Scope,
    registry::ExtensionRegistry,
};
use katalogr_model::RelPath;
use katalogr_store::marker::{self, Overrides as StoreOverrides};
use katalogr_store::{Kindex, ProjectDir, ProjectName, ReadTx, StoreError};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

/// Seconds to wait for a fresh index when `--timeout` is not given (provisional).
pub const DEFAULT_TIMEOUT_SECS: f64 = 30.0;

/// Freshness requirement for a read; the wrapper's flag maps onto this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consistency {
    Wait,
    WaitAll,
    StaleOk,
}

impl From<Consistency> for EngineConsistency {
    fn from(c: Consistency) -> Self {
        match c {
            Consistency::Wait => EngineConsistency::Wait,
            Consistency::WaitAll => EngineConsistency::WaitAll,
            Consistency::StaleOk => EngineConsistency::StaleOk,
        }
    }
}

/// Flag and environment overrides for resolving the kindex and project.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub kindex: Option<String>,
    pub project: Option<String>,
}

/// Options for reads. `None` means the command's default.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub consistency: Option<Consistency>,
    /// Seconds; may be fractional.
    pub timeout: Option<f64>,
}

impl Options {
    fn timeout(&self) -> Result<Duration, DistError> {
        let s = self.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS);
        Duration::try_from_secs_f64(s)
            .map_err(|_| DistError::usage(format!("invalid --timeout {s}")))
    }
}

/// What a read saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub watermark: u64,
    pub pending: u64,
    pub stale: bool,
    /// True when the answer is stale although the caller asked to wait (the timeout elapsed).
    pub timed_out: bool,
    /// Events appended by this call.
    pub queued: usize,
    /// Non-fatal notices (repos skipped because their root is unknown).
    pub warnings: Vec<String>,
}

impl Report {
    /// `{watermark, pending, stale}`.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "watermark": self.watermark,
            "pending": self.pending,
            "stale": self.stale,
        })
    }
}

/// The katalogr facade for one resolved project.
pub struct Katalogr {
    project: ProjectDir,
    /// The repo of the nearest marker, if any.
    repo: Option<String>,
    /// Repo name to root on this machine: `roots.yaml`, overridden by the marker for its repo.
    roots: BTreeMap<String, Utf8PathBuf>,
}

/// Extension registration point. Register extractors here as they are written.
fn build_registry() -> ExtensionRegistry {
    let mut registry = ExtensionRegistry::new();
    registry.register(Box::new(katalogr_ext_rust::RustExtractor));
    registry.register(Box::new(katalogr_ext_ts::TsExtractor));
    #[cfg(feature = "test-extractor")]
    registry.register(Box::new(test_extractor::TestExtractor));
    registry
}

/// How a query ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome<T> {
    Found(T),
    /// The text named several symbols; these are the candidates.
    Ambiguous(Vec<SymbolHit>),
    /// Nothing matched; the message says what was looked for.
    NoMatch(String),
    /// The file on disk is not the file that was indexed; the message says what to do.
    StaleFile(String),
}

/// A query answer with what the read saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply<T> {
    pub outcome: Outcome<T>,
    pub report: Report,
}

fn found_or_none(items: Vec<SymbolHit>, text: &str) -> Outcome<Vec<SymbolHit>> {
    if items.is_empty() {
        Outcome::NoMatch(format!("no symbol matches {text:?}"))
    } else {
        Outcome::Found(items)
    }
}

/// Resolve `text` to one symbol and run `f` on it; several or no matches end the query.
pub(crate) fn on_target<T>(
    tx: &ReadTx,
    text: &str,
    f: impl FnOnce(&SymbolHit) -> Result<T, StoreError>,
) -> Result<Outcome<T>, StoreError> {
    Ok(match katalogr_query::resolve(tx, text)? {
        Resolution::One(t) => Outcome::Found(f(&t)?),
        Resolution::Ambiguous(c) => Outcome::Ambiguous(c),
        Resolution::None => Outcome::NoMatch(format!("no symbol matches {text:?}")),
    })
}

fn from_resolution<T>(r: Resolution<T>, what: &str) -> Outcome<T> {
    match r {
        Resolution::One(v) => Outcome::Found(v),
        Resolution::Ambiguous(c) => Outcome::Ambiguous(c),
        Resolution::None => Outcome::NoMatch(format!("no symbol matches {what:?}")),
    }
}

/// The outcome when `r` did not name exactly one symbol.
fn unresolved<T>(r: Resolution<SymbolHit>, what: &str) -> Outcome<T> {
    match r {
        Resolution::One(_) => unreachable!("callers handle the single match"),
        Resolution::Ambiguous(c) => Outcome::Ambiguous(c),
        Resolution::None => Outcome::NoMatch(format!("no symbol matches {what:?}")),
    }
}

impl Katalogr {
    /// Resolve the kindex and project from flags, environment and the nearest `.katalogr`
    /// marker found walking up from `start_dir`.
    pub fn resolve(start_dir: &Path, overrides: &Overrides) -> Result<Katalogr, DistError> {
        let start = setup::absolute(start_dir)?;
        let kindex = overrides
            .kindex
            .as_deref()
            .map(|k| setup::absolute(Path::new(k)))
            .transpose()?;
        let project = overrides
            .project
            .as_deref()
            .map(ProjectName::new)
            .transpose()?;
        let r = marker::resolve(&start, &StoreOverrides { kindex, project })?;
        let project = Kindex::open(&r.kindex)?.project(&r.project)?;
        let mut roots = roots::read(&project.cache_dir())?;
        if let (Some(repo), Some(root)) = (&r.repo, &r.repo_root) {
            roots.insert(repo.clone(), root.clone());
        }
        Ok(Katalogr {
            project,
            repo: r.repo,
            roots,
        })
    }

    pub fn project(&self) -> &ProjectDir {
        &self.project
    }

    /// Record (or replace) the machine-local root of a repo of this project.
    pub fn add_root(&self, name: &str, path: &Path) -> Result<(), DistError> {
        if !self.project.config().repos.iter().any(|r| r.name == name) {
            return Err(DistError::usage(format!(
                "repo {name} is not part of project {}",
                self.project.root().file_name().unwrap_or("")
            )));
        }
        let root = setup::absolute(path)?;
        if !root.is_dir() {
            return Err(DistError::usage(format!("{root} is not a directory")));
        }
        let root = Utf8PathBuf::from_path_buf(
            std::fs::canonicalize(&root).map_err(|e| DistError::io(&root, e))?,
        )
        .map_err(|p| DistError::usage(format!("{}: path is not UTF-8", p.display())))?;
        let cache = self.project.cache_dir();
        let mut stored = roots::read(&cache)?;
        stored.insert(name.to_string(), root);
        roots::write(&cache, &stored)
    }

    fn engine(&self, deadline: Instant) -> Result<Engine, DistError> {
        let mut engine = Engine::open(
            self.project.clone(),
            build_registry(),
            self.roots.clone(),
            deadline,
        )?;
        for repo in &self.project.config().repos {
            engine.set_repo_filters(&repo.name, repo.include.clone(), repo.exclude.clone());
        }
        Ok(engine)
    }

    fn current(&self) -> Result<(&str, &Utf8Path), DistError> {
        let repo = self.repo.as_deref().ok_or_else(|| {
            DistError::usage(
                "not inside a marked repository: run from a directory with a .katalogr marker",
            )
        })?;
        let root = self.roots.get(repo).ok_or_else(|| unknown_root(repo))?;
        Ok((repo, root))
    }

    /// Append one `Changed` (or `Deleted`) event per path to the inbox. Paths are relative to
    /// the current repo root (absolute paths inside it are accepted). Returns the number queued.
    pub fn notify(&self, paths: &[String], deleted: bool) -> Result<usize, DistError> {
        let (repo, root) = self.current()?;
        let kind = if deleted {
            EventKind::Deleted
        } else {
            EventKind::Changed
        };
        let rels = paths
            .iter()
            .map(|p| rel_to_root(root, p))
            .collect::<Result<Vec<_>, _>>()?;
        let inbox = katalogr_engine::queue::Inbox::new(&self.project);
        for rel in &rels {
            inbox.append(&NewEvent::file(repo, rel, kind))?;
        }
        Ok(rels.len())
    }

    /// Read the queue state without ingesting or processing (stale-ok).
    pub fn status(&self, opts: &Options) -> Result<Report, DistError> {
        let timeout = opts.timeout()?;
        let engine = self.engine(Instant::now() + timeout)?;
        self.read(&engine, Consistency::StaleOk, timeout, 0, Vec::new())
    }

    /// Queue what changed (a revision range, every repo with `full`, else a rescan of the current
    /// repo), then bring the index to the requested consistency (default: wait-all).
    pub fn index(
        &self,
        range: Option<String>,
        full: bool,
        opts: &Options,
    ) -> Result<Report, DistError> {
        let timeout = opts.timeout()?;
        let engine = self.engine(Instant::now() + timeout)?;
        let mut warnings = Vec::new();
        let mut events = Vec::new();
        if full {
            for r in &self.project.config().repos {
                if self.roots.contains_key(&r.name) {
                    events.push(NewEvent::rescan(&r.name));
                } else {
                    warnings.push(unknown_root(&r.name).to_string());
                }
            }
        } else if let Some(range) = range {
            let (repo, root) = self.current()?;
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
        } else {
            let (repo, _) = self.current()?;
            events.push(NewEvent::rescan(repo));
        }
        let inbox = engine.inbox();
        for ev in &events {
            inbox.append(ev)?;
        }
        let consistency = opts.consistency.unwrap_or(Consistency::WaitAll);
        self.read(&engine, consistency, timeout, events.len(), warnings)
    }

    /// Run `f` on one snapshot at the requested consistency (default: wait).
    pub(crate) fn query<T>(
        &self,
        opts: &Options,
        f: impl FnOnce(&ReadTx) -> Result<Outcome<T>, StoreError>,
    ) -> Result<Reply<T>, DistError> {
        let timeout = opts.timeout()?;
        let engine = self.engine(Instant::now() + timeout)?;
        let consistency = opts.consistency.unwrap_or(Consistency::Wait);
        let a = engine.with_consistency(
            &QueryOptions {
                consistency: consistency.into(),
                timeout,
                scope: Scope::Project,
            },
            f,
        )?;
        Ok(Reply {
            outcome: a.value?,
            report: Report {
                watermark: a.watermark.0,
                pending: a.pending,
                stale: a.stale,
                timed_out: a.stale && consistency != Consistency::StaleOk,
                queued: 0,
                warnings: Vec::new(),
            },
        })
    }

    /// Symbols whose name matches `text`.
    pub fn find(&self, text: &str, opts: &Options) -> Result<Reply<Vec<SymbolHit>>, DistError> {
        self.query(opts, |tx| {
            Ok(found_or_none(katalogr_query::find(tx, text)?, text))
        })
    }

    /// References to the symbol `text` names, through re-exports.
    pub fn refs(&self, text: &str, opts: &Options) -> Result<Reply<Vec<RefHit>>, DistError> {
        self.query(opts, |tx| {
            on_target(tx, text, |t| katalogr_query::refs(tx, t))
        })
    }

    /// Callers of the symbol `text` names, up to `depth` hops.
    pub fn callers(
        &self,
        text: &str,
        depth: u32,
        opts: &Options,
    ) -> Result<Reply<Vec<CallerHit>>, DistError> {
        self.query(opts, |tx| {
            on_target(tx, text, |t| katalogr_query::callers(tx, t, depth))
        })
    }

    /// The symbol at `file:line` of the current repo (`file` relative to the repo root) and its
    /// rationale links.
    pub fn why(&self, spec: &str, opts: &Options) -> Result<Reply<WhyResult>, DistError> {
        let (file, line) = spec
            .rsplit_once(':')
            .and_then(|(f, l)| Some((f, l.parse::<u32>().ok().filter(|n| *n >= 1)?)))
            .ok_or_else(|| DistError::usage(format!("{spec:?}: expected FILE:LINE")))?;
        let (repo, root) = self.current()?;
        let path = rel_to_root(root, file)?;
        self.query(opts, |tx| {
            Ok(from_resolution(
                katalogr_query::why(tx, repo, &path, line)?,
                spec,
            ))
        })
    }

    /// All symbols by PageRank.
    pub fn map(&self, opts: &Options) -> Result<Reply<Vec<MapEntry>>, DistError> {
        self.query(opts, |tx| Ok(Outcome::Found(katalogr_query::map(tx)?)))
    }

    /// The best `limit` symbols for the words in `terms` (BM25 over names, docs, sections and
    /// contract keys). No word is a usage error; no hit is no match.
    pub fn search(
        &self,
        terms: &str,
        limit: usize,
        opts: &Options,
    ) -> Result<Reply<Vec<SearchHit>>, DistError> {
        if katalogr_model::tokenize(terms).is_empty() {
            return Err(DistError::usage(format!(
                "{terms:?}: no searchable word (words need two letters, or a digit)"
            )));
        }
        self.query(opts, |tx| {
            let hits = katalogr_query::search(tx, terms, limit)?;
            Ok(if hits.is_empty() {
                Outcome::NoMatch(format!("nothing matches {terms:?}"))
            } else {
                Outcome::Found(hits)
            })
        })
    }

    /// The shortest chain of links between two symbols, in either direction.
    pub fn path(
        &self,
        a: &str,
        b: &str,
        max_len: u32,
        opts: &Options,
    ) -> Result<Reply<Vec<PathStep>>, DistError> {
        self.query(opts, |tx| {
            let first = match katalogr_query::resolve(tx, a)? {
                Resolution::One(h) => h,
                other => return Ok(unresolved(other, a)),
            };
            let second = match katalogr_query::resolve(tx, b)? {
                Resolution::One(h) => h,
                other => return Ok(unresolved(other, b)),
            };
            Ok(match katalogr_query::path(tx, &first, &second, max_len)? {
                Some(steps) => Outcome::Found(steps),
                None => Outcome::NoMatch(format!(
                    "no path from {} to {} within {max_len} steps",
                    first.name, second.name
                )),
            })
        })
    }

    /// One symbol, compactly: signature, doc, its links and rationale.
    pub fn explain(
        &self,
        text: &str,
        per_side: usize,
        opts: &Options,
    ) -> Result<Reply<Explain>, DistError> {
        self.query(opts, |tx| {
            on_target(tx, text, |t| katalogr_query::explain(tx, t, per_side))
        })
    }

    fn read(
        &self,
        engine: &Engine,
        consistency: Consistency,
        timeout: Duration,
        queued: usize,
        warnings: Vec<String>,
    ) -> Result<Report, DistError> {
        let a = engine.with_consistency(
            &QueryOptions {
                consistency: consistency.into(),
                timeout,
                scope: Scope::Project,
            },
            |_| (),
        )?;
        Ok(Report {
            watermark: a.watermark.0,
            pending: a.pending,
            stale: a.stale,
            timed_out: a.stale && consistency != Consistency::StaleOk,
            queued,
            warnings,
        })
    }
}

pub(crate) fn unknown_root(repo: &str) -> DistError {
    DistError::usage(format!(
        "{repo}: root unknown on this machine: run `katalogr project add-root {repo} <path>`"
    ))
}

fn rel_to_root(root: &Utf8Path, p: &str) -> Result<RelPath, DistError> {
    let given = Utf8Path::new(p);
    let rel = if given.is_absolute() {
        given
            .strip_prefix(root)
            .map_err(|_| DistError::usage(format!("{p}: not inside the repo root")))?
            .as_str()
    } else {
        p.strip_prefix("./").unwrap_or(p)
    };
    RelPath::new(rel).map_err(|e| DistError::usage(e.to_string()))
}
