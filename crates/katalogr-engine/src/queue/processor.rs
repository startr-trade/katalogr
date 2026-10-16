//! The processor: one per project, enforced by an exclusive file lock. It ingests inbox
//! entries, extracts the current content of queued paths and publishes results together with
//! the watermark (FEAT-047 sections 3, 4 and 6; ADR-0014).

use super::{ChangeEvent, Context, EventKind, Inbox, Seq};
use crate::consistency::{Engine, Scope};
use crate::context;
use crate::error::EngineError;
use crate::failpoint::fail;
use camino::{Utf8Path, Utf8PathBuf};
use fs4::FileExt;
use katalogr_model::{ArtifactName, ExtractInput, Fragment, RelPath};
use katalogr_store::blob::git_blob_id;
use katalogr_store::{ErrorRow, JournalEntry, PendingKey, ReadTx, Reader};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::File;
use std::time::Instant;

/// Holds the project's processor lock while alive.
pub struct Processor<'a> {
    engine: &'a Engine,
    _lock: File,
}

/// What one pending path needs.
enum Action {
    /// Nothing to publish; the entry is dropped.
    Skip,
    Replace {
        fragment: Box<Fragment>,
        tmp: String,
        fin: String,
    },
    Delete {
        fin: Option<String>,
    },
    Failed(String),
}

struct Planned {
    key: PendingKey,
    seq: u64,
    action: Action,
}

