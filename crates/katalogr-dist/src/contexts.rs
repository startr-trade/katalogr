//! Contexts on the `Katalogr` facade: the base and its advance (FEAT-043).

use crate::{Consistency, DistError, Katalogr, Options, Report};
use camino::Utf8Path;
use katalogr_engine::base::{branch_tip, default_branch, plan_base_advance};
use katalogr_engine::queue::Inbox;
use katalogr_engine::queue::lock::lock_until;
use katalogr_engine::{FileSource, NewEvent, scan::ScanError};
use katalogr_store::{ContextId, ContextKind, ContextManifest, ProjectDir, Reader, seed_layer};
use std::time::Instant;

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

impl Katalogr {
    pub fn set_context(&mut self, c: ContextChoice) {
        self.context = c;
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
        let touch = |layer: &ProjectDir| {
            let _ = std::fs::File::create(layer.root().join(".used"));
        };
        let id = match &self.context {
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
                touch(&layer);
                return Ok(layer);
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
            touch(&layer);
            return Ok(layer);
        }
        self.seed(&layer, &id, repo, &canon, deadline)
    }

    fn seed(
        &self,
        layer: &ProjectDir,
        id: &ContextId,
        repo: &str,
        root: &Utf8Path,
        deadline: Instant,
    ) -> Result<ProjectDir, DistError> {
        let Some(lock) = lock_until(&self.project.cache_dir(), deadline)? else {
            self.note(format!(
                "context {id} not seeded: the base is busy; answered from the base"
            ));
            return Ok(self.project.clone());
        };
        // Another process may have seeded it while this one waited.
        if !layer.root().join(ContextManifest::FILE).is_file() {
            let m = ContextManifest {
                format: ContextManifest::FORMAT,
                id: id.clone(),
                kind: ContextKind::Checkout,
                repo: repo.to_string(),
                root: Some(root.to_owned()),
                seed_commit: None,
            };
            seed_layer(&self.project, layer, &m, deadline)?;
            drop(lock);
            // The checkout may differ from the seed in ways only a listing shows; unchanged
            // files cost a read and a hash, never an extraction.
            Inbox::new(layer).append(&NewEvent::rescan(repo))?;
        }
        let _ = std::fs::File::create(layer.root().join(".used"));
        Ok(layer.clone())
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
