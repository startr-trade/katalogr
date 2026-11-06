//! `katalogr shard show` (FEAT-094 R-13): print the text form of one file's fragment.
//!
//! The fragment is extracted as the planner would, from the working tree or from a commit's
//! blob, with the context built over the same source and the project's profiles applied. Nothing
//! is stored: no write transaction is opened and no event is queued.

use crate::error::DistError;
use crate::{DEFAULT_TIMEOUT_SECS, Katalogr};
use katalogr_engine::{Engine, FileSource};
use katalogr_model::RelPath;
use std::time::{Duration, Instant};

fn refused(message: impl Into<String>) -> DistError {
    DistError::Refused {
        exit: 4,
        message: message.into(),
    }
}

impl Katalogr {
    /// The shard text (format 1) of `path` in `repo`, read from the working tree, or from
    /// `commit` when given. A usage error for an unknown repo, a repo with no root here or a bad
    /// path; a refusal with status 4 when the file is absent or no extractor handles it.
    pub fn shard_show(
        &self,
        repo: &str,
        path: &str,
        commit: Option<&str>,
    ) -> Result<String, DistError> {
        if !self.project.config().repos.iter().any(|r| r.name == repo) {
            return Err(DistError::usage(format!(
                "{repo}: not a repository of this project"
            )));
        }
        let root = self
            .roots
            .get(repo)
            .ok_or_else(|| crate::unknown_root(repo))?
            .clone();
        let rel = RelPath::new(path).map_err(|e| DistError::usage(format!("{path}: {e}")))?;
        let deadline = Instant::now() + Duration::from_secs_f64(DEFAULT_TIMEOUT_SECS);
        let dir = self.layer_dir(deadline)?;
        let mut engine = match self.reader_engine(dir.clone())? {
            Some(e) => e,
            None => self.engine_on(dir, deadline)?,
        };
        if let Some(commit) = commit {
            engine.set_source(
                repo,
                FileSource::Commit {
                    repo_root: root,
                    commit: commit.to_string(),
                },
            );
        }
        self.show_with(&engine, repo, &rel)
    }

    fn show_with(&self, engine: &Engine, repo: &str, rel: &RelPath) -> Result<String, DistError> {
        let source = engine
            .source(repo)
            .ok_or_else(|| crate::unknown_root(repo))?;
        let content = source
            .read(rel)?
            .ok_or_else(|| refused(format!("{repo}:{rel}: no such file")))?;
        let profiles = engine.routing_profiles()?;
        let extraction =
            katalogr_engine::queue::extract_for_show(engine, repo, rel, &content, &profiles)?
                .ok_or_else(|| refused(format!("{repo}:{rel}: no extractor handles this file")))?;
        let bytes = katalogr_store::shard::encode(&extraction.fragment)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}
