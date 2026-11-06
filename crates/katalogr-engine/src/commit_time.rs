//! The time of a commit, for the timeline (FEAT-033). A read that fails is `None`, never an error:
//! the caller shows the point with no date.

use std::path::Path;

/// The committer time of `commit` in the repository at `root`, in Unix seconds. None when the
/// repository or the commit cannot be read: the caller shows the point with no date.
pub fn commit_time(root: &Path, commit: &str) -> Option<i64> {
    let repo = gix::open(root).ok()?;
    let id = gix::ObjectId::from_hex(commit.as_bytes()).ok()?;
    let commit = repo.find_commit(id).ok()?;
    let time = commit.time().ok()?;
    Some(time.seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout)
            .expect("utf8")
            .trim()
            .to_string()
    }

    #[test]
    fn reads_commit_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q"]);
        git(
            dir.path(),
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "first",
            ],
        );
        let head = git(dir.path(), &["rev-parse", "HEAD"]);
        assert_eq!(commit_time(dir.path(), &head), Some(1_700_000_000));
    }

    #[test]
    fn unknown_commit_is_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q"]);
        // A well-formed ID that names no object, a malformed one, and a folder with no repository.
        assert_eq!(commit_time(dir.path(), &"0".repeat(40)), None);
        assert_eq!(commit_time(dir.path(), "not-a-commit"), None);
        let plain = tempfile::tempdir().expect("tempdir");
        assert_eq!(commit_time(plain.path(), &"0".repeat(40)), None);
    }
}
