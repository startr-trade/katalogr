//! Composition root: the Katalogr facade with extensions registered (ADR-0011).
//!
//! All behaviour of the command-line wrapper lives here; the wrapper parses, calls and renders.

mod claude;
mod error;
pub mod hooks;
mod roots;
mod setup;
mod span;
#[cfg(feature = "test-extractor")]
mod test_extractor;

pub use claude::{
    BEGIN_MARKER, DEFAULT_SESSION_BUDGET, END_MARKER, InstallOutcome, SESSION_BUDGET_KEY, SKILL_MD,
    SyncOutcome, claude_block, claude_sync, session_start, skill_install, skill_path,
    user_skill_path,
};
pub use error::DistError;
pub use katalogr_analytics as analytics;
pub use katalogr_query as query;
use katalogr_query::{
    Blame, CallerHit, DanglingRef, Explain, MapEntry, MirrorDrift, Partial, PathStep, RefHit,
    Resolution, SearchHit, SymbolHit, WhyResult, cap,
};
pub use katalogr_query::{ReplaceResult, ShowResult};
pub use setup::{ProjectCreated, kindex_init, project_init};

use camino::{Utf8Path, Utf8PathBuf};
use katalogr_analytics::{
    CommunityOptions, CommunityReport, CycleOptions, CycleReport, DEFAULT_SEED, DeadCodeOptions,
    DeadCodeReport, HubOptions, HubReport, RationaleReport, RationaleScope, STRUCTURAL_KINDS,
};
use katalogr_engine::history::{HISTORY_PREFIX, HistoryOptions, blame_commit, history_shas};
use katalogr_engine::scan::{PathChange, changed_paths};
use katalogr_engine::{
    Consistency as EngineConsistency, Engine, EventKind, NewEvent, QueryOptions, Scope,
    registry::ExtensionRegistry,
};
use katalogr_model::{Name, RelPath};
use katalogr_store::marker::{self, Overrides as StoreOverrides};
use katalogr_store::{Kindex, ProjectDir, ProjectName, ReadTx, StoreError};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

/// Every `--json` schema: the query schemas merged with the analytics schemas.
///
/// Panics if a command name occurs in both (a programming error caught by `query::schemas_current`).
pub fn schemas() -> BTreeMap<&'static str, serde_json::Value> {
    let mut all = katalogr_query::schemas();
    for (name, schema) in katalogr_analytics::schemas() {
        assert!(
            all.insert(name, schema).is_none(),
            "schema name {name} is registered twice"
        );
    }
    all
}

/// The project settings that tune the insights (`insights.*`), with their defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct InsightSettings {
    /// `insights.hub_min_degree`: smallest weighted degree of a hub.
    pub hub_min_degree: f64,
    /// `insights.seed`: seed of the community search.
    pub seed: u64,
    /// `insights.resolution`: modularity resolution.
    pub resolution: f64,
    /// `insights.community_min_size`: smallest community reported.
    pub community_min_size: usize,
    /// `insights.exclude_kinds`: link kinds that are not usage.
    pub exclude_kinds: Vec<String>,
}

impl Default for InsightSettings {
    fn default() -> Self {
        InsightSettings {
            // Calibrated on katalogr's own index: at 15 about 1.7% of symbols are hubs.
            hub_min_degree: 15.0,
            seed: DEFAULT_SEED,
            resolution: 1.0,
            community_min_size: 3,
            exclude_kinds: STRUCTURAL_KINDS.iter().map(|k| k.to_string()).collect(),
        }
    }
}

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

/// What `index --history` indexes: the commits with item trailers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryScope {
    /// Only commits after this revision (up to HEAD).
    pub since: Option<String>,
}

/// The katalogr facade for one resolved project.
pub struct Katalogr {
    project: ProjectDir,
    /// The repo of the nearest marker, if any.
    repo: Option<String>,
    /// Repo name to root on this machine: `roots.yaml`, overridden by the marker for its repo.
    roots: BTreeMap<String, Utf8PathBuf>,
}

/// Profiles embedded in the binary, applied before the project's own: `(file name, text)`.
const BUILTIN_PROFILES: &[(&str, &str)] =
    &[("generic.yaml", include_str!("../profiles/generic.yaml"))];

/// The built-in profiles as the engine takes them.
fn builtin_profiles() -> Vec<(String, String)> {
    BUILTIN_PROFILES
        .iter()
        .map(|(n, t)| (n.to_string(), t.to_string()))
        .collect()
}

