//! Composition root: the Katalogr facade with extensions registered (ADR-0011).
//!
//! All behaviour of the command-line wrapper lives here; the wrapper parses, calls and renders.

mod answers;
mod at;
mod audit;
pub use answers::{
    AnswerCheckReport, AnswerCounters, AnswerMismatch, AnswerRequest, AnswerStats, StoredQuery,
    validity,
};
pub use at::position_without_board;
pub use audit::{AuditOutcome, AuditRecord, PositionText};
mod changes;
mod claude;
mod docs_affected;
mod impact;
pub use docs_affected::{DEFAULT_DOCS_LIMIT, DocsAffectedParams};
pub use impact::ImpactInput;
pub mod contexts;
pub mod contract;
pub use katalogr_query::{PositionAnswer, PositionRef, ViewQuery, WorkingSetSummary};
pub mod doc_blocks;
pub mod doc_views;
pub mod doctor;
mod error;
pub mod eval;
pub mod hooks;
mod roots;
mod setup;
mod shard_show;
pub mod snapshot;
pub mod snapshot_import;
mod symbol_count;
pub use symbol_count::symbol_count;
mod span;
#[cfg(feature = "test-extractor")]
mod test_extractor;
/// How many `.tx` files the test extractor has parsed in this process (for benchmarks).
#[cfg(feature = "test-extractor")]
pub use test_extractor::extract_calls;
mod threads;
mod timeline;
pub use timeline::DEFAULT_TIMELINE_LIMIT;
mod view_store;
pub use view_store::ViewStore;
mod views;
pub use views::{FreshCheck, ViewBytes};
pub mod watch;
pub mod working_set;

use crate::contexts::ContextChoice;
pub use claude::{
    BEGIN_MARKER, DEFAULT_SESSION_BUDGET, END_MARKER, InstallOutcome, SESSION_BUDGET_KEY, SKILL_MD,
    SyncOutcome, claude_block, claude_sync, session_start, skill_install, skill_path,
    user_skill_path,
};
pub use error::DistError;
pub use katalogr_analytics as analytics;
pub use katalogr_query as query;
pub use katalogr_query::DEFAULT_USE_LIMIT;
use katalogr_query::{
    Blame, CallerHit, DanglingRef, Diagnostic, Explain, Fold, MapEntry, MirrorDrift, Partial,
    PathStep, RefHit, Resolution, SearchHit, SearchParams, SymbolHit, TestHit, TestsOptions, Trace,
    WhyResult, cap,
};
pub use katalogr_query::{ReplaceResult, ShowResult};
pub use katalogr_render as render;
pub use katalogr_store::SnapshotManifest;
#[cfg(feature = "web")]
pub use katalogr_web as web;
pub use setup::{ProjectCreated, kindex_init, project_init};
pub use snapshot::SnapshotExport;
pub use snapshot_import::{SnapshotImport, read_snapshot_manifest};
pub use threads::{BATCH_BYTES_KEY, THREADS_ENV, THREADS_KEY, resolve_parallelism};

use camino::{Utf8Path, Utf8PathBuf};
use katalogr_analytics::{
    CommunityOptions, CommunityReport, CoverageInput, CoverageReport, CycleOptions, CycleReport,
    DEFAULT_SEED, DeadCodeOptions, DeadCodeReport, HubOptions, HubReport, RationaleReport,
    RationaleScope, STRUCTURAL_KINDS,
};
use katalogr_engine::history::{HISTORY_PREFIX, HistoryOptions, blame_commit, history_shas};
use katalogr_engine::scan::{PathChange, changed_paths};
use katalogr_engine::{
    Consistency as EngineConsistency, Engine, EngineError, EventKind, NewEvent, QueryOptions,
    Scope, registry::ExtensionRegistry,
};
use katalogr_model::{Name, RelPath};
use katalogr_store::marker::{self, Overrides as StoreOverrides};
use katalogr_store::{
    CompactMode, CompactOutcome, CompactPolicy, Kindex, ProjectDir, ProjectName, ReadTx, StoreError,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Every `--json` schema: the query schemas merged with the analytics schemas and `doctor`.
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
    assert!(
        all.insert(
            "render",
            schemars::schema_for!(katalogr_query::Envelope<katalogr_render::ViewGraph>).to_value()
        )
        .is_none(),
        "schema name render is registered twice"
    );
    assert!(
        all.insert("view", katalogr_query::view_schema()).is_none(),
        "schema name view is registered twice"
    );
    assert!(
        all.insert("doctor", doctor::schema()).is_none(),
        "schema name doctor is registered twice"
    );
    assert!(
        all.insert("audit", audit::schema()).is_none(),
        "schema name audit is registered twice"
    );
    assert!(
        all.insert("contract", contract::schema()).is_none(),
        "schema name contract is registered twice"
    );
    assert!(
        all.insert("answers-check", answers::check_schema())
            .is_none(),
        "schema name answers-check is registered twice"
    );
    assert!(
        all.insert("answers-stats", answers::stats_schema())
            .is_none(),
        "schema name answers-stats is registered twice"
    );
    assert!(
        all.insert("snapshot-manifest", snapshot::manifest_schema())
            .is_none(),
        "schema name snapshot-manifest is registered twice"
    );
    assert!(
        all.insert("snapshot-import", snapshot_import::schema())
            .is_none(),
        "schema name snapshot-import is registered twice"
    );
    assert!(
        all.insert("position", katalogr_query::position_schema())
            .is_none(),
        "schema name position is registered twice"
    );
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

/// Options of the coverage report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageOptions {
    /// Only this repository of the project.
    pub repo: Option<String>,
    /// How many suffixes to list per repository for unclaimed files.
    pub suffix_top: usize,
}

