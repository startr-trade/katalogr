//! Per-query consistency: how far the index must have caught up before a query reads it
//! (FEAT-047 sections 3 and 4; ADR-0008, ADR-0014).

use crate::error::EngineError;
use crate::queue::{Context, Inbox, Processor, Seq};
use crate::registry::ExtensionRegistry;
use crate::stitch::Stitcher;
use camino::Utf8PathBuf;
use katalogr_model::RelPath;
use katalogr_store::{Cache, ProjectDir, ReadTx, Reader};
use std::collections::{BTreeMap, BTreeSet};
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

/// A project's index with its queue: cache, extractors and stitcher.
pub struct Engine {
    dir: ProjectDir,
    cache: Cache,
    registry: ExtensionRegistry,
    stitcher: Stitcher,
    roots: BTreeMap<String, Utf8PathBuf>,
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
        })
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
}
