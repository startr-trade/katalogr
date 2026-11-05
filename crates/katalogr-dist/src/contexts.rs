//! Contexts on the `Katalogr` facade: the base and its advance (FEAT-043).

use crate::{Consistency, DistError, Katalogr, Options, Report};
use katalogr_engine::base::{branch_tip, default_branch, plan_base_advance};
use katalogr_engine::{FileSource, scan::ScanError};
use katalogr_store::Reader;
use std::time::Instant;

/// `meta` key holding the commit the base reflects for `repo`, as hex.
pub fn base_commit_key(repo: &str) -> String {
    format!("base_commit.{repo}")
}

impl Katalogr {
    /// The base cache's view of a repo: the commit the base reflects, if recorded.
    pub fn base_commit(&self, repo: &str, opts: &Options) -> Result<Option<String>, DistError> {
        let engine = self.engine(Instant::now() + opts.timeout()?)?;
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
            let mut engine = self.engine(Instant::now() + timeout)?;
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
                let engine = self.engine(Instant::now() + timeout)?;
                self.read(&engine, Consistency::StaleOk, timeout, 0, Vec::new())?
            }
        };
        report.queued = queued;
        report.warnings.extend(warnings);
        Ok(report)
    }
}