impl Default for CoverageOptions {
    fn default() -> Self {
        CoverageOptions {
            repo: None,
            suffix_top: 20,
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
    /// `base`, `current` or an id from `context list` (FEAT-043 R-4); `None` is `current`.
    pub context: Option<String>,
    /// Folders of release snapshots the timeline also reads (FEAT-033); empty is none.
    pub snapshot_dirs: Vec<PathBuf>,
}

/// Options for reads. `None` means the command's default.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub consistency: Option<Consistency>,
    /// Seconds; may be fractional.
    pub timeout: Option<f64>,
    /// Record a diagnostic trace of the query (FEAT-065).
    pub diagnostic: bool,
    /// `refs` and `callers`: list every hit instead of folding (`--expand`, FEAT-077).
    pub expand: bool,
    /// Answer for this position (`--at`, FEAT-084): `<commit>`, `<commit>+<dirty>` or
    /// `none+<dirty>`.
    pub at: Option<String>,
    /// The question, for the answer's view ID (FEAT-084). `None`: no ID is computed.
    pub view: Option<katalogr_query::ViewQuery>,
    /// Ranking of `search` and `map` (FEAT-039). The library default is neutral; the CLI asks
    /// for [`Ranking::Default`] unless `--neutral` or `--at` is given.
    pub ranking: Ranking,
    /// Recompute instead of reading a stored answer; the result is still written (`--fresh`,
    /// FEAT-096).
    pub fresh: bool,
}

/// How `search` and `map` order their answers (FEAT-039).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Ranking {
    /// Boost the working set: dirty, recent, branch and item paths.
    Default,
    /// Order by relevance alone, as before. Always used under `--at`, in `eval` and by the
    /// library unless asked otherwise.
    #[default]
    Neutral,
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
    /// What the index reflects, one entry per repo sorted by repo (FEAT-084).
    pub positions: Vec<katalogr_query::PositionRef>,
    /// The working set that reordered a `search` or `map` answer (FEAT-039).
    pub working_set: Option<katalogr_query::WorkingSetSummary>,
    /// Nothing was pending and every checkout's position was unchanged across the read.
    pub exact: bool,
    /// The ID of this answer; set only on an exact answer to a question that has one.
    pub view_id: Option<String>,
    /// The context the answer was read from, when it names one.
    pub context: Option<String>,
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
    /// Which context a read or write goes through (FEAT-043); `Current` unless set.
    context: contexts::ContextChoice,
    /// Warnings raised while choosing a context, handed out with the next report.
    notes: std::sync::Mutex<Vec<String>>,
    /// View-ID arguments a read added while it ran (the working-set hash), taken by
    /// `finish_report`.
    view_extra: std::sync::Mutex<Vec<(String, String)>>,
    /// Snapshot folders the timeline reads, from `Overrides::snapshot_dirs`.
    snapshot_dirs: Vec<PathBuf>,
    /// Stored answers: the build they are valid for, the store and the counts (FEAT-096).
    answers: answers::AnswerState,
}

/// Profiles embedded in the binary, applied before the project's own: `(file name, text)`.
const BUILTIN_PROFILES: &[(&str, &str)] = &[
    ("generic.yaml", include_str!("../profiles/generic.yaml")),
    ("http-rust.yaml", include_str!("../profiles/http-rust.yaml")),
    ("http-ts.yaml", include_str!("../profiles/http-ts.yaml")),
];

/// The profiles in force, for `katalogr profiles list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileListing {
    /// Built-in files (`builtin:<name>.yaml`) in application order, including any a project
    /// file replaced.
    pub builtins: Vec<String>,
    /// Project files in force (`profiles/<file>`).
    pub project: Vec<ProjectProfile>,
    /// Project files that are refused, with their file, line and reason.
    pub refused: Vec<katalogr_engine::profiles::ProfileError>,
}

/// One project profile file in force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectProfile {
    pub file: String,
    /// The built-in file it replaces by profile name.
    pub replaces: Option<String>,
}

/// Built-in profile names (file names without `.yaml`), in application order.
pub fn builtin_profile_names() -> Vec<&'static str> {
    BUILTIN_PROFILES
        .iter()
        .map(|(n, _)| n.strip_suffix(".yaml").unwrap_or(n))
        .collect()
}

/// A built-in profile's text by name (`http-ts` or `http-ts.yaml`).
pub fn builtin_profile(name: &str) -> Option<&'static str> {
    let name = name.strip_suffix(".yaml").unwrap_or(name);
    BUILTIN_PROFILES
        .iter()
        .find(|(n, _)| n.strip_suffix(".yaml").unwrap_or(n) == name)
        .map(|(_, t)| *t)
}

/// The built-in profiles as the engine takes them.
fn builtin_profiles() -> Vec<(String, String)> {
    BUILTIN_PROFILES
        .iter()
        .map(|(n, t)| (n.to_string(), t.to_string()))
        .collect()
}

/// What a coverage report takes from the disk (see `Katalogr::coverage_parts`).
pub(crate) struct CoverageParts {
    profiles: katalogr_engine::profiles::ProfileSet,
    unclaimed: BTreeMap<String, Vec<RelPath>>,
    roots_missing: Vec<String>,
}

/// The report of `opts` over one snapshot, with the disk side already read.
pub(crate) fn coverage_run(
    engine: &Engine,
    opts: &CoverageOptions,
    parts: &CoverageParts,
    tx: &ReadTx,
) -> Result<CoverageReport, StoreError> {
    let registry = engine.registry();
    let language = |ext: &str, path: &RelPath| match registry.by_id(ext) {
        Some(e) => e.language(path).to_string(),
        None => ext.to_string(),
    };
    let claim = |path: &RelPath| claim_for(registry, &parts.profiles, path);
    let input = CoverageInput {
        language: &language,
        claim: &claim,
        unclaimed: parts.unclaimed.clone(),
        roots_missing: parts.roots_missing.clone(),
        exclude_prefixes: vec![HISTORY_PREFIX.to_string()],
        repo: opts.repo.clone(),
        suffix_top: opts.suffix_top,
    };
    katalogr_analytics::coverage(tx, &input)
}

/// Extension id to its code kinds, from the registry (dead-code's `code_kinds`).
pub(crate) fn registry_code_kinds() -> BTreeMap<String, std::collections::BTreeSet<String>> {
    build_registry()
        .extractors()
        .map(|e| {
            (
                e.id().to_string(),
                e.code_kinds().iter().map(|k| k.to_string()).collect(),
            )
        })
        .filter(|(_, kinds): &(String, std::collections::BTreeSet<String>)| !kinds.is_empty())
        .collect()
}

/// The extension and language that claim `path`, routing as the indexer does: a profile
/// `select` first, then the registry.
fn claim_for(
    registry: &ExtensionRegistry,
    profiles: &katalogr_engine::profiles::ProfileSet,
    path: &RelPath,
) -> Option<(String, String)> {
    katalogr_engine::census::extractor_for(registry, profiles, path)
        .map(|e| (e.id().to_string(), e.language(path).to_string()))
}

/// Extension registration point. Register extractors here as they are written.
fn build_registry() -> ExtensionRegistry {
    let mut registry = ExtensionRegistry::new();
    registry.register(Box::new(katalogr_ext_rust::RustExtractor));
    registry.register(Box::new(katalogr_ext_ts::TsExtractor));
    registry.register(Box::new(katalogr_ext_sql::SqlExtractor));
    // Claims only conventionally named OpenAPI files (`openapi*`, `swagger*`); a profile
    // `select` routes any other. Registered before the schema extractors so that it claims
    // `openapi.json` before the general JSON Schema extractor can.
    registry.register(Box::new(katalogr_ext_openapi::OpenApiExtractor));
    registry.register(Box::new(katalogr_ext_schema::XsdExtractor));
    registry.register(Box::new(katalogr_ext_schema::JsonSchemaExtractor));
    // Claims no file by itself: a profile `select` routes data documents to it.
    registry.register(Box::new(katalogr_ext_data::DataExtractor));
    // The board extension claims its own Markdown files before the general one.
    registry.register(Box::new(katalogr_ext_kanbanr::BoardExtractor));
    registry.register(Box::new(katalogr_ext_md::MarkdownExtractor));
    // Claims no file by itself: a profile `select` routes unparsed formats to it.
    registry.register(Box::new(katalogr_ext_text::TextExtractor));
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
    /// `--at` asked for a position no context holds (FEAT-084); these are the ones that are.
    PositionUnavailable {
        wanted: String,
        available: Vec<katalogr_query::PositionRef>,
    },
}

