//! Per-query consistency: how far the index must have caught up before a query reads it
//! (FEAT-047 sections 3 and 4; ADR-0008, ADR-0014).

use crate::error::EngineError;
use crate::parallel::Parallelism;
use crate::profiles::{self, LoadReport, ProfileSet};
use crate::queue::{Context, Inbox, Processor, Seq};
use crate::registry::ExtensionRegistry;
use crate::stitch::Stitcher;
use camino::Utf8PathBuf;
use katalogr_model::RelPath;
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
    /// Rank computations since the engine opened (ADR-0033: one per drained queue).
    pub rank_refreshes: u64,
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
        Ok(Engine::with_cache(dir, cache, stitcher, registry, roots))
    }

    /// FEAT-080 R-3. As [`Engine::open`], but a cache held by another writer fails at once with
    /// `StoreError::Busy` instead of waiting; a free cache is opened and synced against `deadline`.
    pub fn open_try(
        dir: ProjectDir,
        registry: ExtensionRegistry,
        roots: BTreeMap<String, Utf8PathBuf>,
        deadline: Instant,
    ) -> Result<Engine, EngineError> {
        let stitcher = Stitcher::new();
        let cache = Cache::open_or_rebuild_busy(&dir, &stitcher, Instant::now(), deadline)?;
        Ok(Engine::with_cache(dir, cache, stitcher, registry, roots))
    }

    /// FEAT-080 R-1. As [`Engine::open`] on a read-only handle: never writes, syncs or replays,
    /// and takes no lock. `Ok(None)` when the cache cannot be read that way (no file, other
    /// schema, tables not yet created, repair needed or running, a corrupt header, or a target
    /// without byte-range locks); the caller then uses [`Engine::open`].
    pub fn open_reader(
        dir: ProjectDir,
        registry: ExtensionRegistry,
        roots: BTreeMap<String, Utf8PathBuf>,
    ) -> Result<Option<Engine>, EngineError> {
        let Some(cache) = Cache::open_reader(&dir)? else {
            return Ok(None);
        };
        Ok(Some(Engine::with_cache(
            dir,
            cache,
            Stitcher::new(),
            registry,
            roots,
        )))
    }

    fn with_cache(
        dir: ProjectDir,
        cache: Cache,
        stitcher: Stitcher,
        registry: ExtensionRegistry,
        roots: BTreeMap<String, Utf8PathBuf>,
    ) -> Engine {
        Engine {
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
        }
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
        g.rank_refreshes += s.rank_refreshes;
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
            && !self.cache.is_reader()
            && let Some(mut p) = Processor::try_acquire(self)?
        {
            // A changed profile queues the files it can affect before pending is counted.
            p.refresh_embed_signature()?;
            p.refresh_profiles()?;
        }
        loop {
            let snap = self.cache.read()?;
            let pending = self.pending_in(&snap, &scope)?;
            let finished =
                o.consistency == Consistency::StaleOk || pending == 0 || Instant::now() >= deadline;
            if finished {
                let watermark = Seq(snap.watermark(Context::WORKTREE)?);
                return Ok(Answer {
                    value: f(&snap),
                    watermark,
                    pending,
                    stale: pending > 0,
                });
            }
            drop(snap);
            let mut held = Processor::try_acquire(self)?;
            if self.cache.is_reader() {
                // FEAT-080 R-4. Another process holds the writer; take over only when it is gone.
                held = match held {
                    Some(p)
                        if self
                            .cache
                            .try_upgrade(&self.dir, &self.stitcher, deadline)? =>
                    {
                        let mut p = p;
                        // What a reader skipped at the start.
                        p.refresh_embed_signature()?;
                        p.refresh_profiles()?;
                        Some(p)
                    }
                    _ => None,
                };
            }
            match held {
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
}
