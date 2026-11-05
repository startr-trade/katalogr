//! Per-query consistency: how far the index must have caught up before a query reads it
//! (FEAT-047 sections 3 and 4; ADR-0008, ADR-0014).

use crate::error::EngineError;
use crate::parallel::Parallelism;
use crate::position::{WorkingState, read_position_cached, stat_cache_path};
use crate::profiles::{self, LoadReport, ProfileSet};
use crate::queue::{Context, EventKind, Inbox, NewEvent, Processor, Seq};
use crate::registry::ExtensionRegistry;
use crate::stitch::Stitcher;
use camino::Utf8PathBuf;
use katalogr_model::RelPath;
use katalogr_query::PositionRef;
use katalogr_store::{Cache, ProjectDir, ReadTx, Reader, RepoConfig};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Paths published per processing round (provisional; FEAT-047 section 9).
pub const DEFAULT_BUDGET: usize = 256;
const POLL_START: Duration = Duration::from_millis(10);
const POLL_MAX: Duration = Duration::from_millis(250);

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Consistency {
    /// Return once nothing queued intersects the scope.
    Wait,
    /// Return once nothing at all is queued.
    WaitAll,
    /// Read the current snapshot; never ingest or process.
    StaleOk,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Intersects everything.
    Project,
    /// `(repo, path)` pairs.
    Paths(BTreeSet<(String, RelPath)>),
}

impl Scope {
    pub fn contains(&self, repo: &str, path: &RelPath) -> bool {
        match self {
            Scope::Project => true,
            Scope::Paths(set) => set.contains(&(repo.to_string(), path.clone())),
        }
    }
}

#[derive(Clone, Debug)]
pub struct QueryOptions {
    pub consistency: Consistency,
    pub timeout: Duration,
    pub scope: Scope,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer<T> {
    pub value: T,
    /// The `worktree` watermark of the snapshot the value was read from.
    pub watermark: Seq,
    /// Queued changes intersecting the query's scope at snapshot time (queue rows plus inbox
    /// files not yet ingested).
    pub pending: u64,
    /// True if and only if `pending > 0` at snapshot time.
    pub stale: bool,
    /// What the index reflects, one entry per repo, sorted by repo (FEAT-084). On an exact
    /// answer it is the checkout's position; otherwise the last position reached exactly, and
    /// a repo that never reached one is absent.
    pub positions: Vec<PositionRef>,
    /// Nothing was pending, and every checkout's position was the same before and after the
    /// read. Always false for `StaleOk`.
    pub exact: bool,
    /// Non-fatal notices, such as a checkout whose position could not be read.
    pub warnings: Vec<String>,
}

/// Meta key prefix of the last position a repo reached exactly: compact JSON of a `PositionRef`.
pub const POSITION_META: &str = "position.";
/// Meta key prefix of the uncommitted paths of that position: JSON of [`ChangedSet`].
pub const POSITION_CHANGED_META: &str = "position_changed.";
/// Uncommitted paths remembered per repo; above it a rescan is queued instead.
pub const CHANGED_CAP: usize = 10_000;

/// The uncommitted paths of a recorded position, capped at [`CHANGED_CAP`].
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChangedSet {
    /// True when the set was over the cap and `paths` is empty.
    pub over: bool,
    pub paths: Vec<String>,
}

/// Per-stage times summed over an engine's processing rounds; for benches only.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default)]
pub struct IndexStats {
    pub rounds: u64,
    pub files: u64,
    /// Plan, encode, hash and temp-file write (the parallel stages).
    pub prepare: Duration,
    /// The stitch loop inside the publish transaction.
    pub stitch: Duration,
    pub commit: Duration,
}

/// A project's index with its queue: cache, extractors and stitcher.
pub struct Engine {
    dir: ProjectDir,
    cache: Cache,
    registry: ExtensionRegistry,
    stitcher: Stitcher,
    roots: BTreeMap<String, Utf8PathBuf>,
    /// Per repo `(include, exclude)` globs; a repo not listed uses the defaults.
    filters: BTreeMap<String, (Vec<String>, Vec<String>)>,
    /// Embedded profiles: `(file name, text)`, applied before the project's own.
    builtin_profiles: Vec<(String, String)>,
    parallelism: Parallelism,
    /// Built on first use with `parallelism.threads` workers; `None` when sequential or when
    /// the pool cannot be built.
    pool: OnceLock<Option<rayon::ThreadPool>>,
    stats: Mutex<IndexStats>,
}

impl Engine {
    /// Open the cache (replaying an interrupted publish and syncing it with the shards).
    /// `roots` maps each repo name to its working-tree root.
    pub fn open(
        dir: ProjectDir,
        registry: ExtensionRegistry,
        roots: BTreeMap<String, Utf8PathBuf>,
        deadline: Instant,
    ) -> Result<Engine, EngineError> {
        let stitcher = Stitcher::new();
        let cache = Cache::open_or_rebuild(&dir, &stitcher, deadline)?;
        Ok(Engine {
            dir,
            cache,
            registry,
            stitcher,
            roots,
            filters: BTreeMap::new(),
            builtin_profiles: Vec::new(),
            parallelism: Parallelism::from_cores(),
            pool: OnceLock::new(),
            stats: Mutex::new(IndexStats::default()),
        })
    }