/// A query answer with what the read saw.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply<T> {
    pub outcome: Outcome<T>,
    pub report: Report,
    /// The trace of the query: `Some` only for a traced command run with `Options::diagnostic`.
    pub diagnostic: Option<Trace>,
    /// Caps that cut this answer (empty when none did). The renderer adds budget cuts.
    pub partial: Vec<Partial>,
    /// Folds of a `refs` or `callers` answer; empty for every other command and with `expand`.
    pub folded: Vec<Fold>,
}

/// What a `search` or `map` read learned about its working set.
#[derive(Default)]
struct WsRun {
    summary: Option<katalogr_query::WorkingSetSummary>,
    warnings: Vec<String>,
}

/// Holds the view-ID arguments a read adds (`view_extra`) for the length of one question: they
/// are removed when the guard drops, so a read that fails before its report is finished leaves
/// none behind for the next one.
pub(crate) struct ExtraScope<'a>(&'a std::sync::Mutex<Vec<(String, String)>>);

impl Drop for ExtraScope<'_> {
    fn drop(&mut self) {
        if let Ok(mut extra) = self.0.lock() {
            extra.clear();
        }
    }
}

impl WsRun {
    fn finish(self, report: &mut Report) {
        report.working_set = self.summary;
        report.warnings.extend(self.warnings);
    }
}

fn found_or_none(items: Vec<SymbolHit>, text: &str) -> Outcome<Vec<SymbolHit>> {
    if items.is_empty() {
        Outcome::NoMatch(format!("no symbol matches {text:?}"))
    } else {
        Outcome::Found(items)
    }
}

