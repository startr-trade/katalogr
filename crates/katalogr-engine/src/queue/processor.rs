//! The processor: one per project, enforced by an exclusive file lock. It ingests inbox
//! entries, extracts the current content of queued paths and publishes results together with
//! the watermark (FEAT-047 sections 3, 4 and 6; ADR-0014).

use super::plan::{PlanEnv, Prepared, Ready, run_batch};
use super::{ChangeEvent, Context, EventKind, Inbox, Seq};
use crate::census::extractor_for;
use crate::consistency::{Engine, IndexStats, Scope};
use crate::context;
use crate::embed;
use crate::error::EngineError;
use crate::failpoint::fail;
use crate::profiles::{self, ProfileSet};
use crate::scan;
use camino::{Utf8Path, Utf8PathBuf};
use katalogr_model::{ArtifactName, RelPath};
use katalogr_store::{ErrorRow, JournalEntry, PendingKey, RANK_CONTEXT, Reader, WriteTx, pagerank};
use std::collections::BTreeSet;
use std::fs::File;
use std::time::Instant;

/// Holds the project's processor lock while alive.
pub struct Processor<'a> {
    engine: &'a Engine,
    _lock: File,
    /// The profiles in force after the last `refresh_profiles`; they route files to
    /// extensions when a rescan lists paths.
    profiles: Option<ProfileSet>,
}