/// Extension registration point. Register extractors here as they are written.
fn build_registry() -> ExtensionRegistry {
    let mut registry = ExtensionRegistry::new();
    registry.register(Box::new(katalogr_ext_rust::RustExtractor));
    registry.register(Box::new(katalogr_ext_ts::TsExtractor));
    registry.register(Box::new(katalogr_ext_sql::SqlExtractor));
    registry.register(Box::new(katalogr_ext_schema::XsdExtractor));
    registry.register(Box::new(katalogr_ext_schema::JsonSchemaExtractor));
    // Claims no file by itself: a profile `select` routes data documents to it.
    registry.register(Box::new(katalogr_ext_data::DataExtractor));
    // The board extension claims its own Markdown files before the general one.
    registry.register(Box::new(katalogr_ext_kanbanr::BoardExtractor));
    registry.register(Box::new(katalogr_ext_md::MarkdownExtractor));
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
    /// Caps that cut this answer (empty when none did). The renderer adds budget cuts.
    pub partial: Vec<Partial>,
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
        engine.set_builtin_profiles(builtin_profiles());
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
    ///
    /// With `history`, the commits that carry item trailers are queued as `@history/<sha>`
    /// paths (every repo with `full`, else the current repo), and the files are queued only
    /// when `full` or `range` asks for them.
    pub fn index(
        &self,
        range: Option<String>,
        full: bool,
        history: Option<&HistoryScope>,
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
        } else if history.is_none() {
            let (repo, _) = self.current()?;
            events.push(NewEvent::rescan(repo));
        }
        if let Some(h) = history {
            let mut repos: Vec<(String, Utf8PathBuf)> = Vec::new();
            if full {
                for r in &self.project.config().repos {
                    if let Some(root) = self.roots.get(&r.name) {
                        repos.push((r.name.clone(), root.clone()));
                    }
                }
            } else {
                let (repo, root) = self.current()?;
                repos.push((repo.to_string(), root.to_owned()));
            }
            for (repo, root) in repos {
                let shas = history_shas(
                    root.as_std_path(),
                    h.since.as_deref(),
                    &HistoryOptions::default(),
                )?;
                for sha in shas {
                    let path = RelPath::new(&format!("{HISTORY_PREFIX}{sha}"))
                        .map_err(|e| DistError::usage(e.to_string()))?;
                    events.push(NewEvent::file(&repo, &path, EventKind::Changed));
                }
            }
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
            partial: Vec::new(),
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
        // One extra level tells whether the depth cut the answer.
        let mut partial = Vec::new();
        // `callers` treats a depth of 0 as 1.
        let depth = depth.max(1);
        let mut reply = self.query(opts, |tx| {
            on_target(tx, text, |t| {
                let mut hits = katalogr_query::callers(tx, t, depth.saturating_add(1))?;
                if hits.iter().any(|h| h.depth > depth) {
                    hits.retain(|h| h.depth <= depth);
                    partial.push(Partial::new(
                        cap::DEPTH,
                        u64::from(depth),
                        hits.len() as u64,
                        None,
                    ));
                }
                Ok(hits)
            })
        })?;
        reply.partial = partial;
        Ok(reply)
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
        // A directory that is not a git repository has no commits to blame, which is no failure.
        let blame = if root.join(".git").exists() {
            match blame_commit(root.as_std_path(), path.as_str(), line) {
                Ok(sha) => Blame::Commit(sha),
                Err(why) => Blame::Unavailable(why),
            }
        } else {
            Blame::NotRequested
        };
        self.query(opts, |tx| {
            Ok(from_resolution(
                katalogr_query::why(tx, repo, &path, line, &blame)?,
                spec,
            ))
        })
    }

    /// Unresolved references of the given kinds (default: Markdown links, annotations and
    /// trailers), across the project.
    pub fn dangling(
        &self,
        kinds: &[&str],
        opts: &Options,
    ) -> Result<Reply<Vec<DanglingRef>>, DistError> {
        self.query(opts, |tx| {
            let kinds = if kinds.is_empty() {
                katalogr_query::DANGLING_KINDS
            } else {
                kinds
            };
            Ok(Outcome::Found(katalogr_query::dangling(tx, kinds)?))
        })
    }

    /// Contracts with two or more declarers (mirrored copies of a structure), compared field by
    /// field; only `kinds` when given. Pairs with no difference are left out.
    pub fn mirrors(
        &self,
        kinds: &[&str],
        opts: &Options,
    ) -> Result<Reply<Vec<MirrorDrift>>, DistError> {
        self.query(opts, |tx| {
            Ok(Outcome::Found(katalogr_query::mirror_drift(tx, kinds)?))
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
        // One extra hit tells whether the limit cut the answer.
        let mut partial = Vec::new();
        let mut reply = self.query(opts, |tx| {
            let mut hits = katalogr_query::search(tx, terms, limit.saturating_add(1))?;
            if hits.len() > limit {
                hits.truncate(limit);
                partial.push(Partial::new(cap::LIMIT, limit as u64, limit as u64, None));
            }
            Ok(if hits.is_empty() {
                Outcome::NoMatch(format!("nothing matches {terms:?}"))
            } else {
                Outcome::Found(hits)
            })
        })?;
        reply.partial = partial;
        Ok(reply)
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
                    "no path from {} to {} within {max_len} steps; {} (searched up to {max_len} links)",
                    first.name,
                    second.name,
                    katalogr_query::PARTIAL_NOTE
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
        let mut partial = Vec::new();
        let mut reply = self.query(opts, |tx| {
            on_target(tx, text, |t| {
                let x = katalogr_query::explain(tx, t, per_side)?;
                if x.outgoing_total as usize > per_side || x.incoming_total as usize > per_side {
                    partial.push(Partial::new(
                        cap::PER_SIDE,
                        per_side as u64,
                        (x.outgoing.len() + x.incoming.len()) as u64,
                        Some(x.outgoing_total + x.incoming_total),
                    ));
                }
                Ok(x)
            })
        })?;
        reply.partial = partial;
        Ok(reply)
    }

    /// The insight defaults from the project settings (`insights.*`); a missing or malformed
    /// value keeps its default.
    pub fn insight_settings(&self) -> InsightSettings {
        let settings = &self.project.config().settings;
        let d = InsightSettings::default();
        InsightSettings {
            hub_min_degree: settings
                .get("insights.hub_min_degree")
                .and_then(|v| v.as_f64())
                .unwrap_or(d.hub_min_degree),
            seed: settings
                .get("insights.seed")
                .and_then(|v| v.as_u64())
                .unwrap_or(d.seed),
            resolution: settings
                .get("insights.resolution")
                .and_then(|v| v.as_f64())
                .unwrap_or(d.resolution),
            community_min_size: settings
                .get("insights.community_min_size")
                .and_then(|v| v.as_u64())
                .map_or(d.community_min_size, |n| n as usize),
            exclude_kinds: settings
                .get("insights.exclude_kinds")
                .and_then(|v| v.as_sequence())
                .map(|seq| {
                    seq.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or(d.exclude_kinds),
        }
    }

    /// Hubs: the nodes with the highest weighted degree.
    pub fn hubs(&self, opts: &HubOptions, o: &Options) -> Result<Reply<HubReport>, DistError> {
        let mut reply = self.query(o, |tx| {
            Ok(Outcome::Found(katalogr_analytics::hubs(tx, opts)?))
        })?;
        if let Outcome::Found(r) = &reply.outcome {
            reply.partial = limit_partial(opts.limit, r.items.len(), None);
        }
        Ok(reply)
    }

    /// Communities of the usage graph.
    pub fn communities(
        &self,
        opts: &CommunityOptions,
        o: &Options,
    ) -> Result<Reply<CommunityReport>, DistError> {
        let mut reply = self.query(o, |tx| {
            Ok(Outcome::Found(katalogr_analytics::communities(tx, opts)?))
        })?;
        if let Outcome::Found(r) = &reply.outcome {
            reply.partial = limit_partial(opts.limit, r.items.len(), Some(r.communities_total));
            let shown: usize = r.items.iter().map(|c| c.top.len()).sum();
            let total: u64 = r.items.iter().map(|c| c.size).sum();
            if total > shown as u64 {
                reply.partial.push(Partial::new(
                    cap::MEMBERS,
                    opts.members as u64,
                    shown as u64,
                    Some(total),
                ));
            }
        }
        Ok(reply)
    }

    /// Dependency cycles.
    pub fn cycles(
        &self,
        opts: &CycleOptions,
        o: &Options,
    ) -> Result<Reply<CycleReport>, DistError> {
        let mut reply = self.query(o, |tx| {
            Ok(Outcome::Found(katalogr_analytics::cycles(tx, opts)?))
        })?;
        if let Outcome::Found(r) = &reply.outcome {
            reply.partial = limit_partial(opts.limit, r.items.len(), Some(r.cycles_total));
            let shown: usize = r.items.iter().map(|c| c.members.len()).sum();
            let total: u64 = r.items.iter().map(|c| c.members_total).sum();
            if total > shown as u64 {
                reply.partial.push(Partial::new(
                    cap::MEMBERS,
                    opts.members as u64,
                    shown as u64,
                    Some(total),
                ));
            }
        }
        Ok(reply)
    }

    /// Dead-code candidates. Fills `opts.code_kinds` from the registered extensions first.
    pub fn dead_code(
        &self,
        mut opts: DeadCodeOptions,
        o: &Options,
    ) -> Result<Reply<DeadCodeReport>, DistError> {
        opts.code_kinds = build_registry()
            .extractors()
            .map(|e| {
                (
                    e.id().to_string(),
                    e.code_kinds().iter().map(|k| k.to_string()).collect(),
                )
            })
            .filter(|(_, kinds): &(String, std::collections::BTreeSet<String>)| !kinds.is_empty())
            .collect();
        let mut reply = self.query(o, |tx| {
            Ok(Outcome::Found(katalogr_analytics::dead_code(tx, &opts)?))
        })?;
        if let Outcome::Found(r) = &reply.outcome {
            reply.partial = limit_partial(opts.limit, r.items.len(), Some(r.candidates_total));
        }
        Ok(reply)
    }

    /// Rationale of one symbol, a path prefix, the top `hubs` hubs, or (none given) everything.
    /// `symbol` is resolved like `explain`.
    pub fn rationale(
        &self,
        symbol: Option<&str>,
        path: Option<&str>,
        hubs: Option<usize>,
        limit: usize,
        o: &Options,
    ) -> Result<Reply<RationaleReport>, DistError> {
        let given = [symbol.is_some(), path.is_some(), hubs.is_some()];
        if given.iter().filter(|g| **g).count() > 1 {
            return Err(DistError::usage(
                "give one of SYMBOL, --path and --hubs".to_string(),
            ));
        }
        let mut reply = self.query(o, |tx| {
            let scope = if let Some(text) = symbol {
                match katalogr_query::resolve(tx, text)? {
                    Resolution::One(h) => match katalogr_model::parse_name(&h.name) {
                        Ok(name) => RationaleScope::Symbol(name.id()),
                        Err(e) => return Ok(Outcome::NoMatch(e.to_string())),
                    },
                    other => return Ok(unresolved(other, text)),
                }
            } else if let Some(prefix) = path {
                RationaleScope::Path(prefix.to_string())
            } else if let Some(n) = hubs {
                RationaleScope::Hubs(n)
            } else {
                RationaleScope::All
            };
            Ok(Outcome::Found(katalogr_analytics::rationale(
                tx, &scope, limit,
            )?))
        })?;
        if let Outcome::Found(r) = &reply.outcome {
            reply.partial = limit_partial(limit, r.items.len(), Some(r.entries_total));
        }
        Ok(reply)
    }

    fn read(
        &self,
        engine: &Engine,
        consistency: Consistency,
        timeout: Duration,
        queued: usize,
        mut warnings: Vec<String>,
    ) -> Result<Report, DistError> {
        let a = engine.with_consistency(
            &QueryOptions {
                consistency: consistency.into(),
                timeout,
                scope: Scope::Project,
            },
            |tx| katalogr_engine::profiles::stored_errors(tx),
        )?;
        warnings.extend(a.value.iter().map(|e| format!("profile refused: {e}")));
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

/// The `limit` marker of a report that shows `shown` items under `limit`: present when the
/// limit was reached and the report's total (if it has one) is larger.
fn limit_partial(limit: usize, shown: usize, total: Option<u64>) -> Vec<Partial> {
    let cut = shown == limit && total.is_none_or(|t| t > limit as u64);
    if cut {
        vec![Partial::new(cap::LIMIT, limit as u64, shown as u64, total)]
    } else {
        Vec::new()
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

#[cfg(test)]
mod profile_tests {
    #[test]
    fn builtin_profiles_are_valid() {
        for (name, text) in super::BUILTIN_PROFILES {
            katalogr_engine::profiles::parse(&format!("builtin:{name}"), text)
                .unwrap_or_else(|e| panic!("{e}"));
        }
    }
}