    /// Set before the first `process`; later calls replace the setting and drop a built pool.
    pub fn set_parallelism(&mut self, p: Parallelism) {
        self.parallelism = p;
        self.pool = OnceLock::new();
    }

    pub fn parallelism(&self) -> Parallelism {
        self.parallelism
    }

    /// The pool, built on first use with `threads` workers named `katalogr-index-<i>`; `None`
    /// when `threads == 1` or when the pool cannot be built (then the run is sequential).
    pub(crate) fn pool(&self) -> Option<&rayon::ThreadPool> {
        self.pool
            .get_or_init(|| {
                let n = self.parallelism.threads.get();
                if n <= 1 {
                    return None;
                }
                rayon::ThreadPoolBuilder::new()
                    .num_threads(n)
                    .thread_name(|i| format!("katalogr-index-{i}"))
                    .build()
                    .ok()
            })
            .as_ref()
    }

    /// Per-stage times summed over this engine's rounds; for benches only.
    #[doc(hidden)]
    pub fn index_stats(&self) -> IndexStats {
        *self.stats.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn add_stats(&self, s: IndexStats) {
        let mut g = self.stats.lock().unwrap_or_else(|e| e.into_inner());
        g.rounds += s.rounds;
        g.files += s.files;
        g.prepare += s.prepare;
        g.stitch += s.stitch;
        g.commit += s.commit;
    }

    /// Set the built-in profiles, as `(file name, text)` pairs.
    pub fn set_builtin_profiles(&mut self, profiles: Vec<(String, String)>) {
        self.builtin_profiles = profiles;
    }

    /// The built-in profiles, as `(file name, text)` pairs.
    pub fn builtin_profiles(&self) -> &[(String, String)] {
        &self.builtin_profiles
    }

    /// Set the include and exclude globs used to list a repo's files (default: those of
    /// `RepoConfig::new`).
    pub fn set_repo_filters(&mut self, repo: &str, include: Vec<String>, exclude: Vec<String>) {
        self.filters.insert(repo.to_string(), (include, exclude));
    }

    /// The `(include, exclude)` globs of a repo.
    pub fn repo_filters(&self, repo: &str) -> (Vec<String>, Vec<String>) {
        self.filters.get(repo).cloned().unwrap_or_else(|| {
            let d = RepoConfig::new(repo);
            (d.include, d.exclude)
        })
    }

    /// The profiles the processor would apply now: the built-ins, then the project's
    /// `profiles/*.yaml`, a refused file replaced by its last good text (meta `profiles_good`),
    /// with the registry's normalisers. Reads the cache and the disk; writes nothing.
    pub fn routing_profiles(&self) -> Result<ProfileSet, EngineError> {
        let snap = self.cache.read()?;
        Ok(self.load_profiles(&snap)?.set)
    }

    /// Load the profiles against the last good texts stored in `snap`.
    pub(crate) fn load_profiles(&self, snap: &ReadTx) -> Result<LoadReport, EngineError> {
        let previous: BTreeMap<String, String> = snap
            .meta(profiles::META_GOOD)?
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let builtins: Vec<(&str, &str)> = self
            .builtin_profiles()
            .iter()
            .map(|(n, t)| (n.as_str(), t.as_str()))
            .collect();
        let normalisers = self.registry().normalisers();
        Ok(ProfileSet::load_full(
            self.dir().root(),
            &builtins,
            &previous,
            &normalisers,
        ))
    }

    pub fn dir(&self) -> &ProjectDir {
        &self.dir
    }

    pub fn cache(&self) -> &Cache {
        &self.cache
    }

    pub fn registry(&self) -> &ExtensionRegistry {
        &self.registry
    }

    pub fn stitcher(&self) -> &Stitcher {
        &self.stitcher
    }

    /// Forwards to `Stitcher::set_fan_rows_from` (tests and benches only).
    #[doc(hidden)]
    pub fn set_fan_rows_from(&self, n: u32) {
        self.stitcher.set_fan_rows_from(n);
    }

    pub fn roots(&self) -> &BTreeMap<String, Utf8PathBuf> {
        &self.roots
    }

    pub fn inbox(&self) -> Inbox {
        Inbox::new(&self.dir)
    }

    /// Queued changes intersecting `scope`, as seen from `snap` plus the inbox.
    fn pending_in(&self, snap: &ReadTx, scope: &Scope) -> Result<u64, EngineError> {
        let mut n = snap
            .pending_entries(None)?
            .iter()
            .filter(|(k, _)| scope.contains(&k.repo, &k.path))
            .count() as u64;
        let inbox = self.inbox();
        let ingested = |id: &str| snap.seen_seq(id);
        for file in inbox.list()? {
            let Ok(ev) = inbox.read(&file) else {
                continue;
            };
            if ingested(&ev.id)?.is_some() {
                continue;
            }
            let hit = match (&ev.path, scope) {
                (_, Scope::Project) | (None, _) => true,
                (Some(p), s) => s.contains(&ev.repo, p),
            };
            n += u64::from(hit);
        }
        Ok(n)
    }

    /// R-5..R-10. Bring the index to the state the options ask for, then run `f` on one read
    /// snapshot. `StaleOk` never ingests or processes. `Wait` returns once nothing queued
    /// intersects the scope, `WaitAll` once nothing is queued. When the processor lock is
    /// free the call ingests and processes (scope first) itself; otherwise it polls with
    /// backoff (10 ms doubling to 250 ms). At the deadline the snapshot is returned stale.
    pub fn with_consistency<T>(
        &self,
        o: &QueryOptions,
        f: impl FnOnce(&ReadTx) -> T,
    ) -> Result<Answer<T>, EngineError> {
        let deadline = Instant::now() + o.timeout;
        let scope = match o.consistency {
            Consistency::WaitAll => Scope::Project,
            _ => o.scope.clone(),
        };
        let mut backoff = POLL_START;
        if o.consistency != Consistency::StaleOk
            && let Some(mut p) = Processor::try_acquire(self)?
        {
            // A changed profile queues the files it can affect before pending is counted.
            p.refresh_embed_signature()?;
            p.refresh_profiles()?;
        }
        let reconcile = o.consistency != Consistency::StaleOk;
        let mut warnings = Vec::new();
        let mut unrecorded = BTreeSet::new();
        let before = if reconcile {
            self.reconcile(&mut warnings, &mut unrecorded)?
        } else {
            BTreeMap::new()
        };
        loop {
            let snap = self.cache.read()?;
            let pending = self.pending_in(&snap, &scope)?;
            let finished =
                o.consistency == Consistency::StaleOk || pending == 0 || Instant::now() >= deadline;
            if finished {
                let watermark = Seq(snap.watermark(Context::WORKTREE)?);
                let recorded = self.recorded_positions(&snap)?;
                // Exactness needs nothing pending anywhere, not only in the scope.
                let all_pending = if scope == Scope::Project {
                    pending
                } else {
                    self.pending_in(&snap, &Scope::Project)?
                };
                let value = f(&snap);
                drop(snap);
                // Read after the value, so an edit made while it was computed is seen.
                let (positions, exact) = self.settle(
                    reconcile && unrecorded.is_empty(),
                    all_pending == 0,
                    &before,
                    recorded,
                    &mut warnings,
                )?;
                return Ok(Answer {
                    value,
                    watermark,
                    pending,
                    stale: pending > 0,
                    positions,
                    exact,
                    warnings,
                });
            }
            drop(snap);
            match Processor::try_acquire(self)? {
                Some(mut p) => {
                    p.recover()?;
                    p.ingest()?;
                    p.process(&scope, DEFAULT_BUDGET, deadline)?;
                    backoff = POLL_START;
                }
                None => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    std::thread::sleep(backoff.min(left));
                    backoff = (backoff * 2).min(POLL_MAX);
                }
            }
        }
    }

