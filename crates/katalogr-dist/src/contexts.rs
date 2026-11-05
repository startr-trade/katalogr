//! Contexts on the `Katalogr` facade: the base and its advance (FEAT-043).

use crate::{Consistency, DistError, Katalogr, Options, Outcome, Reply, Report};
use camino::Utf8Path;
use katalogr_analytics::context_diff::{ContextConflicts, ContextDiff};
use katalogr_engine::Engine;
use katalogr_engine::base::{branch_tip, default_branch, plan_base_advance, resolve_commit};
use katalogr_engine::queue::Inbox;
use katalogr_engine::queue::lock::lock_until;
use katalogr_engine::{EventKind, FileSource, NewEvent, scan::ScanError};
use katalogr_store::{CACHE_SCHEMA, Cache, CacheInspection};
use katalogr_store::{ContextId, ContextKind, ContextManifest, ProjectDir, Reader, seed_layer};
use std::time::Instant;

/// What `context list` shows about one context. No absolute path is ever part of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextInfo {
    pub id: String,
    /// `base`, `checkout` or `commit`.
    pub kind: String,
    pub repo: Option<String>,
    /// The commit a `commit` context reads.
    pub commit: Option<String>,
    /// The base commit a layer was copied from.
    pub seed_commit: Option<String>,
    /// Bytes of the layer's directory.
    pub bytes: u64,
    /// Seconds since the epoch of the last use; `None` for the base.
    pub last_used: Option<u64>,
    /// This invocation's own context.
    pub current: bool,
}

/// `contexts.max` when the project does not set it.
pub const CONTEXTS_MAX_DEFAULT: usize = 8;

fn dir_bytes(dir: &Utf8Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if let Ok(m) = e.metadata() {
                total += if m.is_dir() {
                    Utf8Path::from_path(&e.path()).map_or(0, dir_bytes)
                } else {
                    m.len()
                };
            }
        }
    }
    total
}

/// Which context a command reads and writes.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum ContextChoice {
    /// This checkout's own context; the project itself when the repo has no git repository.
    #[default]
    Current,
    Base,
    Named(ContextId),
}

/// `meta` key holding the commit the base reflects for `repo`, as hex.
pub fn base_commit_key(repo: &str) -> String {
    format!("base_commit.{repo}")
}

impl ContextChoice {
    /// `current`, `base`, or the id of a context. Other text is a usage error.
    pub fn parse(text: &str) -> Result<ContextChoice, DistError> {
        Ok(match text {
            "current" => ContextChoice::Current,
            "base" => ContextChoice::Base,
            id => ContextChoice::Named(ContextId::new(id)?),
        })
    }
}

impl Katalogr {
    pub fn set_context(&mut self, c: ContextChoice) {
        self.context = c;
    }

    /// The repo of the nearest marker.
    pub fn current_repo(&self) -> Result<&str, DistError> {
        self.current().map(|(r, _)| r)
    }

    /// `contexts.max` of the project settings.
    pub fn contexts_max(&self) -> usize {
        self.project
            .config()
            .settings
            .get("contexts.max")
            .and_then(|v| v.as_u64())
            .map_or(CONTEXTS_MAX_DEFAULT, |n| n as usize)
    }

    /// The id of this invocation's own checkout context, when it has one.
    fn own_id(&self) -> Option<ContextId> {
        let root = self.repo.as_deref().and_then(|r| self.roots.get(r))?;
        if !root.join(".git").exists() {
            return None;
        }
        Some(ContextId::for_checkout(&root.canonicalize_utf8().ok()?))
    }

    /// The id of the context this invocation reads: `base`, a checkout's `w-...` or a named one.
    /// Never a path.
    pub fn context_name(&self) -> String {
        match &self.context {
            ContextChoice::Base => ContextId::BASE.to_string(),
            ContextChoice::Named(id) => id.to_string(),
            ContextChoice::Current => self
                .own_id()
                .map_or_else(|| ContextId::BASE.to_string(), |id| id.to_string()),
        }
    }

