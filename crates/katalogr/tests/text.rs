//! Text fallback through the CLI (FEAT-066): routing by profile `select`, BM25 search and
//! shard removal on deselect.

mod textcli {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn run(dir: &Path, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(args)
            .output()
            .expect("run katalogr");
        (
            out.status.code().expect("exit code"),
            String::from_utf8(out.stdout).expect("utf8"),
            String::from_utf8(out.stderr).expect("utf8"),
        )
    }

    fn ok(dir: &Path, args: &[&str]) -> String {
        let (code, out, err) = run(dir, args);
        assert_eq!(code, 0, "{args:?}: {out}{err}");
        out
    }

    const NOTES: &str = "\
# Ledger notes

The ledger tracks pending invoices and their reminders.

## Reminders

Overdue invoices receive a reminder after fourteen days.
The tortoise schedule runs weekly.
";

    const PROFILE: &str = "\
profile: text-notes
version: 2
select:
  - { extension: text, path: 'notes/*.txt' }
rules: []
";

    /// A project `demo` with one marked repo `app` holding `notes/notes.txt`; returns the
    /// kindex and the repo.
    fn project(tmp: &Path) -> (PathBuf, PathBuf) {
        let kx = tmp.join("kx");
        let repo = tmp.join("app");
        std::fs::create_dir_all(repo.join("notes")).unwrap();
        std::fs::write(repo.join("notes/notes.txt"), NOTES).unwrap();
        ok(tmp, &["kindex", "init", kx.to_str().unwrap()]);
        let spec = format!("app={}", repo.display());
        ok(
            tmp,
            &[
                "--kindex",
                kx.to_str().unwrap(),
                "project",
                "init",
                "demo",
                "--repo",
                &spec,
                "--mark",
            ],
        );
        (kx, repo)
    }

    fn profile_path(kx: &Path) -> PathBuf {
        kx.join("projects/demo/profiles/text-notes.yaml")
    }

    fn install_profile(kx: &Path) {
        let p = profile_path(kx);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, PROFILE).unwrap();
    }

    fn shard(kx: &Path) -> PathBuf {
        kx.join("projects/demo/shards/app/notes/notes.txt.kshard.json")
    }

    #[test]
    fn default_index_skips_unclaimed_files() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = project(tmp.path());
        ok(&repo, &["index"]);
        assert!(!shard(&kx).exists(), "no profile routes the file");
        let (code, out, err) = run(&repo, &["search", "tortoise"]);
        assert_eq!(code, 4, "{out}{err}");
        assert!(!out.contains("notes.txt"), "{out}");
    }

    #[test]
    fn select_routes_file_to_text() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = project(tmp.path());
        install_profile(&kx);
        ok(&repo, &["index"]);
        let text = std::fs::read_to_string(shard(&kx)).expect("a shard for the routed file");
        assert!(text.contains("\"extension\": \"text\""), "{text}");
        assert_eq!(text.matches("\"kind\": \"chunk\"").count(), 2, "{text}");
        assert!(
            text.contains("#chunk:l1") && text.contains("#chunk:l5"),
            "{text}"
        );
        assert!(text.contains("\"text\""), "each chunk carries a text fact");
    }

    #[test]
    fn search_finds_chunk_by_content() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = project(tmp.path());
        install_profile(&kx);
        ok(&repo, &["index"]);
        // A word that appears only in the second block.
        let out = ok(&repo, &["search", "tortoise"]);
        assert!(out.contains("notes/notes.txt"), "{out}");
        assert!(out.contains("chunk:l5"), "{out}");
        assert!(!out.contains("chunk:l1"), "{out}");
    }

    #[test]
    fn deselect_removes_shard() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = project(tmp.path());
        install_profile(&kx);
        ok(&repo, &["index"]);
        assert!(shard(&kx).exists());
        std::fs::remove_file(profile_path(&kx)).unwrap();
        ok(&repo, &["index"]);
        assert!(
            !shard(&kx).exists(),
            "the shard of a deselected file is removed"
        );
        let (code, out, err) = run(&repo, &["search", "tortoise"]);
        assert_eq!(code, 4, "{out}{err}");
    }
}
