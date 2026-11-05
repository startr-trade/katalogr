//! Graph diff and conflict detection between contexts (FEAT-043 R-8, R-9).

mod context_diff {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn run(dir: &Path, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .env_remove("KATALOGR_CONTEXT")
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

    fn git(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    struct P {
        _tmp: tempfile::TempDir,
        repo: PathBuf,
    }

    /// A git project whose base holds `files`.
    fn project(files: &[(&str, &str)]) -> P {
        let tmp = tempfile::tempdir().unwrap();
        let kx = tmp.path().join("kx");
        let repo = tmp.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        ok(tmp.path(), &["kindex", "init", kx.to_str().unwrap()]);
        let spec = format!("app={}", repo.display());
        ok(
            tmp.path(),
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
        for (n, t) in files {
            std::fs::write(repo.join(n), t).unwrap();
        }
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c1"]);
        ok(&repo, &["index"]);
        P { _tmp: tmp, repo }
    }

    fn worktree(p: &P, name: &str) -> PathBuf {
        let wt = p.repo.with_file_name(name);
        git(
            &p.repo,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", name],
        );
        std::fs::copy(p.repo.join(".katalogr"), wt.join(".katalogr")).unwrap();
        wt
    }

    /// The id of the context of the checkout at `at`.
    fn id_of(at: &Path) -> String {
        let v: serde_json::Value =
            serde_json::from_str(&ok(at, &["--json", "context", "list"])).unwrap();
        v["result"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["current"] == true)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn diff(at: &Path, a: &str, b: &str, extra: &[&str]) -> serde_json::Value {
        let mut args = vec!["--json", "context", "diff", a, b];
        args.extend_from_slice(extra);
        let (code, out, err) = run(at, &args);
        assert_eq!(code, 0, "{out}{err}");
        serde_json::from_str(&out).unwrap()
    }

    const BASE: [(&str, &str); 2] = [
        ("a.tx", "sym fn A\nsym fn B\nlink B calls A\n"),
        ("c.tx", "sym fn C\n"),
    ];

    #[test]
    fn added_and_removed_links() {
        let p = project(&BASE);
        std::fs::write(
            p.repo.join("a.tx"),
            "sym fn A\nsym fn B\nsym fn D\nlink D calls A\n",
        )
        .unwrap();
        ok(&p.repo, &["notify", "a.tx"]);
        let me = id_of(&p.repo);
        let v = diff(&p.repo, "base", &me, &[]);
        let added = v["result"]["links_added"].as_array().unwrap();
        let removed = v["result"]["links_removed"].as_array().unwrap();
        assert_eq!(added.len(), 1, "{v}");
        assert_eq!(added[0]["from"], "app:a.tx#fn:D");
        assert_eq!(added[0]["to"], "app:a.tx#fn:A");
        assert_eq!(removed.len(), 1, "{v}");
        assert_eq!(removed[0]["from"], "app:a.tx#fn:B");
        // The other way round swaps the lists, and identical contexts differ in nothing.
        let w = diff(&p.repo, &me, "base", &[]);
        assert_eq!(w["result"]["links_added"], v["result"]["links_removed"]);
        let same = diff(&p.repo, "base", "base", &[]);
        assert!(same["result"]["links_added"].as_array().unwrap().is_empty());
        let text = ok(&p.repo, &["context", "diff", "base", &me]);
        assert!(
            text.contains("+ app:a.tx#fn:D calls app:a.tx#fn:A"),
            "{text}"
        );
    }

    #[test]
    fn new_dangling_reported() {
        let p = project(&BASE);
        std::fs::write(p.repo.join("c.tx"), "sym fn C\nref C links-to ns:ghost\n").unwrap();
        ok(&p.repo, &["notify", "c.tx"]);
        let me = id_of(&p.repo);
        let v = diff(&p.repo, "base", &me, &[]);
        let d = v["result"]["dangling_added"].as_array().unwrap();
        assert_eq!(d.len(), 1, "{v}");
        assert_eq!(d[0]["wanted"][0], "ns:ghost");
        assert!(
            v["result"]["dangling_removed"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn metric_deltas() {
        let p = project(&BASE);
        std::fs::write(
            p.repo.join("c.tx"),
            "sym fn C\nsym fn C2\nlink C2 calls C\nref C links-to ns:ghost\n",
        )
        .unwrap();
        ok(&p.repo, &["notify", "c.tx"]);
        let me = id_of(&p.repo);
        let v = diff(&p.repo, "base", &me, &[]);
        let m = |name: &str| -> serde_json::Value {
            v["result"]["metrics"]
                .as_array()
                .unwrap()
                .iter()
                .find(|x| x["name"] == name)
                .unwrap_or_else(|| panic!("{name}: {v}"))
                .clone()
        };
        assert_eq!(m("graph.symbols")["delta"], 1.0);
        assert_eq!(m("graph.links")["delta"], 1.0);
        assert_eq!(m("graph.dangling")["delta"], 1.0);
        assert_eq!(m("graph.dangling")["better"], "lower");
    }

    #[test]
    fn capped_is_partial() {
        let p = project(&BASE);
        std::fs::write(
            p.repo.join("c.tx"),
            "sym fn C\nsym fn C1\nsym fn C2\nsym fn C3\nlink C1 calls C\nlink C2 calls C\nlink C3 calls C\n",
        )
        .unwrap();
        ok(&p.repo, &["notify", "c.tx"]);
        let me = id_of(&p.repo);
        let v = diff(&p.repo, "base", &me, &["--limit", "2"]);
        assert_eq!(v["result"]["links_added"].as_array().unwrap().len(), 2);
        let partial = v["partial"].as_array().unwrap();
        assert_eq!(partial.len(), 1, "{v}");
        assert_eq!(partial[0]["cap"], "limit");
        assert_eq!(partial[0]["total"], 3);
    }

    #[test]
    fn conflicting_contracts_intersect() {
        let p = project(&[
            ("a.tx", "sym fn Prov\nbind Prov provides route:x\n"),
            ("c.tx", "sym fn Other\nbind Other provides route:y\n"),
        ]);
        let (w1, w2) = (worktree(&p, "w1"), worktree(&p, "w2"));
        // Both add a consumer of /x; only the second also adds one of /y.
        std::fs::write(
            w1.join("n1.tx"),
            "sym fn Use1\nbind Use1 consumes route:x\n",
        )
        .unwrap();
        std::fs::write(
            w2.join("n2.tx"),
            "sym fn Use2\nbind Use2 consumes route:x\nsym fn Use3\nbind Use3 consumes route:y\n",
        )
        .unwrap();
        ok(&w1, &["notify", "n1.tx"]);
        ok(&w2, &["notify", "n2.tx"]);
        let (i1, i2) = (id_of(&w1), id_of(&w2));
        let (code, out, err) = run(&p.repo, &["--json", "context", "conflicts", &i1, &i2]);
        assert_eq!(code, 1, "{out}{err}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let r = &v["result"];
        assert_eq!(r["conflicts"].as_array().unwrap().len(), 1, "{v}");
        assert!(r["conflicts"][0].as_str().unwrap().contains(":x"), "{v}");
        assert_eq!(r["changed_a"].as_array().unwrap().len(), 1);
        assert_eq!(r["changed_b"].as_array().unwrap().len(), 2);
        // A context against itself changed things, but two disjoint changes would not conflict.
        let (code, _, _) = run(&p.repo, &["context", "conflicts", &i1, "base"]);
        assert_eq!(code, 0);
    }
}
