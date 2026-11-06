//! The change range shared by `docs-affected` and `impact` (FEAT-086, FEAT-028): the files and
//! line ranges git reports between two commits, or between a commit and the working tree.

use crate::{DistError, Katalogr, Options};
use katalogr_engine::base::{branch_tip, default_branch, merge_base, resolve_commit};
use katalogr_engine::scan::{ChangeKind, ScanError, changed_hunks};
use katalogr_query::{ChangedFile, PositionRef};
use std::time::Instant;

/// The changes of one repository between `from` and the end of the range.
pub(crate) struct ChangeRange {
    /// The marked repository the changes belong to.
    pub repo: String,
    /// The commit the range starts at.
    pub from: String,
    pub changes: Vec<ChangedFile>,
}

pub(crate) fn bad_rev(e: ScanError) -> DistError {
    match e {
        ScanError::BadRevision { rev, message } => {
            DistError::usage(format!("unknown revision `{rev}`: {message}"))
        }
        other => other.into(),
    }
}

fn convert(changes: Vec<katalogr_engine::scan::FileChange>) -> Vec<ChangedFile> {
    changes
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
        .collect()
}

impl Katalogr {
    /// The changes since `since` (a revision or a clean position; default the merge-base of
    /// `HEAD` and the default branch) up to `opts.at` (default this checkout's position, so
    /// uncommitted changes count). A dirty `since` is a usage error, since old uncommitted
    /// content cannot be diffed.
    pub(crate) fn change_range(
        &self,
        since: Option<&str>,
        opts: &Options,
        deadline: Instant,
    ) -> Result<ChangeRange, DistError> {
        let (repo, root) = self.current()?;
        let root = root.as_std_path();

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

        let changes = convert(changed_hunks(root, &from, to.as_deref()).map_err(bad_rev)?);
        Ok(ChangeRange {
            repo: repo.to_string(),
            from,
            changes,
        })
    }

    /// `base..head`, both resolved to commits in the marked repository, with the head commit.
    /// Text without `..`, or with `...`, is a usage error.
    pub(crate) fn range_changes(&self, text: &str) -> Result<(ChangeRange, String), DistError> {
        let usage = || DistError::usage(format!("--range {text}: expected BASE..HEAD"));
        let (base, head) = text.split_once("..").ok_or_else(usage)?;
        if base.is_empty() || head.is_empty() || head.starts_with('.') || head.contains("..") {
            return Err(usage());
        }
        let (repo, root) = self.current()?;
        let root = root.as_std_path();
        let from = resolve_commit(root, base).map_err(bad_rev)?;
        let head = resolve_commit(root, head).map_err(bad_rev)?;
        let changes = convert(changed_hunks(root, &from, Some(&head)).map_err(bad_rev)?);
        Ok((
            ChangeRange {
                repo: repo.to_string(),
                from,
                changes,
            },
            head,
        ))
    }
}