    /// The last position each repo reached exactly, from `snap`, sorted by repo.
    fn recorded_positions(
        &self,
        snap: &ReadTx,
    ) -> Result<BTreeMap<String, PositionRef>, EngineError> {
        let mut out = BTreeMap::new();
        for repo in self.roots.keys() {
            if let Some(p) = recorded_position(snap, repo)? {
                out.insert(repo.clone(), p);
            }
        }
        Ok(out)
    }

    fn state_of(&self, repo: &str) -> Result<WorkingState, crate::scan::ScanError> {
        let root = &self.roots[repo];
        let (include, exclude) = self.repo_filters(repo);
        let cache = stat_cache_path(self.dir.cache_dir().as_std_path(), repo);
        read_position_cached(repo, root.as_std_path(), &include, &exclude, Some(&cache))
    }

    /// FEAT-084 reconcile, steps 1 and 2. Reads each checkout's position, and where it differs
    /// from the recorded one, queues events for what differs, so that a change no hook saw is
    /// still found. Returns the positions read (a repo whose status failed is left out, with a
    /// warning).
    fn reconcile(
        &self,
        warnings: &mut Vec<String>,
        unrecorded: &mut BTreeSet<String>,
    ) -> Result<BTreeMap<String, WorkingState>, EngineError> {
        let snap = self.cache.read()?;
        let mut before = BTreeMap::new();
        for repo in self.roots.keys() {
            let state = match self.state_of(repo) {
                Ok(s) => s,
                Err(e) => {
                    warnings.push(format!("position of {repo} unreadable: {e}"));
                    continue;
                }
            };
            let recorded = recorded_position(&snap, repo)?;
            // With no record, a rescan is the only way to know what the index reflects. Queued
            // changes mean a producer is already feeding the index, so none is added then.
            let unknown_but_fed =
                recorded.is_none() && self.pending_in(&snap, &Scope::Project)? > 0;
            if unknown_but_fed {
                // Nothing says what the index reflects, so the answer cannot be exact yet.
                unrecorded.insert(repo.clone());
            }
            if recorded.as_ref() != Some(&state.position) && !unknown_but_fed {
                let old = recorded_changed(&snap, repo)?;
                for ev in self.events_for_difference(repo, recorded.as_ref(), &state, old) {
                    // A rescan already waiting in the inbox covers the same ground.
                    if ev.kind == EventKind::Rescan && self.rescan_waiting(repo)? {
                        continue;
                    }
                    self.inbox().append(&ev)?;
                }
            }
            before.insert(repo.clone(), state);
        }
        Ok(before)
    }

