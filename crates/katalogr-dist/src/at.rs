//! `--at <position>`: which context answers for a position (FEAT-084).

use crate::contexts::{ContextChoice, base_commit_key};
use crate::{DistError, Katalogr};
use katalogr_engine::Engine;
use katalogr_engine::consistency::recorded_position;
use katalogr_query::PositionRef;
use katalogr_store::{ContextId, ContextKind, ContextManifest, ProjectDir, Reader};
use std::time::Instant;

/// Where a position can be answered.
pub(crate) enum Resolved {
    /// The current context's own live position: read as usual.
    Current,
    /// Another context holds it as its recorded position.
    Context {
        choice: ContextChoice,
        wanted: PositionRef,
    },
    /// No context does.
    Unavailable {
        wanted: String,
        available: Vec<PositionRef>,
    },
}

impl Katalogr {
    /// The repo a `--at` text is parsed for: the named context's, else the marked repo's.
    pub(crate) fn at_repo(&self) -> Result<String, DistError> {
        if let ContextChoice::Named(id) = &self.context
            && id.as_str() != ContextId::BASE
            && let Ok(m) = ContextManifest::read(self.project.layer(id).root())
        {
            return Ok(m.repo);
        }
        self.repo.clone().ok_or_else(|| {
            DistError::usage(
                "--at needs a repository: run inside a marked repository or name a context",
            )
        })
    }

    /// An engine to look at a context's cache, writer or reader, whichever is to hand.
    fn peek(&self, dir: ProjectDir, deadline: Instant) -> Result<Engine, DistError> {
        match self.reader_engine(dir.clone())? {
            Some(e) => Ok(e),
            None => self.engine_on(dir, deadline),
        }
    }

    /// The position `repo` last reached in the context at `dir`, if it has one.
    fn recorded_in(
        &self,
        dir: ProjectDir,
        manifest: Option<&ContextManifest>,
        repo: &str,
        deadline: Instant,
    ) -> Result<Option<PositionRef>, DistError> {
        let clean = |commit: Option<String>| {
            commit.map(|c| PositionRef {
                repo: repo.to_string(),
                commit: Some(c),
                dirty: None,
            })
        };
        match manifest {
            Some(m) if m.repo != repo => Ok(None),
            Some(m) if m.kind == ContextKind::Commit => Ok(clean(m.commit.clone())),
            Some(_) => {
                let engine = self.peek(dir, deadline)?;
                let snap = engine.cache().read()?;
                Ok(recorded_position(&snap, repo)?)
            }
            None => {
                let engine = self.peek(dir, deadline)?;
                let snap = engine.cache().read()?;
                Ok(clean(
                    snap.meta(&base_commit_key(repo))?
                        .and_then(|b| String::from_utf8(b).ok()),
                ))
            }
        }
    }

    /// FEAT-084 `--at`: parse `text`; if it is this context's live position read as usual, else
    /// find the context that holds it (the base for a clean base commit, then the layers in id
    /// order), else report every position that is held.
    pub(crate) fn resolve_at(&self, text: &str, deadline: Instant) -> Result<Resolved, DistError> {
        let repo = self.at_repo()?;
        let wanted =
            PositionRef::parse(&repo, text).map_err(|e| DistError::usage(e.to_string()))?;
        let mut available: Vec<PositionRef> = Vec::new();

        let own = self.engine_for_choice(crate::Consistency::StaleOk, deadline, &self.context)?;
        if own.roots().contains_key(&repo) {
            let live = own.live_position(&repo)?;
            if live == wanted {
                return Ok(Resolved::Current);
            }
            available.push(live);
        }
        drop(own);

        if let Some(p) = self.recorded_in(self.project.clone(), None, &repo, deadline)? {
            if p == wanted {
                return Ok(Resolved::Context {
                    choice: ContextChoice::Base,
                    wanted,
                });
            }
            available.push(p);
        }
        for m in self.project.contexts()? {
            let dir = self.project.layer(&m.id);
            if let Some(p) = self.recorded_in(dir, Some(&m), &repo, deadline)? {
                if p == wanted {
                    return Ok(Resolved::Context {
                        choice: ContextChoice::Named(m.id),
                        wanted,
                    });
                }
                available.push(p);
            }
        }
        available.sort();
        available.dedup();
        Ok(Resolved::Unavailable {
            wanted: text.to_string(),
            available,
        })
    }
}

impl Katalogr {
    /// FEAT-084 `katalogr position`: the position of `repo` (default: the marked repo) as it is
    /// now, and what this context's index reflects (read without waiting or reconciling).
    pub fn position(
        &self,
        repo: Option<&str>,
        opts: &crate::Options,
    ) -> Result<katalogr_query::PositionAnswer, DistError> {
        let repo = match repo {
            Some(r) => r.to_string(),
            None => self.at_repo()?,
        };
        if !self.roots.contains_key(&repo) {
            return Err(crate::unknown_root(&repo));
        }
        let timeout = opts.timeout()?;
        let deadline = Instant::now() + timeout;
        let engine = self.engine_for(crate::Consistency::StaleOk, deadline)?;
        let live = engine.live_position(&repo)?;
        let report = self.read(&engine, crate::Consistency::StaleOk, timeout, 0, Vec::new())?;
        let reflected: Vec<PositionRef> = report.positions;
        let exact = reflected.iter().any(|p| p == &live) && !report.stale;
        Ok(katalogr_query::PositionAnswer {
            schema: katalogr_query::RESULT_SCHEMA_VERSION,
            text: live.text(),
            live,
            reflected,
            exact,
        })
    }
}

/// `katalogr position` with no board: the position of the git checkout that holds `dir`, named
/// after its directory, with nothing reflected.
pub fn position_without_board(
    dir: &std::path::Path,
) -> Result<katalogr_query::PositionAnswer, DistError> {
    let root = katalogr_engine::position::checkout_root(dir)
        .ok_or_else(|| DistError::usage("not inside a git checkout, and no project was found"))?;
    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo")
        .to_string();
    let live = katalogr_engine::position::read_position(&name, &root, &["**".to_string()], &[])
        .map_err(|e| DistError::usage(e.to_string()))?
        .position;
    Ok(katalogr_query::PositionAnswer {
        schema: katalogr_query::RESULT_SCHEMA_VERSION,
        text: live.text(),
        live,
        reflected: Vec::new(),
        exact: false,
    })
}