    fn info_of(&self, m: &ContextManifest, dir: &ProjectDir) -> ContextInfo {
        let used = std::fs::metadata(dir.root().join(".used"))
            .and_then(|md| md.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        let chosen = match &self.context {
            ContextChoice::Named(id) => Some(id.clone()),
            ContextChoice::Current => self.own_id(),
            ContextChoice::Base => None,
        };
        ContextInfo {
            id: m.id.to_string(),
            kind: match m.kind {
                ContextKind::Checkout => "checkout",
                ContextKind::Commit => "commit",
            }
            .to_string(),
            repo: Some(m.repo.clone()),
            commit: m.commit.clone(),
            seed_commit: m.seed_commit.clone(),
            bytes: dir_bytes(dir.root()),
            last_used: used,
            current: chosen.as_ref() == Some(&m.id),
        }
    }

    /// The base, then every layer, sorted by id. Unreadable layers are left out.
    pub fn context_list(&self) -> Result<Vec<ContextInfo>, DistError> {
        let base_current = matches!(self.context, ContextChoice::Base)
            || (matches!(self.context, ContextChoice::Current) && self.own_id().is_none());
        let mut out = vec![ContextInfo {
            id: ContextId::BASE.to_string(),
            kind: "base".to_string(),
            repo: None,
            commit: None,
            seed_commit: None,
            bytes: dir_bytes(&self.project.cache_dir())
                .saturating_sub(dir_bytes(&self.project.cache_dir().join("contexts"))),
            last_used: None,
            current: base_current,
        }];
        for m in self.project.contexts()? {
            let dir = self.project.layer(&m.id);
            out.push(self.info_of(&m, &dir));
        }
        Ok(out)
    }

    /// A context that reads `commit` of `repo` from git objects, with no checkout. Built and
    /// ready when this returns.
    pub fn context_add_commit(
        &self,
        repo: &str,
        commit: &str,
        o: &Options,
    ) -> Result<ContextInfo, DistError> {
        let timeout = o.timeout()?;
        let deadline = Instant::now() + timeout;
        let root = self
            .roots
            .get(repo)
            .ok_or_else(|| crate::unknown_root(repo))?;
        let full = resolve_commit(root.as_std_path(), commit)?;
        let id = ContextId::for_commit(repo, &full);
        let layer = self.project.layer(&id);
        if !layer.root().join(ContextManifest::FILE).is_file() {
            let m = ContextManifest {
                format: ContextManifest::FORMAT,
                id: id.clone(),
                kind: ContextKind::Commit,
                repo: repo.to_string(),
                root: None,
                seed_commit: None,
                commit: Some(full),
            };
            self.evict_for_seed(&id);
            self.seed_from(&layer, &m, deadline)?;
        }
        let layer = self.current_layer(layer, deadline)?;
        let engine = self.engine_on(layer.clone(), deadline)?;
        self.read(&engine, Consistency::WaitAll, timeout, 0, Vec::new())?;
        drop(engine);
        let m = ContextManifest::read(layer.root())?;
        Ok(self.info_of(&m, &layer))
    }

    /// Remove contexts until at most `keep` remain: those whose checkout no longer exists
    /// first, then the least recently used. The base, this invocation's own context and any
    /// context whose processor lock is held are never removed. Returns the removed ids.
    pub fn context_prune(&self, keep: usize) -> Result<Vec<ContextId>, DistError> {
        self.prune_keeping(keep, None)
    }

    fn prune_keeping(
        &self,
        keep: usize,
        also_keep: Option<&ContextId>,
    ) -> Result<Vec<ContextId>, DistError> {
        let own = self.own_id();
        let named = match &self.context {
            ContextChoice::Named(id) => Some(id.clone()),
            _ => None,
        };
        let all = self.project.contexts()?;
        let mut excess = all.len().saturating_sub(keep);
        // Removal order: missing root first, then oldest use.
        let mut order: Vec<(bool, u64, ContextManifest)> = all
            .into_iter()
            .map(|m| {
                let missing = m.root.as_ref().is_some_and(|r| !r.exists());
                let used = std::fs::metadata(self.project.layer(&m.id).root().join(".used"))
                    .and_then(|md| md.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs());
                (!missing, used, m)
            })
            .collect();
        order.sort_by(|a, b| (a.0, a.1, &a.2.id).cmp(&(b.0, b.1, &b.2.id)));
        let mut removed = Vec::new();
        for (_, _, m) in order {
            if excess == 0 {
                break;
            }
            if Some(&m.id) == own.as_ref()
                || Some(&m.id) == named.as_ref()
                || Some(&m.id) == also_keep
            {
                continue;
            }
            let layer = self.project.layer(&m.id);
            // A held lock means a process is using it.
            let Some(lock) = katalogr_engine::queue::lock::try_lock(&layer.cache_dir())? else {
                continue;
            };
            std::fs::remove_dir_all(layer.root()).map_err(|e| DistError::io(layer.root(), e))?;
            drop(lock);
            removed.push(m.id);
            excess -= 1;
        }
        Ok(removed)
    }

    /// Make room before a seed: past `contexts.max`, the oldest go.
    fn evict_for_seed(&self, new: &ContextId) {
        let max = self.contexts_max().max(1);
        if let Ok(removed) = self.prune_keeping(max - 1, Some(new))
            && !removed.is_empty()
        {
            self.note(format!(
                "contexts.max {max}: removed {} context(s) to make room",
                removed.len()
            ));
        }
    }

    /// Snapshots of several contexts (`base`, `current` or an id), each drained first. A context
    /// named twice is opened once. Returns the report that covers them all.
    fn snapshots(
        &self,
        names: &[&str],
        o: &Options,
    ) -> Result<(Vec<Engine>, Vec<usize>, Report), DistError> {
        let timeout = o.timeout()?;
        let deadline = Instant::now() + timeout;
        let mut engines: Vec<Engine> = Vec::new();
        let mut roots: Vec<camino::Utf8PathBuf> = Vec::new();
        let mut which = Vec::new();
        for n in names {
            let dir = self.layer_dir_for(&ContextChoice::parse(n)?, deadline)?;
            match roots.iter().position(|r| r == dir.root()) {
                Some(i) => which.push(i),
                None => {
                    roots.push(dir.root().to_owned());
                    engines.push(self.engine_on(dir, deadline)?);
                    which.push(engines.len() - 1);
                }
            }
        }
        let consistency = o.consistency.unwrap_or(Consistency::Wait);
        let mut report: Option<Report> = None;
        for e in &engines {
            let r = self.read(e, consistency, timeout, 0, Vec::new())?;
            report = Some(match report {
                None => r,
                Some(mut acc) => {
                    acc.watermark = acc.watermark.max(r.watermark);
                    acc.pending += r.pending;
                    acc.stale |= r.stale;
                    acc.timed_out |= r.timed_out;
                    acc.warnings.extend(r.warnings);
                    acc
                }
            });
        }
        Ok((engines, which, report.expect("at least one context")))
    }

    /// What differs between contexts `a` and `b` (`base`, `current` or an id).
    pub fn context_diff(
        &self,
        a: &str,
        b: &str,
        limit: usize,
        o: &Options,
    ) -> Result<Reply<ContextDiff>, DistError> {
        let (engines, which, report) = self.snapshots(&[a, b], o)?;
        let (ra, rb) = (
            engines[which[0]].cache().read()?,
            engines[which[1]].cache().read()?,
        );
        let diff = katalogr_analytics::context_diff::context_diff(&ra, &rb, limit)?;
        let partial = diff.partial.clone();
        Ok(Reply {
            outcome: Outcome::Found(diff),
            report,
            diagnostic: None,
            partial,
            folded: Vec::new(),
        })
    }

    /// The contracts both `a` and `b` changed against the base.
    pub fn context_conflicts(
        &self,
        a: &str,
        b: &str,
        o: &Options,
    ) -> Result<Reply<ContextConflicts>, DistError> {
        let (engines, which, report) = self.snapshots(&["base", a, b], o)?;
        let (rbase, ra, rb) = (
            engines[which[0]].cache().read()?,
            engines[which[1]].cache().read()?,
            engines[which[2]].cache().read()?,
        );
        let c = katalogr_analytics::context_diff::context_conflicts(&rbase, &ra, &rb)?;
        Ok(Reply {
            outcome: Outcome::Found(c),
            report,
            diagnostic: None,
            partial: Vec::new(),
            folded: Vec::new(),
        })
    }

    pub(crate) fn take_notes(&self) -> Vec<String> {
        std::mem::take(&mut *self.notes.lock().expect("notes lock"))
    }

    fn note(&self, text: String) {
        self.notes.lock().expect("notes lock").push(text);
    }

    /// The `ProjectDir` for the chosen context, seeding it first when it is this checkout's
    /// and does not exist yet. A busy base never hangs a read: when its lock cannot be taken
    /// by `deadline` the answer comes from the base, with a warning naming the context.
    pub(crate) fn layer_dir(&self, deadline: Instant) -> Result<ProjectDir, DistError> {
        self.layer_dir_for(&self.context, deadline)
    }

    pub(crate) fn layer_dir_for(
        &self,
        choice: &ContextChoice,
        deadline: Instant,
    ) -> Result<ProjectDir, DistError> {
        let id = match choice {
            ContextChoice::Base => return Ok(self.project.clone()),
            ContextChoice::Named(id) => {
                if id.as_str() == ContextId::BASE {
                    return Ok(self.project.clone());
                }
                let layer = self.project.layer(id);
                if !layer.root().join(ContextManifest::FILE).is_file() {
                    return Err(DistError::usage(format!(
                        "unknown context {id}: see `katalogr context list`"
                    )));
                }
                return self.current_layer(layer, deadline);
            }
            ContextChoice::Current => {
                let Some((repo, root)) = self
                    .repo
                    .as_deref()
                    .and_then(|r| self.roots.get(r).map(|p| (r, p)))
                else {
                    return Ok(self.project.clone());
                };
                if !root.join(".git").exists() {
                    return Ok(self.project.clone());
                }
                let canon = root
                    .canonicalize_utf8()
                    .map_err(|e| DistError::io(root, e))?;
                (ContextId::for_checkout(&canon), repo, canon)
            }
        };
        let (id, repo, canon) = id;
        let layer = self.project.layer(&id);
        if layer.root().join(ContextManifest::FILE).is_file() {
            return self.current_layer(layer, deadline);
        }
        self.seed(&layer, &id, repo, &canon, deadline)
    }

    /// An existing layer, made usable: one whose cache has another `CACHE_SCHEMA`, or cannot be
    /// read, is deleted and seeded again. `open_or_rebuild` is never let loose on a layer,
    /// because rebuilding from delta shards alone would give an empty graph.
    fn current_layer(&self, layer: ProjectDir, deadline: Instant) -> Result<ProjectDir, DistError> {
        let usable = match Cache::inspect(&layer)? {
            CacheInspection::Missing | CacheInspection::InUse => true,
            CacheInspection::Open(o) => o.stored_schema == Some(CACHE_SCHEMA),
            CacheInspection::Unreadable(_) => false,
        };
        if usable {
            let _ = std::fs::File::create(layer.root().join(".used"));
            return Ok(layer);
        }
        let m = ContextManifest::read(layer.root())?;
        // The manifest of a commit context names no root; keep what it says.
        std::fs::remove_dir_all(layer.root()).map_err(|e| DistError::io(layer.root(), e))?;
        self.seed_from(&layer, &m, deadline)
    }

    fn seed(
        &self,
        layer: &ProjectDir,
        id: &ContextId,
        repo: &str,
        root: &Utf8Path,
        deadline: Instant,
    ) -> Result<ProjectDir, DistError> {
        let m = ContextManifest {
            format: ContextManifest::FORMAT,
            id: id.clone(),
            kind: ContextKind::Checkout,
            repo: repo.to_string(),
            root: Some(root.to_owned()),
            seed_commit: None,
            commit: None,
        };
        self.evict_for_seed(id);
        self.seed_from(layer, &m, deadline)
    }

    fn seed_from(
        &self,
        layer: &ProjectDir,
        m: &ContextManifest,
        deadline: Instant,
    ) -> Result<ProjectDir, DistError> {
        let (id, repo) = (&m.id, m.repo.as_str());
        let Some(lock) = lock_until(&self.project.cache_dir(), deadline)? else {
            self.note(format!(
                "context {id} not seeded: the base is busy; answered from the base"
            ));
            return Ok(self.project.clone());
        };
        // Another process may have seeded it while this one waited.
        if !layer.root().join(ContextManifest::FILE).is_file() {
            let mut m = m.clone();
            m.seed_commit = None;
            seed_layer(&self.project, layer, &m, deadline)?;
            drop(lock);
            self.queue_seed_delta(layer, repo)?;
        }
        let _ = std::fs::File::create(layer.root().join(".used"));
        Ok(layer.clone())
    }

    /// What a fresh layer still lacks (FEAT-043 Seed step 5, with FEAT-084). A checkout layer
    /// is seeded with the position it reflects, so its first read queues the paths between that
    /// commit and the checkout plus the uncommitted ones (reconcile). A commit layer queues the
    /// paths between the seed commit and its own commit. With the seed commit unknown, the
    /// whole repo is queued.
    fn queue_seed_delta(&self, layer: &ProjectDir, repo: &str) -> Result<(), DistError> {
        let m = ContextManifest::read(layer.root())?;
        let inbox = Inbox::new(layer);
        let seed = m.seed_commit.as_deref().filter(|c| !c.is_empty());
        match m.kind {
            ContextKind::Checkout => {
                if seed.is_none() {
                    inbox.append(&NewEvent::rescan(repo))?;
                }
            }
            ContextKind::Commit => {
                let (Some(seed), Some(commit), Some(root)) =
                    (seed, m.commit.as_deref(), self.roots.get(repo))
                else {
                    inbox.append(&NewEvent::rescan(repo))?;
                    return Ok(());
                };
                match katalogr_engine::scan::changed_paths(
                    root.as_std_path(),
                    &format!("{seed}..{commit}"),
                ) {
                    Ok(changes) => {
                        use katalogr_engine::scan::PathChange as C;
                        for c in changes {
                            match c {
                                C::Added(p) | C::Modified(p) => {
                                    inbox.append(&NewEvent::file(repo, &p, EventKind::Changed))?;
                                }
                                C::Deleted(p) => {
                                    inbox.append(&NewEvent::file(repo, &p, EventKind::Deleted))?;
                                }
                                C::Renamed { from, to } => {
                                    inbox.append(&NewEvent::file(
                                        repo,
                                        &from,
                                        EventKind::Deleted,
                                    ))?;
                                    inbox.append(&NewEvent::file(repo, &to, EventKind::Changed))?;
                                }
                            }
                        }
                    }
                    Err(_) => {
                        inbox.append(&NewEvent::rescan(repo))?;
                    }
                }
            }
        }
        Ok(())
    }

    /// The base cache's view of a repo: the commit the base reflects, if recorded.
    pub fn base_commit(&self, repo: &str, opts: &Options) -> Result<Option<String>, DistError> {
        let engine = self.engine_on(self.project.clone(), Instant::now() + opts.timeout()?)?;
        let r = engine.cache().read()?;
        Ok(r.meta(&base_commit_key(repo))?
            .and_then(|b| String::from_utf8(b).ok()))
    }

    /// Bring the base to the default-branch tip of every repo with a root here, reading each
    /// repo at that commit whatever the checkout has on disk. `base_commit.<repo>` is set once
    /// the queue is drained; a failed or timed-out advance leaves it where it was. Repos
    /// without a git repository or default branch are skipped.
    pub fn advance_base(&self, opts: &Options) -> Result<Report, DistError> {
        let timeout = opts.timeout()?;
        let mut warnings = Vec::new();
        let mut queued = 0;
        let mut last: Option<Report> = None;
        for r in &self.project.config().repos {
            let Some(root) = self.roots.get(&r.name) else {
                continue;
            };
            let configured = self
                .project
                .config()
                .settings
                .get(&format!("base.branch.{}", r.name))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let tip = match default_branch(root.as_std_path(), configured.as_deref())
                .and_then(|b| b.map_or(Ok(None), |b| branch_tip(root.as_std_path(), &b)))
            {
                Ok(Some(tip)) => tip,
                Ok(None) | Err(ScanError::RepoOpen { .. }) => continue,
                Err(e) => return Err(e.into()),
            };
            let mut engine = self.engine_on(self.project.clone(), Instant::now() + timeout)?;
            let key = base_commit_key(&r.name);
            let from = engine
                .cache()
                .read()?
                .meta(&key)?
                .and_then(|b| String::from_utf8(b).ok());
            if from.as_deref() == Some(tip.as_str()) {
                continue;
            }
            engine.set_source(
                &r.name,
                FileSource::Commit {
                    repo_root: root.clone(),
                    commit: tip.clone(),
                },
            );
            let events = plan_base_advance(&r.name, root.as_std_path(), from.as_deref(), &tip)?;
            let inbox = engine.inbox();
            for ev in &events {
                inbox.append(ev)?;
            }
            queued += events.len();
            let rep = self.read(
                &engine,
                Consistency::WaitAll,
                timeout,
                events.len(),
                Vec::new(),
            )?;
            if rep.stale || rep.pending > 0 {
                warnings.push(format!(
                    "{}: base advance to {} not finished; base_commit unchanged",
                    r.name,
                    &tip[..12.min(tip.len())]
                ));
            } else {
                let mut tx = engine.cache().write()?;
                tx.set_meta(&key, tip.as_bytes())?;
                tx.commit()?;
            }
            last = Some(rep);
        }
        let mut report = match last {
            Some(r) => r,
            None => {
                let engine = self.engine_on(self.project.clone(), Instant::now() + timeout)?;
                self.read(&engine, Consistency::StaleOk, timeout, 0, Vec::new())?
            }
        };
        report.queued = queued;
        report.warnings.extend(warnings);
        Ok(report)
    }
}