    /// True when the inbox holds a rescan of `repo` that no processor round has taken yet.
    fn rescan_waiting(&self, repo: &str) -> Result<bool, EngineError> {
        let inbox = self.inbox();
        for file in inbox.list()? {
            if let Ok(ev) = inbox.read(&file)
                && ev.kind == EventKind::Rescan
                && ev.repo == repo
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The events that bring the index from the `recorded` position to `now`: the paths that
    /// differ between the two commits, plus every path of either uncommitted set. A rescan when
    /// there is no record, the commits cannot be compared, or a set was over the cap.
    fn events_for_difference(
        &self,
        repo: &str,
        recorded: Option<&PositionRef>,
        now: &WorkingState,
        old: Option<ChangedSet>,
    ) -> Vec<NewEvent> {
        let rescan = || vec![NewEvent::rescan(repo)];
        let Some(recorded) = recorded else {
            return rescan();
        };
        let old = old.unwrap_or_default();
        if old.over || now.changed.len() > CHANGED_CAP {
            return rescan();
        }
        let mut paths: BTreeSet<String> = old.paths.into_iter().collect();
        paths.extend(now.changed.iter().map(|(p, _)| p.as_str().to_string()));
        match (&recorded.commit, &now.position.commit) {
            (Some(a), Some(b)) if a != b => {
                let root = self.roots[repo].as_std_path();
                match crate::scan::changed_paths(root, &format!("{a}..{b}")) {
                    Ok(changes) => {
                        use crate::scan::PathChange as C;
                        for c in changes {
                            match c {
                                C::Added(p) | C::Modified(p) | C::Deleted(p) => {
                                    paths.insert(p.as_str().to_string());
                                }
                                C::Renamed { from, to } => {
                                    paths.insert(from.as_str().to_string());
                                    paths.insert(to.as_str().to_string());
                                }
                            }
                        }
                    }
                    Err(_) => return rescan(),
                }
            }
            (a, b) if a != b => return rescan(),
            _ => {}
        }
        if paths.len() > CHANGED_CAP {
            return rescan();
        }
        let root = self.roots[repo].as_std_path();
        paths
            .into_iter()
            .filter_map(|p| {
                let rel = RelPath::new(&p).ok()?;
                let kind = if std::fs::symlink_metadata(root.join(&p)).is_ok() {
                    EventKind::Changed
                } else {
                    EventKind::Deleted
                };
                Some(NewEvent::file(repo, &rel, kind))
            })
            .collect()
    }

    /// FEAT-084 reconcile, steps 4 and 5, after the value is computed. An exact answer needs
    /// nothing pending and each position, read again, equal to the one read before. It then
    /// records the position (skipped when another process holds the processor lock; the next
    /// read records it). Otherwise it answers with the recorded positions and `exact: false`.
    fn settle(
        &self,
        reconcile: bool,
        nothing_pending: bool,
        before: &BTreeMap<String, WorkingState>,
        recorded: BTreeMap<String, PositionRef>,
        warnings: &mut Vec<String>,
    ) -> Result<(Vec<PositionRef>, bool), EngineError> {
        let not_exact = |recorded: BTreeMap<String, PositionRef>| {
            (recorded.into_values().collect::<Vec<_>>(), false)
        };
        if !reconcile || !nothing_pending || self.roots.is_empty() {
            return Ok(not_exact(recorded));
        }
        let mut after = BTreeMap::new();
        for (repo, state) in before {
            match self.state_of(repo) {
                Ok(s) if s.position == state.position => {
                    after.insert(repo.clone(), s);
                }
                Ok(_) => return Ok(not_exact(recorded)),
                Err(e) => {
                    warnings.push(format!("position of {repo} unreadable: {e}"));
                    return Ok(not_exact(recorded));
                }
            }
        }
        if after.len() != self.roots.len() {
            return Ok(not_exact(recorded));
        }
        let changed: Vec<&WorkingState> = after
            .values()
            .filter(|s| recorded.get(&s.position.repo) != Some(&s.position))
            .collect();
        if !changed.is_empty()
            && let Some(_lock) = Processor::try_acquire(self)?
        {
            let mut tx = self.cache.write()?;
            for s in changed {
                let repo = &s.position.repo;
                let json = serde_json::to_vec(&s.position)
                    .map_err(|e| EngineError::Queue(e.to_string()))?;
                tx.set_meta(&format!("{POSITION_META}{repo}"), &json)?;
                let set = if s.changed.len() > CHANGED_CAP {
                    ChangedSet {
                        over: true,
                        paths: Vec::new(),
                    }
                } else {
                    ChangedSet {
                        over: false,
                        paths: s
                            .changed
                            .iter()
                            .map(|(p, _)| p.as_str().to_string())
                            .collect(),
                    }
                };
                let json =
                    serde_json::to_vec(&set).map_err(|e| EngineError::Queue(e.to_string()))?;
                tx.set_meta(&format!("{POSITION_CHANGED_META}{repo}"), &json)?;
            }
            tx.commit()?;
        }
        Ok((after.into_values().map(|s| s.position).collect(), true))
    }
}

/// The position `repo` last reached exactly, if any.
pub fn recorded_position(
    snap: &impl Reader,
    repo: &str,
) -> Result<Option<PositionRef>, EngineError> {
    Ok(snap
        .meta(&format!("{POSITION_META}{repo}"))?
        .and_then(|b| serde_json::from_slice(&b).ok()))
}

fn recorded_changed(snap: &impl Reader, repo: &str) -> Result<Option<ChangedSet>, EngineError> {
    Ok(snap
        .meta(&format!("{POSITION_CHANGED_META}{repo}"))?
        .and_then(|b| serde_json::from_slice(&b).ok()))
}
