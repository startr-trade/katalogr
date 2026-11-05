//! `katalogr render` through the CLI (FEAT-032): every format, the default, the exit code of a
//! bad format, byte-stable output, partial views in JSON, and no need for a board.

mod render {
    use serde_json::Value;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn run_env(dir: &Path, args: &[&str], path_env: Option<&str>) -> (i32, String, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_katalogr"));
        cmd.current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .env_remove("KANBANR_DATA_DIR")
            .args(args);
        if let Some(p) = path_env {
            cmd.env("PATH", p);
        }
        let out = cmd.output().expect("run katalogr");
        (
            out.status.code().expect("exit code"),
            String::from_utf8(out.stdout).expect("utf8"),
            String::from_utf8(out.stderr).expect("utf8"),
        )
    }

    fn run(dir: &Path, args: &[&str]) -> (i32, String, String) {
        run_env(dir, args, None)
    }

    fn ok(dir: &Path, args: &[&str]) -> String {
        let (code, out, err) = run(dir, args);
        assert_eq!(code, 0, "{args:?}: {out}{err}");
        out
    }

    /// A project `demo` with one repo `app` holding a chain of three files, indexed.
    fn project(tmp: &Path) -> PathBuf {
        let kx = tmp.join("kx");
        let repo = tmp.join("app");
        std::fs::create_dir_all(&repo).unwrap();
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
        for (name, text) in [
            ("f1.tx", "sym fn a\nref a calls x:b\n"),
            ("f2.tx", "sym fn b\nexport x:b b\nref b calls x:c\n"),
            ("f3.tx", "sym fn c\nexport x:c c\n"),
        ] {
            std::fs::write(repo.join(name), text).unwrap();
        }
        ok(&repo, &["index"]);
        repo
    }

    /// R-1: every format writes its own kind of text.
    #[test]
    fn formats_all_emit() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        let marks = [
            ("mermaid", "flowchart LR"),
            ("dot", "digraph katalogr"),
            ("graphml", "<graphml"),
            ("structurizr", "workspace"),
            ("svg", "<svg"),
        ];
        for (format, mark) in marks {
            let out = ok(
                &repo,
                &["render", "structure", "--level", "file", "--format", format],
            );
            assert!(out.contains(mark), "{format}: {out}");
        }
    }

    /// R-1: with no `--format`, the output is Mermaid.
    #[test]
    fn mermaid_is_default() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        let out = ok(&repo, &["render", "structure", "--level", "file"]);
        assert!(out.starts_with("flowchart LR\n"), "{out}");
        assert!(out.contains("%% katalogr view: structure"), "{out}");
        assert!(out.contains("-->"), "{out}");
    }

    /// R-1: a format that does not exist is a usage error, exit 2.
    #[test]
    fn unknown_format_exit_2() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        let (code, out, _) = run(&repo, &["render", "structure", "--format", "pdf"]);
        assert_eq!(code, 2);
        assert!(out.is_empty());
        let (code, _, err) = run(&repo, &["render", "nonsense"]);
        assert_eq!(code, 2, "an unknown op is a usage error: {err}");
    }

    /// R-3: the same index gives the same bytes, in every format.
    #[test]
    fn same_index_same_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        for format in ["mermaid", "dot", "graphml", "structurizr", "svg"] {
            let args = ["render", "structure", "--level", "file", "--format", format];
            let first = ok(&repo, &args);
            for _ in 0..3 {
                assert_eq!(first, ok(&repo, &args), "{format}");
            }
        }
    }

    /// R-4: a cut view says so in the JSON envelope and in the diagram.
    #[test]
    fn partial_in_json() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        let out = ok(
            &repo,
            &[
                "--json",
                "render",
                "structure",
                "--level",
                "file",
                "--max-nodes",
                "2",
            ],
        );
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["result"]["shown"], 2, "{v}");
        assert_eq!(v["result"]["total"], 3, "{v}");
        assert_eq!(v["partial"][0]["cap"], "limit", "{v}");
        assert!(v["result"]["nodes"].as_array().unwrap().len() == 2);
        let text = ok(
            &repo,
            &["render", "structure", "--level", "file", "--max-nodes", "2"],
        );
        assert!(text.contains("2 of 3 nodes shown"), "{text}");
    }

    /// R-14: nothing here needs a board or any other tool on the PATH.
    #[test]
    fn runs_without_board() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        let (code, out, err) =
            run_env(&repo, &["render", "structure", "--level", "file"], Some(""));
        assert_eq!(code, 0, "{out}{err}");
        assert!(out.starts_with("flowchart LR"), "{out}");
    }
}