impl<'a> Processor<'a> {
    /// Non-blocking exclusive lock on `.cache/processor.lock`; `None` when another holder has it.
    pub fn try_acquire(engine: &'a Engine) -> Result<Option<Processor<'a>>, EngineError> {
        let cache_dir = engine.dir().cache_dir();
        std::fs::create_dir_all(&cache_dir).map_err(|e| EngineError::io(&cache_dir, e))?;
        let path = cache_dir.join("processor.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| EngineError::io(&path, e))?;
        match FileExt::try_lock(&file) {
            Ok(()) => Ok(Some(Processor {
                engine,
                _lock: file,
            })),
            Err(fs4::TryLockError::WouldBlock) => Ok(None),
            Err(fs4::TryLockError::Error(e)) => Err(EngineError::io(&path, e)),
        }
    }

    /// Replay a non-empty publish journal (idempotent): rename each temp file that still
    /// exists onto its final name, then clear the journal.
    pub fn recover(&mut self) -> Result<(), EngineError> {
        self.engine.cache().replay_journal(self.engine.dir())?;
        Ok(())
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

    /// Paths of a rescanned repository: every file the registry handles under its root, and
    /// every path that has a shard (so vanished files are removed).
    fn expand_rescan(
        &self,
        tx: &impl Reader,
        ev: &ChangeEvent,
    ) -> Result<BTreeSet<RelPath>, EngineError> {
        let mut out = BTreeSet::new();
        if let Some(root) = self.engine.roots().get(&ev.repo) {
            walk(root, "", self.engine, &mut out)?;
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
        Ok(wm)
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
        let dir = engine.dir();
        self.refresh_contexts()?;
        let snap = engine.cache().read()?;
        let pending = snap.pending_entries(None)?;
        let (mut first, rest): (Vec<_>, Vec<_>) = pending
            .into_iter()
            .partition(|(k, _)| scope.contains(&k.repo, &k.path));
        first.extend(rest);
        first.truncate(budget);

        let mut planned = Vec::new();
        for (key, seq) in first {
            if Instant::now() >= deadline && !planned.is_empty() {
                break;
            }
            let action = self.plan(&snap, &key, seq)?;
            planned.push(Planned { key, seq, action });
        }
        if planned.is_empty() {
            return Ok(Seq(engine.cache().read()?.watermark(Context::WORKTREE)?));
        }

        // Shard bytes go to temp files first; they become visible only through the journal.
        let mut journal: Vec<JournalEntry> = Vec::new();
        let mut written: Vec<(Utf8PathBuf, Vec<u8>)> = Vec::new();
        for p in &planned {
            if let Action::Replace {
                fragment, tmp, fin, ..
            } = &p.action
            {
                let bytes = dir.encode_shard(fragment)?;
                written.push((dir.root().join(tmp), bytes));
                journal.push((tmp.clone(), fin.clone()));
            }
            if let Action::Delete { fin: Some(fin) } = &p.action {
                journal.push((String::new(), fin.clone()));
            }
        }
        let mut hashes = Vec::new();
        for (path, bytes) in &written {
            write_synced(path, bytes)?;
            hashes.push(<[u8; 32]>::from(Sha256::digest(bytes)));
        }

        let mut tx = engine.cache().write()?;
        let mut touched: BTreeSet<String> = BTreeSet::new();
        let mut hash_iter = hashes.into_iter();
        for p in planned {
            let Planned { key, seq, action } = p;
            match action {
                Action::Skip => {}
                Action::Replace { fragment, .. } => {
                    let hash = hash_iter.next().expect("one hash per written shard");
                    engine.stitcher().replace_fragment(&mut tx, *fragment)?;
                    tx.put_shard_hash(&key.repo, &key.path, &hash)?;
                    tx.put_index_errors(&key.repo, &key.path, &[])?;
                }
                Action::Delete { .. } => {
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
                Action::Failed(message) => {
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
        tx.set_publish_journal(&journal)?;
        let wm = tx.watermark(Context::WORKTREE)?;
        tx.commit()?;
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
        for (key, _) in snap.pending_entries(None)? {
            if !engine.roots().contains_key(&key.repo) {
                continue;
            }
            for ext in engine.registry().extractors() {
                if !ext.context_files().is_empty() && context::is_context_file(ext, &key.path) {
                    due.insert((key.repo.clone(), ext.id()));
                }
            }
            if let Some(ext) = engine.registry().for_path(&key.path)
                && !ext.context_files().is_empty()
                && snap.ext_context(&key.repo, ext.id())?.is_none()
            {
                due.insert((key.repo.clone(), ext.id()));
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
                let ctx = Context::WORKTREE;
                let seq = tx.last_seq(ctx)? + 1;
                tx.set_last_seq(ctx, seq)?;
                let ev = ChangeEvent {
                    id: format!("context-{id}-{seq}"),
                    context: Context::worktree(),
                    repo: repo.clone(),
                    path: None,
                    kind: EventKind::Rescan,
                };
                let json =
                    serde_json::to_vec(&ev).map_err(|e| EngineError::Queue(e.to_string()))?;
                tx.put_event(ctx, seq, &json)?;
                for path in self.expand_rescan(&tx, &ev)? {
                    tx.put_pending(
                        &PendingKey {
                            context: ctx.to_string(),
                            repo: repo.clone(),
                            path,
                        },
                        seq,
                    )?;
                }
            }
        }
        if changed {
            tx.commit()?;
        }
        Ok(())
    }

    /// Decide what one queued path needs, from the file's current content.
    fn plan(&self, snap: &ReadTx, key: &PendingKey, seq: u64) -> Result<Action, EngineError> {
        let engine = self.engine;
        let dir = engine.dir();
        let Some(root) = engine.roots().get(&key.repo) else {
            return Ok(Action::Skip);
        };
        let file = root.join(key.path.as_str());
        let fin = relative_shard(dir.root(), &dir.shard_path(&key.repo, &key.path));
        let content = match std::fs::read(&file) {
            Ok(c) => Some(c),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(EngineError::io(&file, e)),
        };
        let existing = dir.read_shard(&key.repo, &key.path).unwrap_or_default();
        let Some(content) = content else {
            let has_shard = dir.shard_path(&key.repo, &key.path).exists();
            return Ok(Action::Delete {
                fin: has_shard.then_some(fin),
            });
        };
        let Some(extractor) = engine.registry().for_path(&key.path) else {
            return Ok(Action::Skip);
        };
        let ctx = snap
            .ext_context(&key.repo, extractor.id())?
            .map(|(_, bytes)| bytes)
            .unwrap_or_default();
        let revision = context::revision(&git_blob_id(&content), &ctx);
        if existing.is_some_and(|f| f.provenance.revision == revision) {
            return Ok(Action::Skip);
        }
        match extractor.extract(&ExtractInput {
            repo: &key.repo,
            path: &key.path,
            revision: &revision,
            content: &content,
            context: &ctx,
        }) {
            Ok(fragment) => {
                let leaf = Utf8Path::new(&fin).file_name().unwrap_or("shard");
                let parent = Utf8Path::new(&fin).parent().unwrap_or(Utf8Path::new(""));
                Ok(Action::Replace {
                    fragment: Box::new(fragment),
                    tmp: parent.join(format!(".tmp-{seq}-{leaf}")).to_string(),
                    fin,
                })
            }
            Err(e) => Ok(Action::Failed(e.to_string())),
        }
    }
}

fn relative_shard(root: &Utf8Path, full: &Utf8Path) -> String {
    full.strip_prefix(root).unwrap_or(full).to_string()
}

fn write_synced(path: &Utf8Path, bytes: &[u8]) -> Result<(), EngineError> {
    use std::io::Write;
    let io = |e| EngineError::io(path, e);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| EngineError::io(parent, e))?;
    }
    let mut f = File::create(path).map_err(io)?;
    f.write_all(bytes).map_err(io)?;
    f.sync_all().map_err(io)
}

fn walk(
    dir: &Utf8Path,
    prefix: &str,
    engine: &Engine,
    out: &mut BTreeSet<RelPath>,
) -> Result<(), EngineError> {
    let rd = std::fs::read_dir(dir).map_err(|e| EngineError::io(dir, e))?;
    for e in rd {
        let e = e.map_err(|e| EngineError::io(dir, e))?;
        let Some(name) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let ft = e.file_type().map_err(|e| EngineError::io(dir, e))?;
        let rel = format!("{prefix}{name}");
        if ft.is_dir() {
            if name != ".git" {
                walk(&dir.join(&name), &format!("{rel}/"), engine, out)?;
            }
        } else if ft.is_file()
            && let Ok(p) = RelPath::new(&rel)
            && engine.registry().for_path(&p).is_some()
        {
            out.insert(p);
        }
    }
    Ok(())
}
