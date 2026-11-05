//! `docs-affected`: which documents are linked to code that changed (FEAT-086).

use crate::{DistError, Katalogr, Options, Outcome, Reply};
use katalogr_engine::base::{branch_tip, default_branch, merge_base, resolve_commit};
use katalogr_engine::scan::{ChangeKind, ScanError, changed_hunks};
use katalogr_query::{ChangedFile, DOC_LINK_KINDS, DocImpact, DocImpactInput, PositionRef};
use std::time::Instant;

/// Default number of documents reported.
pub const DEFAULT_DOCS_LIMIT: usize = 200;

/// What a `docs-affected` question adds to the read options.
#[derive(Debug, Clone)]
pub struct DocsAffectedParams {
    /// Link kinds that connect a document to code; empty means the `docs.link_kinds` project
    /// setting, else [`DOC_LINK_KINDS`].
    pub kinds: Vec<String>,
    /// Most documents to report.
    pub limit: usize,
}

impl Default for DocsAffectedParams {
    fn default() -> Self {
        DocsAffectedParams {
            kinds: Vec::new(),
            limit: DEFAULT_DOCS_LIMIT,
        }
    }
}

fn bad_rev(e: ScanError) -> DistError {
    match e {
        ScanError::BadRevision { rev, message } => {
            DistError::usage(format!("unknown revision `{rev}`: {message}"))
        }
        other => other.into(),
    }
}

impl Katalogr {
    /// The `docs.link_kinds` setting, else [`DOC_LINK_KINDS`].
    fn doc_link_kinds(&self) -> Vec<String> {
        self.project
            .config()
            .settings
            .get("docs.link_kinds")
            .and_then(|v| v.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DOC_LINK_KINDS.iter().map(|k| k.to_string()).collect())
    }

    /// Documents linked to code that changed since `since` (a revision or a clean position;
    /// default the merge-base of `HEAD` and the default branch) up to `opts.at` (default this
    /// checkout's position, so uncommitted changes count). A dirty `since` is a usage error,
    /// since old uncommitted content cannot be diffed.
    pub fn docs_affected(
        &self,
        since: Option<&str>,
        params: &DocsAffectedParams,
        opts: &Options,
    ) -> Result<Reply<DocImpact>, DistError> {
        let (repo, root) = self.current()?;
        let root = root.as_std_path();
        let deadline = Instant::now() + opts.timeout()?;

        // The end of the range.
        let to: Option<String> = match opts.at.as_deref() {
            None => None,
            Some(text) => {
                let wanted =
                    PositionRef::parse(repo, text).map_err(|e| DistError::usage(e.to_string()))?;
                let engine = self.engine_for(crate::Consistency::StaleOk, deadline)?;
                let live = engine.live_position(repo)?;
                drop(engine);
                if wanted == live {
                    None
                } else if wanted.dirty.is_some() {
                    return Err(DistError::usage(format!(
                        "--at {text}: a dirty position other than this checkout's cannot be diffed"
                    )));
                } else {
                    wanted.commit.clone()
                }
            }
        };

        // The start of the range.
        let from: String = match since {
            Some(text) => match PositionRef::parse(repo, text) {
                Ok(p) if p.dirty.is_some() => {
                    return Err(DistError::usage(format!(
                        "--since {text}: a dirty position cannot be diffed; name a revision or a clean position"
                    )));
                }
                Ok(PositionRef {
                    commit: Some(c), ..
                }) => c,
                _ => resolve_commit(root, text).map_err(bad_rev)?,
            },
            None => {
                let configured = self
                    .project
                    .config()
                    .settings
                    .get(&format!("base.branch.{repo}"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let branch = default_branch(root, configured.as_deref())?;
                let tip = match branch {
                    Some(b) => branch_tip(root, &b)?,
                    None => None,
                };
                let Some(tip) = tip else {
                    return Err(DistError::usage(
                        "no default branch to compare against: pass --since <revision>",
                    ));
                };
                let end = to.as_deref().unwrap_or("HEAD");
                merge_base(root, end, &tip)?.ok_or_else(|| {
                    DistError::usage(
                        "no common history with the default branch: pass --since <revision>",
                    )
                })?
            }
        };

        let changes: Vec<ChangedFile> = changed_hunks(root, &from, to.as_deref())
            .map_err(bad_rev)?
            .into_iter()
            .map(|c| ChangedFile {
                path: c.path,
                kind: match c.kind {
                    ChangeKind::Added => "added",
                    ChangeKind::Modified => "modified",
                    ChangeKind::Deleted => "deleted",
                }
                .to_string(),
                ranges: c.ranges,
            })
            .collect();
        let kinds = if params.kinds.is_empty() {
            self.doc_link_kinds()
        } else {
            params.kinds.clone()
        };
        let repo = repo.to_string();
        let mut reply = self.query(opts, |tx| {
            Ok(Outcome::Found(katalogr_query::doc_impact(
                tx,
                &DocImpactInput {
                    repo: &repo,
                    changes: &changes,
                    kinds: &kinds,
                    limit: params.limit,
                },
            )?))
        })?;
        if let Outcome::Found(d) = &reply.outcome {
            reply.partial = d.partial.clone();
        }
        Ok(reply)
    }
}