/// [`on_target`], recording the routing into `diag`.
pub(crate) fn on_target_traced<T>(
    tx: &ReadTx,
    text: &str,
    diag: &mut Diagnostic,
    f: impl FnOnce(&SymbolHit, &mut Diagnostic) -> Result<T, StoreError>,
) -> Result<Outcome<T>, StoreError> {
    Ok(match katalogr_query::resolve_traced(tx, text, diag)? {
        Resolution::One(t) => Outcome::Found(f(&t, diag)?),
        Resolution::Ambiguous(c) => Outcome::Ambiguous(c),
        Resolution::None => Outcome::NoMatch(format!("no symbol matches {text:?}")),
    })
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

/// Follow-up commands for a `why` answer that name another tool's view: when a `rationale`
/// entry reached an item (kind `refs-item` or `changed-by`), the board owner's own `why` for
/// the same position. Text only: nothing is run.
pub fn why_next(w: &WhyResult) -> Vec<String> {
    if w.rationale
        .iter()
        .any(|r| r.kind == "refs-item" || r.kind == "changed-by")
    {
        vec![format!(
            "kanbanr why {}:{}",
            w.symbol.path, w.symbol.span.start_line
        )]
    } else {
        Vec::new()
    }
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
    /// Built-in profile names (file names without `.yaml`), in application order.
    pub fn builtin_profile_names() -> Vec<&'static str> {
        builtin_profile_names()
    }

    /// A built-in profile's text by name (`http-ts` or `http-ts.yaml`).
    pub fn builtin_profile(name: &str) -> Option<&'static str> {
        builtin_profile(name)
    }

    /// The built-in profiles, the project's profile files in force, which built-ins they replace,
    /// and the files that are refused.
    pub fn profile_listing(&self) -> ProfileListing {
        let builtins: Vec<(&str, &str)> = BUILTIN_PROFILES.to_vec();
        let report = katalogr_engine::profiles::ProfileSet::load_full(
            self.project.project_root(),
            &builtins,
            &BTreeMap::new(),
            &build_registry().normalisers(),
        );
        let overridden = report.set.overridden();
        let project = report
            .set
            .files()
            .into_iter()
            .filter(|(f, _)| !f.starts_with("builtin:"))
            .map(|(f, _)| ProjectProfile {
                file: f.to_string(),
                replaces: overridden
                    .iter()
                    .find(|(pf, _)| pf == f)
                    .map(|(_, b)| b.clone()),
            })
            .collect();
        ProfileListing {
            builtins: BUILTIN_PROFILES
                .iter()
                .map(|(n, _)| format!("builtin:{n}"))
                .collect(),
            project,
            refused: report.errors,
        }
    }

    /// Only the built-ins: what `profile_listing` returns outside any project.
    pub fn builtin_listing() -> ProfileListing {
        ProfileListing {
            builtins: BUILTIN_PROFILES
                .iter()
                .map(|(n, _)| format!("builtin:{n}"))
                .collect(),
            project: Vec::new(),
            refused: Vec::new(),
        }
    }

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
        let context = match overrides.context.as_deref() {
            Some(c) => contexts::ContextChoice::parse(c)?,
            None => contexts::ContextChoice::Current,
        };
        Ok(Katalogr {
            project,
            repo: r.repo,
            roots,
            context,
            notes: std::sync::Mutex::new(Vec::new()),
            view_extra: std::sync::Mutex::new(Vec::new()),
            snapshot_dirs: overrides.snapshot_dirs.clone(),
            answers: answers::AnswerState::default(),
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
        self.engine_on(self.layer_dir(deadline)?, deadline)
    }

    /// The roots a `dir` reads with, and its manifest when it is a layer. A checkout context
    /// reads its own checkout, whichever one this command runs in.
    fn layer_setup(
        &self,
        dir: &ProjectDir,
    ) -> (
        BTreeMap<String, Utf8PathBuf>,
        Option<katalogr_store::ContextManifest>,
    ) {
        let manifest = dir
            .is_layer()
            .then(|| katalogr_store::ContextManifest::read(dir.root()).ok())
            .flatten();
        let mut roots = self.roots.clone();
        if let Some(m) = &manifest
            && m.kind == katalogr_store::ContextKind::Checkout
            && let Some(root) = &m.root
        {
            roots.insert(m.repo.clone(), root.clone());
        }
        (roots, manifest)
    }

    /// An engine over `dir`: the base, or one of its layers.
    fn engine_on(&self, dir: ProjectDir, deadline: Instant) -> Result<Engine, DistError> {
        let (roots, manifest) = self.layer_setup(&dir);
        let engine = Engine::open(dir, build_registry(), roots, deadline)?;
        self.configure_layer(engine, manifest)
    }

    /// `configure`, then a commit context reads git objects at its commit, never the disk.
    fn configure_layer(
        &self,
        mut engine: Engine,
        manifest: Option<katalogr_store::ContextManifest>,
    ) -> Result<Engine, DistError> {
        let manifest_kind = manifest.clone();
        if let Some(m) = manifest
            && m.kind == katalogr_store::ContextKind::Commit
            && let (Some(commit), Some(root)) = (m.commit.clone(), self.roots.get(&m.repo))
        {
            engine.set_source(
                &m.repo,
                katalogr_engine::FileSource::Commit {
                    repo_root: root.clone(),
                    commit,
                },
            );
        }
        self.pin_positions(&mut engine, manifest_kind.as_ref())?;
        self.recorded_only(&mut engine)?;
        self.configure(engine)
    }

    /// FEAT-097 R-13. A repo the project lists, with no root here and a recorded position (an
    /// imported snapshot), answers from its rows. It is told apart from a repo with no record,
    /// which is absent from this index.
    fn recorded_only(&self, engine: &mut Engine) -> Result<(), DistError> {
        use std::collections::BTreeSet;
        let missing: Vec<String> = self
            .project
            .config()
            .repos
            .iter()
            .filter(|r| !engine.roots().contains_key(&r.name))
            .map(|r| r.name.clone())
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let snap = engine.cache().read()?;
        let mut only = BTreeSet::new();
        for repo in missing {
            if let Some(p) = katalogr_engine::consistency::recorded_position(&snap, &repo)?
                && p.commit.as_deref().is_some_and(|c| !c.is_empty())
            {
                only.insert(repo);
            }
        }
        drop(snap);
        engine.set_recorded_only(only);
        Ok(())
    }

    /// FEAT-084. Fix the position of every repo this context does not read from a checkout: a
    /// commit context's repo at its commit, every other repo of a layer at the commit the base
    /// reflects (none if unknown), and, for the base itself, every repo with a git checkout.
    /// A checkout layer's own repo and plain directories are read live.
    fn pin_positions(
        &self,
        engine: &mut Engine,
        manifest: Option<&katalogr_store::ContextManifest>,
    ) -> Result<(), DistError> {
        use katalogr_store::{ContextKind, Reader};
        // What the base reflects now, not what it reflected when this layer was seeded; a layer
        // falls back to its own copy when the base cannot be read beside a writer.
        let base = if manifest.is_some() {
            katalogr_store::Cache::open_reader(&self.project)?
        } else {
            None
        };
        let snap = match &base {
            Some(c) => c.read()?,
            None => engine.cache().read()?,
        };
        let repos: Vec<String> = engine.roots().keys().cloned().collect();
        let mut pins = Vec::new();
        for repo in repos {
            let pin = match manifest {
                Some(m) if m.repo == repo && m.kind == ContextKind::Checkout => continue,
                Some(m) if m.repo == repo => m.commit.clone(),
                None if !engine.roots()[&repo].join(".git").exists() => continue,
                _ => None,
            };
            let commit = match pin {
                Some(c) => Some(c),
                None => snap
                    .meta(&contexts::base_commit_key(&repo))?
                    .and_then(|b| String::from_utf8(b).ok()),
            };
            pins.push((repo, commit));
        }
        drop(snap);
        for (repo, commit) in pins {
            engine.set_pinned_position(&repo, commit);
        }
        Ok(())
    }

    /// Registry, profiles, parallelism and filters, shared by every open.
    fn configure(&self, mut engine: Engine) -> Result<Engine, DistError> {
        engine.set_builtin_profiles(builtin_profiles());
        engine.set_parallelism(resolve_parallelism(
            &self.project.config().settings,
            std::env::var(THREADS_ENV).ok().as_deref(),
        )?);
        for repo in &self.project.config().repos {
            engine.set_repo_filters(&repo.name, repo.include.clone(), repo.exclude.clone());
        }
        self.after_open(&engine)?;
        Ok(engine)
    }

    /// What follows opening a graph that was lost (FEAT-094 R-8, R-10). A graph of another
    /// schema, or a corrupt one, was recreated empty: one rescan per repo with a root is queued,
    /// so reads and `index` rebuild it by extracting. A repo with no root cannot be, and is
    /// reported. A missing file queues nothing (R-9): see [`Katalogr::note_missing_index`].
    fn after_open(&self, engine: &Engine) -> Result<(), DistError> {
        if engine.cache().state() != katalogr_store::OpenState::Replaced {
            return Ok(());
        }
        for repo in &self.project.config().repos {
            if engine.roots().contains_key(&repo.name) {
                engine.inbox().append(&NewEvent::rescan(&repo.name))?;
            } else {
                self.note(format!(
                    "repo {} has no root here, so it is absent from this index; add it with katalogr project add-root",
                    repo.name
                ));
            }
        }
        Ok(())
    }

    /// R-9. A graph file that was missing and has been created empty: say so, queue nothing.
    fn note_missing_index(&self, engine: &Engine) {
        if engine.cache().state() == katalogr_store::OpenState::Created {
            self.note("no index yet: run katalogr index --full".to_string());
        }
    }

    /// The read-only engine over `dir`, or `None` when the cache cannot be read that way
    /// (FEAT-080 R-6).
    fn reader_engine(&self, dir: ProjectDir) -> Result<Option<Engine>, DistError> {
        let (roots, manifest) = self.layer_setup(&dir);
        Engine::open_reader(dir, build_registry(), roots)?
            .map(|e| self.configure_layer(e, manifest))
            .transpose()
    }

    /// The engine for a read at `c` (FEAT-080 R-1, R-3, R-6), over the chosen context's own
    /// cache. `stale-ok` reads through a read-only handle beside any writer. `wait` and
    /// `wait-all` try the writer open once; a cache held by another process then gets a
    /// read-only handle to poll on. A handle that cannot be had falls back to the writer open,
    /// bounded by `deadline`.
    fn engine_for(
        &self,
        c: impl Into<EngineConsistency>,
        deadline: Instant,
    ) -> Result<Engine, DistError> {
        self.engine_for_choice(c, deadline, &self.context)
    }

    /// [`Katalogr::engine_for`] over the context `choice` names, not this invocation's own.
    fn engine_for_choice(
        &self,
        c: impl Into<EngineConsistency>,
        deadline: Instant,
        choice: &ContextChoice,
    ) -> Result<Engine, DistError> {
        let engine = self.open_for_choice(c, deadline, choice)?;
        self.note_missing_index(&engine);
        Ok(engine)
    }

    fn open_for_choice(
        &self,
        c: impl Into<EngineConsistency>,
        deadline: Instant,
        choice: &ContextChoice,
    ) -> Result<Engine, DistError> {
        let dir = self.layer_dir_for(choice, deadline)?;
        match c.into() {
            EngineConsistency::StaleOk => match self.reader_engine(dir.clone())? {
                Some(e) => Ok(e),
                None => self.engine_on(dir, deadline),
            },
            EngineConsistency::Wait | EngineConsistency::WaitAll => {
                let (roots, manifest) = self.layer_setup(&dir);
                match Engine::open_try(dir.clone(), build_registry(), roots, deadline) {
                    Ok(e) => self.configure_layer(e, manifest),
                    Err(EngineError::Store(StoreError::Busy)) => {
                        match self.reader_engine(dir.clone())? {
                            Some(e) => Ok(e),
                            None => self.engine_on(dir, deadline),
                        }
                    }
                    Err(e) => Err(e.into()),
                }
            }
        }
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
        let layer =
            self.layer_dir(Instant::now() + Duration::from_secs_f64(DEFAULT_TIMEOUT_SECS))?;
        let inbox = katalogr_engine::queue::Inbox::new(&layer);
        for rel in &rels {
            inbox.append(&NewEvent::file(repo, rel, kind))?;
        }
        Ok(rels.len())
    }

    /// Read the queue state without ingesting or processing (stale-ok).
    pub fn status(&self, opts: &Options) -> Result<Report, DistError> {
        let timeout = opts.timeout()?;
        let engine = self.engine_for(Consistency::StaleOk, Instant::now() + timeout)?;
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
        let queued = events.len();
        let mut report = self.read(&engine, consistency, timeout, queued, warnings)?;
        // FEAT-075 R-6: a full run that drained everything rewrites the cache sorted; any other
        // run does when the file has grown past the policy. Query processes never compact. The
        // engine is the selected context's own (a layer's file, never the base's).
        let drained_full = full && queued > 0 && report.pending == 0 && !report.stale;
        let due = drained_full
            || engine
                .cache()
                .compaction_due(&CompactPolicy::default())
                .unwrap_or(false);
        if due {
            match engine.compact_cache(CompactMode::preferred(), Instant::now() + timeout) {
                Ok((_, c)) => {
                    if let CompactOutcome::Skipped(why) = c.outcome {
                        report
                            .warnings
                            .push(format!("cache compaction skipped: {why}"));
                    }
                }
                Err(e) => report
                    .warnings
                    .push(format!("cache compaction skipped: {e}")),
            }
        } else {
            drop(engine);
        }
        // The base follows the default branch's commit, not the disk (FEAT-043 R-1).
        match self.advance_base(opts) {
            Ok(adv) => {
                report.queued += adv.queued;
                report.warnings.extend(adv.warnings);
            }
            Err(e) => report.warnings.push(format!("base advance: {e}")),
        }
        Ok(report)
    }

    /// Run `f` on one snapshot at the requested consistency (default: wait).
    pub(crate) fn query<T>(
        &self,
        opts: &Options,
        f: impl FnOnce(&ReadTx) -> Result<Outcome<T>, StoreError>,
    ) -> Result<Reply<T>, DistError> {
        self.query_traced(
            &Options {
                diagnostic: false,
                ..opts.clone()
            },
            |tx, _| f(tx),
        )
    }

    /// Like [`Katalogr::query`], with a collector; its trace comes back in `Reply::diagnostic`
    /// when `opts.diagnostic`.
    pub(crate) fn query_traced<T>(
        &self,
        opts: &Options,
        f: impl FnOnce(&ReadTx, &mut Diagnostic) -> Result<Outcome<T>, StoreError>,
    ) -> Result<Reply<T>, DistError> {
        let timeout = opts.timeout()?;
        let consistency = opts.consistency.unwrap_or(Consistency::Wait);
        let deadline = Instant::now() + timeout;
        if let Some(at) = opts.at.as_deref() {
            return match self.resolve_at(at, deadline)? {
                at::Resolved::Current => {
                    let engine = self.engine_for(consistency, deadline)?;
                    self.query_on_traced(&engine, opts, f)
                }
                at::Resolved::Context { choice, wanted } => {
                    // A recorded position is answered as recorded, whatever the checkout is now.
                    let engine = self.engine_for_choice(Consistency::StaleOk, deadline, &choice)?;
                    let o = Options {
                        consistency: Some(Consistency::StaleOk),
                        ..opts.clone()
                    };
                    let mut reply = self.query_on_traced(&engine, &o, f)?;
                    let r = &mut reply.report;
                    r.exact = !r.stale && r.positions.iter().any(|p| p == &wanted);
                    r.context = Some(match &choice {
                        ContextChoice::Named(id) => id.to_string(),
                        _ => "base".to_string(),
                    });
                    self.finish_report(&engine, &mut reply.report, opts.view.as_ref());
                    Ok(reply)
                }
                at::Resolved::Unavailable { wanted, available } => Ok(Reply {
                    outcome: Outcome::PositionUnavailable { wanted, available },
                    report: Report {
                        watermark: 0,
                        pending: 0,
                        stale: false,
                        timed_out: false,
                        queued: 0,
                        warnings: self.take_notes(),
                        positions: Vec::new(),
                        exact: false,
                        view_id: None,
                        context: None,
                        working_set: None,
                    },
                    diagnostic: None,
                    partial: Vec::new(),
                    folded: Vec::new(),
                }),
            };
        }
        let engine = self.engine_for(consistency, deadline)?;
        self.query_on_traced(&engine, opts, f)
    }

    /// Like `query`, on an engine the caller already opened, so one engine serves many reads.
    pub(crate) fn query_on<T>(
        &self,
        engine: &Engine,
        opts: &Options,
        f: impl FnOnce(&ReadTx) -> Result<Outcome<T>, StoreError>,
    ) -> Result<Reply<T>, DistError> {
        self.query_on_traced(
            engine,
            &Options {
                diagnostic: false,
                ..opts.clone()
            },
            |tx, _| f(tx),
        )
    }

    /// `query_on` with a collector.
    pub(crate) fn query_on_traced<T>(
        &self,
        engine: &Engine,
        opts: &Options,
        f: impl FnOnce(&ReadTx, &mut Diagnostic) -> Result<Outcome<T>, StoreError>,
    ) -> Result<Reply<T>, DistError> {
        let mut diag = if opts.diagnostic {
            Diagnostic::on_new()
        } else {
            Diagnostic::off()
        };
        let timeout = opts.timeout()?;
        let consistency = opts.consistency.unwrap_or(Consistency::Wait);
        let a = engine.with_consistency(
            &QueryOptions {
                consistency: consistency.into(),
                timeout,
                scope: Scope::Project,
            },
            |tx| f(tx, &mut diag),
        )?;
        Ok(Reply {
            outcome: a.value?,
            partial: Vec::new(),
            folded: Vec::new(),
            report: Report {
                watermark: a.watermark.0,
                pending: a.pending,
                stale: a.stale,
                timed_out: a.stale && consistency != Consistency::StaleOk,
                queued: 0,
                warnings: a.warnings,
                positions: a.positions,
                exact: a.exact,
                view_id: None,
                context: None,
                working_set: None,
            },
            diagnostic: diag.finish(),
        })
        .map(|mut reply| {
            self.finish_report(engine, &mut reply.report, opts.view.as_ref());
            reply
        })
    }

    /// A guard that clears `view_extra` when the question ends, however it ends.
    pub(crate) fn extra_scope(&self) -> ExtraScope<'_> {
        ExtraScope(&self.view_extra)
    }

    /// FEAT-084. Give an exact answer to a question its view ID. A snapshot that cannot be read
    /// leaves the ID off.
    pub(crate) fn finish_report(
        &self,
        engine: &Engine,
        report: &mut Report,
        view: Option<&katalogr_query::ViewQuery>,
    ) {
        report.view_id = None;
        let extra = std::mem::take(&mut *self.view_extra.lock().expect("view_extra lock"));
        let merged = view.map(|v| {
            let mut v = v.clone();
            v.args.extend(extra);
            v
        });
        let Some(view) = merged.as_ref() else { return };
        if !report.exact {
            return;
        }
        if let Ok(snap) = engine.cache().read()
            && let Ok(facts) = engine.facts_version(&snap)
        {
            report.view_id = Some(katalogr_query::view_id(&report.positions, &facts, view));
        }
    }

    /// Symbols whose name matches `text`.
    pub fn find(&self, text: &str, opts: &Options) -> Result<Reply<Vec<SymbolHit>>, DistError> {
        self.query(opts, |tx| {
            Ok(found_or_none(katalogr_query::find(tx, text)?, text))
        })
    }

    /// References to the symbol `text` names, through re-exports.
    pub fn refs(&self, text: &str, opts: &Options) -> Result<Reply<Vec<RefHit>>, DistError> {
        let fold_from = (!opts.expand).then_some(katalogr_query::FOLD_FROM);
        let mut folds = Vec::new();
        let mut reply = self.query_traced(opts, |tx, diag| {
            on_target_traced(tx, text, diag, |t, diag| {
                let f = katalogr_query::refs_folded_traced(tx, t, fold_from, diag)?;
                folds = f.folds;
                Ok(f.hits)
            })
        })?;
        reply.folded = folds;
        Ok(reply)
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
        let fold_from = (!opts.expand).then_some(katalogr_query::FOLD_FROM);
        let mut folds = Vec::new();
        let mut reply = self.query(opts, |tx| {
            on_target(tx, text, |t| {
                let a = katalogr_query::callers_folded(tx, t, depth, fold_from)?;
                if a.deeper {
                    partial.push(Partial::new(
                        cap::DEPTH,
                        u64::from(depth),
                        a.unfolded_len as u64,
                        None,
                    ));
                }
                folds = a.folded.folds;
                Ok(a.folded.hits)
            })
        })?;
        reply.partial = partial;
        reply.folded = folds;
        Ok(reply)
    }

    /// Tests of the symbol `text` names (FEAT-063): resolved as `callers` does.
    pub fn tests(
        &self,
        text: &str,
        topts: TestsOptions,
        opts: &Options,
    ) -> Result<Reply<Vec<TestHit>>, DistError> {
        let (deeper, depth) = one_deeper(topts);
        let mut partial = Vec::new();
        let mut reply = self.query(opts, |tx| {
            on_target(tx, text, |t| {
                let hits = katalogr_query::tests(tx, t, deeper)?;
                Ok(cut_depth(hits, depth, &mut partial))
            })
        })?;
        reply.partial = partial;
        Ok(reply)
    }

    /// Tests reaching any of the symbols `seeds` name, each once with its nearest seed
    /// (FEAT-063 R-10: the multi-seed query that impact analysis consumes).
    pub fn tests_for(
        &self,
        seeds: &[String],
        topts: TestsOptions,
        opts: &Options,
    ) -> Result<Reply<Vec<TestHit>>, DistError> {
        let (deeper, depth) = one_deeper(topts);
        let mut partial = Vec::new();
        let mut reply = self.query(opts, |tx| {
            let mut ids = Vec::new();
            for text in seeds {
                match katalogr_query::resolve(tx, text)? {
                    Resolution::One(t) => ids.push(katalogr_model::Id::of(&t.name)),
                    other => return Ok(unresolved(other, text)),
                }
            }
            let hits = katalogr_query::tests_for(tx, &ids, deeper)?;
            Ok(Outcome::Found(cut_depth(hits, depth, &mut partial)))
        })?;
        reply.partial = partial;
        Ok(reply)
    }

    /// The symbol at `file:line` of the current repo (`file` relative to the repo root) and its
    /// rationale links. With `use_limit` `Some(n)`, also who uses the symbol (at most `n`
    /// callers and tests), read in the same snapshot; `None` leaves `used_by` out.
    pub fn why(
        &self,
        spec: &str,
        use_limit: Option<usize>,
        opts: &Options,
    ) -> Result<Reply<WhyResult>, DistError> {
        if matches!(
            katalogr_model::parse_name(spec),
            Ok(katalogr_model::AnyName::Contract(_))
        ) {
            return Err(DistError::usage(format!(
                "{spec}: a contract node has no file or line; `why` takes FILE:LINE (use `katalogr explain` for a contract)"
            )));
        }
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
        let mut partial = Vec::new();
        let mut reply = self.query(opts, |tx| {
            let mut out =
                from_resolution(katalogr_query::why(tx, repo, &path, line, &blame)?, spec);
            if let (Some(limit), Outcome::Found(w)) = (use_limit, &mut out) {
                let used = katalogr_query::why_use(tx, &w.symbol, limit)?;
                for (shown, total) in [
                    (used.callers.len(), used.callers_total),
                    (used.tests.len(), used.tests_total),
                ] {
                    if total > shown as u64 {
                        partial.push(Partial::new(
                            cap::LIMIT,
                            limit as u64,
                            shown as u64,
                            Some(total),
                        ));
                    }
                }
                katalogr_query::attach_use(w, used);
            }
            Ok(out)
        })?;
        reply.partial = partial;
        Ok(reply)
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
        let _extra = self.extra_scope();
        if !opts.diagnostic && opts.at.is_none() {
            return self.map_stored(opts);
        }
        let mut ws = WsRun::default();
        let mut reply = self.query_traced(opts, |tx, diag| {
            let boost = self.ws_boost(opts, tx, &mut ws);
            Ok(Outcome::Found(katalogr_query::map_boosted_traced(
                tx,
                boost.as_ref(),
                diag,
            )?))
        })?;
        ws.finish(&mut reply.report);
        Ok(reply)
    }

    /// `map` through the stored answers. The working set that boosts the ranking is part of the
    /// question, so it joins the key; it is read at most once, whichever path asks first.
    fn map_stored(&self, opts: &Options) -> Result<Reply<Vec<MapEntry>>, DistError> {
        struct Boosted {
            read: bool,
            boost: Option<katalogr_query::Boost>,
            ws: WsRun,
        }
        let state = std::cell::RefCell::new(Boosted {
            read: false,
            boost: None,
            ws: WsRun::default(),
        });
        let read = |tx: &ReadTx| {
            let mut st = state.borrow_mut();
            if !st.read {
                st.read = true;
                let mut ws = WsRun::default();
                st.boost = self.ws_boost(opts, tx, &mut ws);
                st.ws = ws;
            }
        };
        let mut reply = self.stored(
            opts,
            &answers::StoredQuery::Map,
            |tx| {
                read(tx);
                // What `ws_boost` added to the question joins the key.
                let args = self
                    .view_extra
                    .lock()
                    .expect("view_extra lock")
                    .iter()
                    .cloned()
                    .collect();
                Some(answers::Extra {
                    inputs: BTreeMap::new(),
                    args,
                })
            },
            |tx| {
                read(tx);
                let st = state.borrow();
                Ok(Outcome::Found(katalogr_query::map_boosted_traced(
                    tx,
                    st.boost.as_ref(),
                    &mut Diagnostic::off(),
                )?))
            },
        )?;
        state.into_inner().ws.finish(&mut reply.report);
        Ok(reply)
    }

    /// The working-set boost for a `search` or `map` read, or `None` when ranking is neutral,
    /// the read is `--at`, or the set is empty. Records the set's summary and view-ID arguments
    /// in `run` and in `view_extra`.
    fn ws_boost(
        &self,
        opts: &Options,
        tx: &ReadTx,
        run: &mut WsRun,
    ) -> Option<katalogr_query::Boost> {
        if opts.ranking != Ranking::Default || opts.at.is_some() {
            return None;
        }
        let ((set, warnings), item) = self.working_set_with(tx);
        run.warnings = warnings;
        let boost = katalogr_query::Boost::new(&set, self.working_set_weights())?;
        let mut summary = set.counts();
        summary.item = item;
        run.summary = Some(summary);
        self.view_extra.lock().expect("view_extra lock").extend([
            ("ranking".to_string(), "working-set".to_string()),
            ("working_set".to_string(), set.hash()),
        ]);
        Some(boost)
    }

    /// The best `limit` symbols for the words in `terms` (BM25 over names, docs, sections and
    /// contract keys). No word is a usage error; no hit is no match.
    pub fn search(
        &self,
        terms: &str,
        limit: usize,
        opts: &Options,
    ) -> Result<Reply<Vec<SearchHit>>, DistError> {
        self.search_with(terms, limit, &SearchParams::default(), opts)
    }

    /// `search` with explicit BM25F parameters (same outcome rules as `search`).
    pub fn search_with(
        &self,
        terms: &str,
        limit: usize,
        p: &SearchParams,
        opts: &Options,
    ) -> Result<Reply<Vec<SearchHit>>, DistError> {
        if katalogr_model::tokenize(terms).is_empty() {
            return Err(DistError::usage(format!(
                "{terms:?}: no searchable word (words need two letters, or a digit)"
            )));
        }
        // One extra hit tells whether the limit cut the answer.
        let mut partial = Vec::new();
        let _extra = self.extra_scope();
        let mut ws = WsRun::default();
        let mut reply = self.query_traced(opts, |tx, diag| {
            let boost = self.ws_boost(opts, tx, &mut ws);
            let mut hits = katalogr_query::search_boosted_traced(
                tx,
                terms,
                limit.saturating_add(1),
                p,
                boost.as_ref(),
                diag,
            )?;
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
        ws.finish(&mut reply.report);
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

    /// How many callers and tests `why` lists: the project setting `why.use_limit`, else
    /// [`DEFAULT_USE_LIMIT`]. A missing, malformed or zero value keeps the default.
    pub fn why_use_limit(&self) -> usize {
        self.project
            .config()
            .settings
            .get("why.use_limit")
            .and_then(serde_yaml_ng::Value::as_u64)
            .filter(|n| *n > 0)
            .map_or(DEFAULT_USE_LIMIT, |n| n as usize)
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

    /// A read of a whole-graph command through the stored answers (FEAT-096). A read at another
    /// position (`--at`) is computed as before.
    fn stored<T: serde::Serialize + serde::de::DeserializeOwned>(
        &self,
        o: &Options,
        q: &answers::StoredQuery,
        extra: impl FnOnce(&ReadTx) -> Option<answers::Extra>,
        f: impl FnOnce(&ReadTx) -> Result<Outcome<T>, StoreError>,
    ) -> Result<Reply<T>, DistError> {
        if o.at.is_some() {
            return self.query(o, f);
        }
        let timeout = o.timeout()?;
        let consistency = o.consistency.unwrap_or(Consistency::Wait);
        let engine = self.engine_for(consistency, Instant::now() + timeout)?;
        self.query_stored(&engine, o, q, extra, f)
    }

    /// Hubs: the nodes with the highest weighted degree.
    pub fn hubs(&self, opts: &HubOptions, o: &Options) -> Result<Reply<HubReport>, DistError> {
        let mut reply = self.stored(
            o,
            &answers::StoredQuery::Hubs(opts.clone()),
            |_| Some(answers::Extra::default()),
            |tx| Ok(Outcome::Found(katalogr_analytics::hubs(tx, opts)?)),
        )?;
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
        let mut reply = self.stored(
            o,
            &answers::StoredQuery::Communities(opts.clone()),
            |_| Some(answers::Extra::default()),
            |tx| Ok(Outcome::Found(katalogr_analytics::communities(tx, opts)?)),
        )?;
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
        let mut reply = self.stored(
            o,
            &answers::StoredQuery::Cycles(opts.clone()),
            |_| Some(answers::Extra::default()),
            |tx| Ok(Outcome::Found(katalogr_analytics::cycles(tx, opts)?)),
        )?;
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

    /// How complete the graph is: references resolved, inferred and unresolved, dangling
    /// contracts, parse errors and unclaimed files, per repository, extension and language.
    /// Files are routed as the indexer routes them: the profiles are loaded once and a profile
    /// `select` claims a file before the registry does (census and `claim` alike). Walks each repository root once, then reads one snapshot at `o`'s consistency (default
    /// wait). The walk is not transactional with the index: a file created in between can
    /// show as unclaimed while its artifact also exists; the next run settles it.
    pub fn coverage(
        &self,
        opts: &CoverageOptions,
        o: &Options,
    ) -> Result<Reply<CoverageReport>, DistError> {
        let repos: Vec<&str> = self
            .project
            .config()
            .repos
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        if let Some(r) = &opts.repo
            && !repos.contains(&r.as_str())
        {
            return Err(DistError::usage(format!(
                "repo {r} is not part of this project (repos: {})",
                repos.join(", ")
            )));
        }
        let timeout = o.timeout()?;
        let consistency = o.consistency.unwrap_or(Consistency::Wait);
        let engine = self.engine_for(consistency, Instant::now() + timeout)?;
        let parts = self.coverage_parts(&engine, opts)?;
        let run = |tx: &ReadTx| Ok(Outcome::Found(coverage_run(&engine, opts, &parts, tx)?));
        if o.at.is_some() {
            return self.query_on(&engine, o, run);
        }
        // The census walks the disk, so the listing it saw is an input of the stored answer.
        let named: Vec<String> = self
            .project
            .config()
            .repos
            .iter()
            .filter(|r| opts.repo.as_deref().is_none_or(|w| w == r.name))
            .map(|r| r.name.clone())
            .collect();
        let listing = answers::listing_digest(&engine, &named)?;
        self.query_stored(
            &engine,
            o,
            &answers::StoredQuery::Coverage(opts.clone()),
            |_| {
                Some(answers::Extra {
                    inputs: BTreeMap::from([("listing".to_string(), listing)]),
                    args: BTreeMap::new(),
                })
            },
            run,
        )
    }

    /// The disk side of a coverage report: the profiles, the files no extension claims and the
    /// repos with no root here. Walks each repository root once.
    pub(crate) fn coverage_parts(
        &self,
        engine: &Engine,
        opts: &CoverageOptions,
    ) -> Result<CoverageParts, DistError> {
        let profiles = engine.routing_profiles()?;
        let mut unclaimed = BTreeMap::new();
        let mut roots_missing = Vec::new();
        for r in &self.project.config().repos {
            let repo = r.name.as_str();
            if opts.repo.as_deref().is_some_and(|w| w != repo) {
                continue;
            }
            match katalogr_engine::census::census_with(engine, repo, &profiles)? {
                Some(c) => {
                    unclaimed.insert(repo.to_string(), c.unclaimed);
                }
                None => roots_missing.push(repo.to_string()),
            }
        }
        Ok(CoverageParts {
            profiles,
            unclaimed,
            roots_missing,
        })
    }

    /// Dead-code candidates. Fills `opts.code_kinds` from the registered extensions first.
    pub fn dead_code(
        &self,
        mut opts: DeadCodeOptions,
        o: &Options,
    ) -> Result<Reply<DeadCodeReport>, DistError> {
        opts.code_kinds = registry_code_kinds();
        // The question as asked: the code kinds come from the registry, which the build covers.
        let asked = answers::StoredQuery::DeadCode(DeadCodeOptions {
            code_kinds: BTreeMap::new(),
            ..opts.clone()
        });
        let mut reply = self.stored(
            o,
            &asked,
            |_| Some(answers::Extra::default()),
            |tx| Ok(Outcome::Found(katalogr_analytics::dead_code(tx, &opts)?)),
        )?;
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
        warnings.extend(a.warnings);
        warnings.extend(self.take_notes());
        Ok(Report {
            watermark: a.watermark.0,
            pending: a.pending,
            stale: a.stale,
            timed_out: a.stale && consistency != Consistency::StaleOk,
            queued,
            warnings,
            positions: a.positions,
            exact: a.exact,
            view_id: None,
            context: None,
            working_set: None,
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

    /// The built-ins load with the registered normalisers, and `generic` stays version 1.
    #[test]
    fn builtin_profiles_load_with_normalisers() {
        let dir = std::env::temp_dir();
        let dir = camino::Utf8Path::from_path(&dir).unwrap();
        let r = katalogr_engine::profiles::ProfileSet::load_full(
            &dir.join("katalogr-no-such-project"),
            super::BUILTIN_PROFILES,
            &Default::default(),
            &super::build_registry().normalisers(),
        );
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.set.names(), ["generic", "http-rust", "http-ts"]);
        let generic = katalogr_engine::profiles::parse(
            "builtin:generic.yaml",
            super::builtin_profile("generic").unwrap(),
        )
        .unwrap();
        assert_eq!(generic.version, 1);
    }
}

#[cfg(test)]
mod dist {
    /// A file a profile `select` routes to the text extension is claimed by it (and its
    /// language), not left to the registry.
    #[test]
    fn claim_follows_routes() {
        use katalogr_engine::profiles::ProfileSet;
        let registry = super::build_registry();
        let select = "profile: go-text\nversion: 2\nselect:\n  - extension: text\n    path: 'svc/*.go'\nrules: []\n";
        let dir = std::env::temp_dir();
        let dir = camino::Utf8Path::from_path(&dir).unwrap();
        let none = dir.join("katalogr-no-such-project");
        let with = ProfileSet::load_full(
            &none,
            &[("go-text.yaml", select)],
            &Default::default(),
            &registry.normalisers(),
        );
        assert!(with.errors.is_empty(), "{:?}", with.errors);
        let path = katalogr_model::RelPath::new("svc/run.go").unwrap();
        assert_eq!(
            super::claim_for(&registry, &with.set, &path),
            Some(("text".to_string(), "text".to_string()))
        );
        let without = ProfileSet::load_full(&none, &[], &Default::default(), &[]);
        assert_eq!(super::claim_for(&registry, &without.set, &path), None);
    }

    /// Two extensions providing a normaliser of the same id would make the second unreachable
    /// (the first registered wins); the build refuses that instead.
    #[test]
    fn normaliser_ids_unique() {
        let registry = super::build_registry();
        let mut ids: Vec<&str> = registry
            .extractors()
            .flat_map(|e| e.key_normalisers().iter().map(|n| n.id()))
            .collect();
        assert!(ids.contains(&"http_route"), "{ids:?}");
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "a normaliser id is provided twice");
        assert_eq!(registry.normalisers().len(), n);
    }
}

#[cfg(test)]
mod eval_runner {
    use crate::eval::explain_items;
    use katalogr_model::Confidence;
    use katalogr_query::{Explain, LinkBrief, SymbolHit};

    fn link(other: &str) -> LinkBrief {
        LinkBrief {
            kind: "calls".to_string(),
            other: other.to_string(),
            confidence: Confidence::Extracted,
            origin: None,
        }
    }

    #[test]
    fn explain_items_dedup() {
        let e = Explain {
            symbol: SymbolHit {
                name: "app:src/a.rs#fn:a".to_string(),
                kind: "fn".to_string(),
                path: "src/a.rs".to_string(),
                span: katalogr_model::Span {
                    start_line: 1,
                    start_col: 1,
                    end_line: 1,
                    end_col: 1,
                },
            },
            signature: None,
            doc: None,
            outgoing: vec![link("x"), link("y")],
            outgoing_total: 2,
            incoming: vec![link("y"), link("z")],
            incoming_total: 2,
            items: vec!["x".to_string(), "item".to_string()],
            contract: None,
        };
        let names: Vec<String> = explain_items(&e).into_iter().map(|i| i.name).collect();
        assert_eq!(names, ["x", "y", "z", "item"]);
    }
}

/// `topts` with one more level, and the depth the caller asked for: the extra level tells
/// whether the depth cut the answer.
fn one_deeper(topts: TestsOptions) -> (TestsOptions, u32) {
    let depth = topts.depth.max(1);
    (
        TestsOptions {
            depth: depth.saturating_add(1),
            ..topts
        },
        depth,
    )
}

/// Keep the hits within `depth`; a hit beyond it records the depth cap in `partial`.
fn cut_depth(mut hits: Vec<TestHit>, depth: u32, partial: &mut Vec<Partial>) -> Vec<TestHit> {
    if hits.iter().any(|h| h.depth > depth) {
        hits.retain(|h| h.depth <= depth);
        partial.push(Partial::new(
            cap::DEPTH,
            u64::from(depth),
            hits.len() as u64,
            None,
        ));
    }
    hits
}
