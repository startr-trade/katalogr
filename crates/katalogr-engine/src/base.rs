//! The base: the shared index of a repo's default branch at one commit (FEAT-043).

use crate::queue::{EventKind, NewEvent};
use crate::scan::{PathChange, ScanError, changed_paths};
use std::path::Path;

fn open(repo_root: &Path) -> Result<gix::Repository, ScanError> {
    gix::open(repo_root).map_err(|e| ScanError::RepoOpen {
        path: repo_root.display().to_string(),
        message: e.to_string(),
    })
}

fn has_ref(repo: &gix::Repository, name: &str) -> bool {
    matches!(repo.try_find_reference(name), Ok(Some(_)))
}

/// `refs/remotes/origin/HEAD`, else `main`, else `master`, else `None`.
/// The project setting `base.branch.<repo>` (passed as `configured`) overrides all of them.
pub fn default_branch(
    repo_root: &Path,
    configured: Option<&str>,
) -> Result<Option<String>, ScanError> {
    if let Some(c) = configured {
        return Ok(Some(c.to_string()));
    }
    let repo = open(repo_root)?;
    if let Ok(Some(head)) = repo.try_find_reference("refs/remotes/origin/HEAD")
        && let gix::refs::TargetRef::Symbolic(name) = head.target()
        && let Some(branch) = name
            .as_bstr()
            .to_string()
            .strip_prefix("refs/remotes/origin/")
    {
        return Ok(Some(branch.to_string()));
    }
    for b in ["main", "master"] {
        if has_ref(&repo, &format!("refs/heads/{b}")) {
            return Ok(Some(b.to_string()));
        }
    }
    Ok(None)
}

/// The commit a branch points at: `refs/heads/<branch>`, else `refs/remotes/origin/<branch>`.
/// Full lower-case hex; `None` when neither exists.
pub fn branch_tip(repo_root: &Path, branch: &str) -> Result<Option<String>, ScanError> {
    let repo = open(repo_root)?;
    for name in [
        format!("refs/heads/{branch}"),
        format!("refs/remotes/origin/{branch}"),
    ] {
        if let Ok(Some(mut r)) = repo.try_find_reference(name.as_str())
            && let Ok(id) = r.peel_to_id()
        {
            return Ok(Some(id.to_hex().to_string()));
        }
    }
    Ok(None)
}

/// Events taking the base from `from` to `to`: a rescan when `from` is `None` or no longer
/// resolves (rewritten history).
pub fn plan_base_advance(
    repo: &str,
    repo_root: &Path,
    from: Option<&str>,
    to: &str,
) -> Result<Vec<NewEvent>, ScanError> {
    let Some(from) = from else {
        return Ok(vec![NewEvent::rescan(repo)]);
    };
    let changes = match changed_paths(repo_root, &format!("{from}..{to}")) {
        Ok(c) => c,
        Err(ScanError::BadRevision { .. }) => return Ok(vec![NewEvent::rescan(repo)]),
        Err(e) => return Err(e),
    };
    let mut events = Vec::new();
    for change in changes {
        match change {
            PathChange::Added(p) | PathChange::Modified(p) => {
                events.push(NewEvent::file(repo, &p, EventKind::Changed));
            }
            PathChange::Deleted(p) => events.push(NewEvent::file(repo, &p, EventKind::Deleted)),
            PathChange::Renamed { from, to } => {
                events.push(NewEvent::file(repo, &from, EventKind::Deleted));
                events.push(NewEvent::file(repo, &to, EventKind::Changed));
            }
        }
    }
    Ok(events)
}

/// Whether `HEAD` is the branch `branch` (not detached, not another branch).
pub fn head_is_branch(repo_root: &Path, branch: &str) -> bool {
    open(repo_root).ok().is_some_and(|r| {
        r.head_name()
            .ok()
            .flatten()
            .is_some_and(|n| n.as_bstr() == format!("refs/heads/{branch}").as_bytes())
    })
}

/// The full commit id a revision names in the repo, as lower-case hex.
pub fn resolve_commit(repo_root: &Path, rev: &str) -> Result<String, ScanError> {
    let repo = open(repo_root)?;
    let bad = |e: &dyn std::fmt::Display| ScanError::BadRevision {
        rev: rev.to_string(),
        message: e.to_string(),
    };
    let id = repo.rev_parse_single(rev).map_err(|e| bad(&e))?;
    let commit = id
        .object()
        .map_err(|e| bad(&e))?
        .peel_to_commit()
        .map_err(|e| bad(&e))?;
    Ok(commit.id().to_hex().to_string())
}

/// The merge-base of two revisions as lower-case hex; `None` when they share no history.
pub fn merge_base(repo_root: &Path, a: &str, b: &str) -> Result<Option<String>, ScanError> {
    let repo = open(repo_root)?;
    let a = resolve_commit(repo_root, a)?;
    let b = resolve_commit(repo_root, b)?;
    let id = |hex: &str| {
        gix::ObjectId::from_hex(hex.as_bytes()).map_err(|e| ScanError::BadRevision {
            rev: hex.to_string(),
            message: e.to_string(),
        })
    };
    Ok(repo
        .merge_base(id(&a)?, id(&b)?)
        .ok()
        .flatten()
        .map(|i| i.to_hex().to_string()))
}