impl<'a> Processor<'a> {
    /// Non-blocking exclusive lock on `.cache/processor.lock`; `None` when another holder has it.
    pub fn try_acquire(engine: &'a Engine) -> Result<Option<Processor<'a>>, EngineError> {
        Ok(
            super::lock::try_lock(&engine.dir().cache_dir())?.map(|file| Processor {
                engine,
                _lock: file,
                profiles: None,
            }),
        )
    }

    /// Replay a non-empty publish journal (idempotent): rename each temp file that still
    /// exists onto its final name, then clear the journal.
    pub fn recover(&mut self) -> Result<(), EngineError> {
        let dir = self.engine.dir();
        self.engine.cache().replay_journal(dir)?;
        // The journal is empty now, so any `.tmp-*` shard file left is an orphan of a crash
        // before the publish commit.
        remove_orphan_temps(&dir.root().join("shards"))
    }

    /// R-2/R-3. One write transaction: skip ids already in `seen`, assign the next sequence
    /// number of the event's context, record the event and set the path's pending entry to that
    /// number. A rescan is expanded here into one pending entry per path the registry handles
    /// plus one per path that still has a shard. Inbox files are deleted after the commit.
    /// Returns the number of newly ingested events.
    pub fn ingest(&mut self) -> Result<usize, EngineError> {
        let inbox = Inbox::new(self.engine.dir());
        let files = inbox.list()?;
        if files.is_empty() {
            return Ok(0);
        }
        if self.profiles.is_none() {
            self.refresh_profiles()?;
        }
        let mut tx = self.engine.cache().write()?;
        let mut ingested = 0;
        let mut done = Vec::new();
        let mut bad = Vec::new();
        for file in files {
            let ev = match inbox.read(&file) {
                Ok(ev) => ev,
                Err(_) => {
                    bad.push(file);
                    continue;
                }
            };
            if tx.seen_seq(&ev.id)?.is_some() {
                done.push(file);
                continue;
            }
            let ctx = ev.context.0.as_str();
            let seq = tx.last_seq(ctx)? + 1;
            tx.set_last_seq(ctx, seq)?;
            tx.put_seen(&ev.id, seq)?;
            let bytes = serde_json::to_vec(&ev).map_err(|e| EngineError::Queue(e.to_string()))?;
            tx.put_event(ctx, seq, &bytes)?;
            let key = |path: RelPath| PendingKey {
                context: ev.context.0.clone(),
                repo: ev.repo.clone(),
                path,
            };
            match (&ev.path, ev.kind) {
                (Some(p), _) => tx.put_pending(&key(p.clone()), seq)?,
                (None, EventKind::Rescan) => {
                    for p in self.expand_rescan(&tx, &ev)? {
                        tx.put_pending(&key(p), seq)?;
                    }
                }
                (None, _) => {
                    // A change without a path carries nothing to process.
                }
            }
            ingested += 1;
            done.push(file);
        }
        tx.commit()?;
        fail!("after_ingest_commit");
        inbox.remove(&done)?;
        for f in bad {
            inbox.set_aside(&f)?;
        }
        Ok(ingested)
    }

    /// Paths of a rescanned repository: every file the registry handles under its root that the
    /// repo's include and exclude globs admit, and
    /// every path that has a shard (so vanished files are removed).
    fn expand_rescan(
        &self,
        tx: &impl Reader,
        ev: &ChangeEvent,
    ) -> Result<BTreeSet<RelPath>, EngineError> {
        let mut out = BTreeSet::new();
        if let Some(root) = self.engine.roots().get(&ev.repo) {
            let (include, exclude) = self.engine.repo_filters(&ev.repo);
            let none = ProfileSet::empty();
            let profiles = self.profiles.as_ref().unwrap_or(&none);
            for p in scan::list_files(root.as_std_path(), &include, &exclude)? {
                if extractor_for(self.engine.registry(), profiles, &p).is_some() {
                    out.insert(p);
                }
            }
        }
        for ((repo, path), _) in tx.shard_hashes()? {
            if repo == ev.repo {
                out.insert(path);
            }
        }
        Ok(out)
    }

    /// Process up to `budget` pending paths, those in `scope` first, then publish them and
    /// finish the publish (renames, journal clear). Returns the `worktree` watermark.
    pub fn process(
        &mut self,
        scope: &Scope,
        budget: usize,
        deadline: Instant,
    ) -> Result<Seq, EngineError> {
        let wm = self.process_to_commit(scope, budget, deadline)?;
        self.recover()?;
        self.refresh_rank()?;
        Ok(wm)
    }

    /// FEAT-057. When the stored rank does not reflect the current watermark (a publish changed
    /// the rank's inputs, which drops it), compute it and store it with that watermark, in one
    /// transaction while this processor holds its lock. A query that arrives meanwhile sees a
    /// rank whose watermark is behind and computes live.
    pub fn refresh_rank(&mut self) -> Result<(), EngineError> {
        let cache = self.engine.cache();
        if cache.read()?.rank_is_current(RANK_CONTEXT)? {
            return Ok(());
        }
        let mut tx = cache.write()?;
        let symbols = tx.symbols()?;
        let scores = pagerank(&tx, &symbols)?;
        let ranked: Vec<_> = symbols.iter().map(|(id, _)| *id).zip(scores).collect();
        let wm = tx.watermark(RANK_CONTEXT)?;
        tx.store_rank(&ranked, wm)?;
        tx.commit()?;
        Ok(())
    }

    /// The publish up to and including the commit; the renames and the journal clear are left
    /// for [`Processor::recover`]. Exposed so tests can stop at that point.
    #[doc(hidden)]
    pub fn process_to_commit(
        &mut self,
        scope: &Scope,
        budget: usize,
        deadline: Instant,
    ) -> Result<Seq, EngineError> {
        let engine = self.engine;
        self.refresh_embed_signature()?;
        let profiles = self.refresh_profiles()?;
        self.refresh_contexts()?;
        let snap = engine.cache().read()?;
        let pending = snap.pending_entries(None)?;
        let (mut first, rest): (Vec<_>, Vec<_>) = pending
            .into_iter()
            .partition(|(k, _)| scope.contains(&k.repo, &k.path));
        first.extend(rest);
        first.truncate(budget);

        let env = PlanEnv::load(engine, &snap, &profiles, &first)?;
        let par = engine.parallelism();
        let started = Instant::now();
        let out = run_batch(&env, &first, deadline, par, engine.pool())?;
        let prepare = started.elapsed();
        if out.is_empty() {
            return Ok(Seq(engine.cache().read()?.watermark(Context::WORKTREE)?));
        }

        // Shard bytes are already in temp files; they become visible only through the journal.
        let mut journal: Vec<JournalEntry> = Vec::new();
        for p in &out {
            match &p.ready {
                Ready::Replace { tmp, fin, .. } => journal.push((tmp.clone(), fin.clone())),
                Ready::Delete { fin: Some(fin) } => journal.push((String::new(), fin.clone())),
                _ => {}
            }
        }

        let mut tx = engine.cache().write()?;
        let rank_was_current = tx.rank_is_current(RANK_CONTEXT)?;
        let mut touched: BTreeSet<String> = BTreeSet::new();
        let stitch_started = Instant::now();
        let files = out.len() as u64;
        for p in out {
            let Prepared { key, seq, ready } = p;
            match ready {
                Ready::Skip => {}
                Ready::Replace {
                    prepared,
                    errors,
                    hash,
                    ..
                } => {
                    engine.stitcher().apply(&mut tx, *prepared)?;
                    tx.put_shard_hash(&key.repo, &key.path, &hash)?;
                    tx.put_index_errors(&key.repo, &key.path, &errors)?;
                }
                Ready::Delete { .. } => {
                    engine.stitcher().remove_artifact(
                        &mut tx,
                        &ArtifactName {
                            repo: key.repo.clone(),
                            path: key.path.clone(),
                        },
                    )?;
                    tx.remove_shard_hash(&key.repo, &key.path)?;
                    tx.put_index_errors(&key.repo, &key.path, &[])?;
                }
                Ready::Failed(message) => {
                    tx.put_index_errors(
                        &key.repo,
                        &key.path,
                        &[ErrorRow {
                            span: None,
                            message,
                        }],
                    )?;
                }
            }
            if tx.pending_seq(&key)?.is_some_and(|s| s <= seq) {
                tx.remove_pending(&key)?;
            }
            touched.insert(key.context);
        }
        // The watermark covers an event only when nothing at or below it is still queued.
        for ctx in &touched {
            let old = tx.watermark(ctx)?;
            let lowest = tx
                .pending_entries(Some(ctx))?
                .into_iter()
                .map(|(_, s)| s)
                .min();
            let wm = match lowest {
                Some(s) => s - 1,
                None => tx.last_seq(ctx)?,
            };
            let wm = wm.max(old);
            tx.set_watermark(ctx, wm)?;
            tx.prune_events(ctx, wm)?;
        }
        let stitch = stitch_started.elapsed();
        tx.set_publish_journal(&journal)?;
        let wm = tx.watermark(Context::WORKTREE)?;
        // A publish that changed nothing the rank depends on keeps the stored rank, which then
        // reflects the new watermark; one that did drops it (in `commit`) for `refresh_rank`.
        if !tx.rank_inputs_changed()? && rank_was_current {
            tx.set_rank_watermark(wm)?;
        }
        let commit_started = Instant::now();
        tx.commit()?;
        engine.add_stats(IndexStats {
            rounds: 1,
            files,
            prepare,
            stitch,
            commit: commit_started.elapsed(),
        });
        fail!("after_publish_commit");
        Ok(Seq(wm))
    }

    /// Rebuild the context of every (repo, extension) pair that a queued path touches: the path
    /// is one of the extension's context files, or the extension handles it and has no stored
    /// context yet. A changed context is stored, and a rescan of the repo is queued in the same
    /// transaction, so every handled file is extracted again against it.
    fn refresh_contexts(&mut self) -> Result<(), EngineError> {
        let engine = self.engine;
        let snap = engine.cache().read()?;
        let mut due: BTreeSet<(String, &'static str)> = BTreeSet::new();
        let none = ProfileSet::empty();
        let profiles = self.profiles.as_ref().unwrap_or(&none);
        for (key, _) in snap.pending_entries(None)? {
            if !engine.roots().contains_key(&key.repo) {
                continue;
            }
            for ext in engine.registry().extractors() {
                if context::is_context_file(ext, &key.path) {
                    due.insert((key.repo.clone(), ext.id()));
                }
            }
            // The routed extractor (a profile `select`, else the path's own) is the one planned.
            let routed = extractor_for(engine.registry(), profiles, &key.path);
            if let Some(ext) = routed
                && !ext.context_files().is_empty()
                && snap.ext_context(&key.repo, ext.id())?.is_none()
            {
                due.insert((key.repo.clone(), ext.id()));
            }
            // A host's guests need their context before the host file is planned.
            if routed.is_some_and(|host| host.embeds()) {
                for g in engine.registry().extractors() {
                    if g.embedded_version().is_some()
                        && !g.context_files().is_empty()
                        && snap.ext_context(&key.repo, g.id())?.is_none()
                    {
                        due.insert((key.repo.clone(), g.id()));
                    }
                }
            }
        }
        if due.is_empty() {
            return Ok(());
        }
        let mut tx = engine.cache().write()?;
        let mut changed = false;
        for (repo, id) in due {
            let Some(ext) = engine.registry().extractors().find(|e| e.id() == id) else {
                continue;
            };
            let Some(bytes) = context::build(engine, &repo, ext)? else {
                continue;
            };
            let new = context::hash(&bytes);
            let stored = tx.ext_context(&repo, id)?;
            if stored.as_ref().is_some_and(|(h, _)| *h == new) {
                continue;
            }
            let old = stored.map_or_else(|| context::hash(&[]), |(h, _)| h);
            tx.put_ext_context(&repo, id, &new, &bytes)?;
            changed = true;
            if old != new {
                self.queue_rescan(&mut tx, &repo, &format!("context-{id}"))?;
            }
        }
        if changed {
            tx.commit()?;
        }
        Ok(())
    }

    /// When the guest signature (ids and versions of the guest extensions) differs from the
    /// stored one, store it and queue a rescan of every repository, so host files are extracted
    /// against the new guest set. Runs before `refresh_contexts` so that step sees the queued paths.
    pub fn refresh_embed_signature(&mut self) -> Result<(), EngineError> {
        let engine = self.engine;
        let new = embed::signature(engine.registry())
            .map(|h| h.to_vec())
            .unwrap_or_default();
        let snap = engine.cache().read()?;
        let stored = snap.meta(embed::META_SIGNATURE)?.unwrap_or_default();
        drop(snap);
        if stored == new {
            return Ok(());
        }
        let mut tx = engine.cache().write()?;
        tx.set_meta(embed::META_SIGNATURE, &new)?;
        let repos: Vec<String> = engine.roots().keys().cloned().collect();
        for repo in repos {
            self.queue_rescan(&mut tx, &repo, "embed")?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Record a rescan of `repo` as a worktree event named `<label>-<seq>` and queue every path
    /// it expands to, in the caller's transaction.
    fn queue_rescan(&self, tx: &mut WriteTx, repo: &str, label: &str) -> Result<(), EngineError> {
        let ctx = Context::WORKTREE;
        let seq = tx.last_seq(ctx)? + 1;
        tx.set_last_seq(ctx, seq)?;
        let ev = ChangeEvent {
            id: format!("{label}-{seq}"),
            context: Context::worktree(),
            repo: repo.to_string(),
            path: None,
            kind: EventKind::Rescan,
        };
        let json = serde_json::to_vec(&ev).map_err(|e| EngineError::Queue(e.to_string()))?;
        tx.put_event(ctx, seq, &json)?;
        for path in self.expand_rescan(tx, &ev)? {
            tx.put_pending(
                &PendingKey {
                    context: ctx.to_string(),
                    repo: repo.to_string(),
                    path,
                },
                seq,
            )?;
        }
        Ok(())
    }

    /// Load the profiles in force (built-ins, then the project's `profiles/` folder; a refused
    /// file keeps its last valid text). The valid texts and the refusals are stored in the
    /// cache. For each extension whose profile hash differs from the stored one, every
    /// repository is queued for a rescan in the same transaction, so the files the changed
    /// profile can affect are extracted and profiled again.
    pub fn refresh_profiles(&mut self) -> Result<ProfileSet, EngineError> {
        let engine = self.engine;
        let snap = engine.cache().read()?;
        let report = engine.load_profiles(&snap)?;
        self.profiles = Some(report.set.clone());
        let good =
            serde_json::to_vec(&report.good).map_err(|e| EngineError::Queue(e.to_string()))?;
        let errors =
            serde_json::to_vec(&report.errors).map_err(|e| EngineError::Queue(e.to_string()))?;
        let mut changed: Vec<(String, Vec<u8>)> = Vec::new();
        for id in engine.registry().ids() {
            let key = format!("profile_hash:{id}");
            let new = report
                .set
                .hash_for(id)
                .map(|h| h.to_vec())
                .unwrap_or_default();
            let stored = snap.meta(&key)?.unwrap_or_default();
            if stored != new {
                changed.push((key, new));
            }
        }
        let same =
            |key: &str, bytes: &[u8]| snap.meta(key).ok().flatten().as_deref() == Some(bytes);
        if changed.is_empty()
            && same(profiles::META_GOOD, &good)
            && same(profiles::META_ERRORS, &errors)
        {
            return Ok(report.set);
        }
        drop(snap);
        let mut tx = engine.cache().write()?;
        tx.set_meta(profiles::META_GOOD, &good)?;
        tx.set_meta(profiles::META_ERRORS, &errors)?;
        if !changed.is_empty() {
            for (key, hash) in &changed {
                tx.set_meta(key, hash)?;
            }
            let repos: Vec<String> = engine.roots().keys().cloned().collect();
            for repo in repos {
                self.queue_rescan(&mut tx, &repo, "profiles")?;
            }
        }
        tx.commit()?;
        Ok(report.set)
    }
}

/// The `.tmp-*` files under `dir`, recursively, sorted. A missing directory has none.
pub fn find_orphan_temps(dir: &Utf8Path) -> Result<Vec<Utf8PathBuf>, EngineError> {
    let mut out = Vec::new();
    collect_orphan_temps(dir, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_orphan_temps(dir: &Utf8Path, out: &mut Vec<Utf8PathBuf>) -> Result<(), EngineError> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(EngineError::io(dir, e)),
    };
    for e in rd {
        let e = e.map_err(|e| EngineError::io(dir, e))?;
        let Some(name) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let path = dir.join(&name);
        let ft = e.file_type().map_err(|e| EngineError::io(dir, e))?;
        if ft.is_dir() {
            collect_orphan_temps(&path, out)?;
        } else if ft.is_file() && name.starts_with(".tmp-") {
            out.push(path);
        }
    }
    Ok(())
}

/// Delete every `.tmp-*` file under `dir`, recursively.
fn remove_orphan_temps(dir: &Utf8Path) -> Result<(), EngineError> {
    for path in find_orphan_temps(dir)? {
        std::fs::remove_file(&path).map_err(|e| EngineError::io(&path, e))?;
    }
    Ok(())
}
