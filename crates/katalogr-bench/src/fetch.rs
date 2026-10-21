//! Pinned shallow clones and commit replay checkouts.

use camino::Utf8Path;

use crate::error::{BenchError, Result};
use crate::git::Git;
use crate::manifest::{Corpus, RepoPin};
use crate::workspace::Workspace;

#[derive(Debug, Clone, PartialEq)]
pub struct Fetched {
    pub repo: String,
    /// The pinned commit.
    pub head: String,
    /// The commit to index cold: the oldest replay commit of the primary repository, else head.
    pub base: String,
    /// The first-parent chain `base ..= head` (oldest first); `[head]` for other repositories.
    pub replay: Vec<String>,
}

fn head_of(git: &Git, dir: &Utf8Path) -> Option<String> {
    git.run(dir, &["rev-parse", "HEAD"]).ok()
}

/// For each repository: reuse a correct existing clone (no fetch); re-check-out one that has the
/// commit but another HEAD; delete a broken one; otherwise `git init`, add the remote, fetch the
/// pinned commit with its depth and no tags, and check it out detached. Verifies
/// `rev-parse HEAD == commit`. For the primary repository `replay` is the first-parent chain of
/// `replay + 1` commits ending at the pin.
pub fn fetch(ws: &Workspace, c: &Corpus, git: &Git) -> Result<Vec<Fetched>> {
    let mut out = Vec::new();
    for r in &c.repos {
        let dir = ws.clone_dir(&c.name, &r.name);
        ensure_clone(git, &dir, r)?;
        let replay = if r.name == c.primary && c.replay > 0 {
            let n = (c.replay + 1).to_string();
            let chain = git.run(
                &dir,
                &[
                    "rev-list",
                    "--first-parent",
                    &format!("--max-count={n}"),
                    &r.commit,
                ],
            )?;
            let mut v: Vec<String> = chain.lines().map(str::to_string).collect();
            v.reverse();
            if v.len() < 2 {
                return Err(BenchError::Git(format!(
                    "repository {} has {} reachable commit(s); replay {} needs {}",
                    r.name,
                    v.len(),
                    c.replay,
                    c.replay + 1
                )));
            }
            v
        } else {
            vec![r.commit.clone()]
        };
        out.push(Fetched {
            repo: r.name.clone(),
            head: r.commit.clone(),
            base: replay[0].clone(),
            replay,
        });
    }
    Ok(out)
}

fn ensure_clone(git: &Git, dir: &Utf8Path, r: &RepoPin) -> Result<()> {
    if dir.join(".git").exists() {
        match head_of(git, dir) {
            Some(h) if h == r.commit => return Ok(()),
            Some(_) => {
                if git
                    .run(
                        dir,
                        &["cat-file", "-e", &format!("{}^{{commit}}", r.commit)],
                    )
                    .is_ok()
                {
                    return checkout(git, dir, &r.commit);
                }
                // A clone that lacks the pin is useless: re-clone below.
                std::fs::remove_dir_all(dir)?;
            }
            None => std::fs::remove_dir_all(dir)?,
        }
    } else if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir)?;
    let go = || -> Result<()> {
        git.run(dir, &["init", "-q"])?;
        git.run(dir, &["remote", "add", "origin", &r.url])?;
        let depth = r.depth.to_string();
        git.run(
            dir,
            &[
                "fetch",
                "-q",
                "--depth",
                &depth,
                "--no-tags",
                "origin",
                &r.commit,
            ],
        )?;
        checkout(git, dir, &r.commit)
    };
    if let Err(e) = go() {
        let _ = std::fs::remove_dir_all(dir);
        return Err(e);
    }
    let head = head_of(git, dir).unwrap_or_default();
    if head != r.commit {
        return Err(BenchError::Git(format!(
            "repository {}: HEAD is {head}, expected the pinned {}",
            r.name, r.commit
        )));
    }
    Ok(())
}

/// `git checkout --detach --force <sha>` plus `git clean -fdq`. The `.katalogr` marker survives
/// the clean, because indexing a replay range finds its project through it.
pub fn checkout(git: &Git, dir: &Utf8Path, sha: &str) -> Result<()> {
    git.run(dir, &["checkout", "-q", "--detach", "--force", sha])?;
    git.run(dir, &["clean", "-fdq", "-e", ".katalogr"])?;
    Ok(())
}
