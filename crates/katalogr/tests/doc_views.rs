//! FEAT-035: query blocks in documents, checked and snapshotted through the CLI. The project is
//! a synthetic repository of `.tx` files; the documents are synthetic Markdown.

#[path = "views_support/mod.rs"]
mod support;

mod doc_views {
    use super::support::*;
    use serde_json::Value;
    use std::path::{Path, PathBuf};

    /// A docs folder holding `files` (relative path, text).
    fn docs(tmp: &Path, files: &[(&str, &str)]) -> PathBuf {
        let dir = tmp.join("docs");
        for (name, text) in files {
            let p = dir.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        dir
    }

    fn block(query: &str) -> String {
        format!("# Title\n\ntext\n\n```katalogr-query\n{query}\n```\n")
    }

    fn snapshot(repo: &Path, docs: &Path, out: &Path, extra: &[&str]) -> std::process::Output {
        let mut args = vec!["--json"];
        args.extend_from_slice(extra);
        args.extend_from_slice(&[
            "view",
            "snapshot",
            "--docs",
            docs.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ]);
        cli(repo, &args)
    }

    fn index_of(out: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(out.join("index.json")).unwrap()).unwrap()
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn head(repo: &Path) -> String {
        let out = std::process::Command::new("git")
            .current_dir(repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// R-1. Every bad block is reported with its file and line; the exit is 1.
    #[test]
    fn check_reports_bad_block() {
        let tmp = tempfile::tempdir().unwrap();
        let d = docs(
            tmp.path(),
            &[
                ("a.md", &block("structure")),
                ("sub/b.md", &block("nonsense --x")),
                (
                    "sub/c.md",
                    &format!("{}\n{}", block("cycles --limit 0"), block("path a")),
                ),
            ],
        );
        let out = cli(tmp.path(), &["view", "check", d.to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(text.contains("sub/b.md:5:"), "{text}");
        assert!(text.contains("sub/c.md:5:"), "{text}");
        assert!(text.contains("sub/c.md:13:"), "{text}");
        assert!(!text.contains("a.md:"), "{text}");
        let json = cli(
            tmp.path(),
            &["--json", "view", "check", d.to_str().unwrap()],
        );
        let v: Value = serde_json::from_slice(&json.stdout).unwrap();
        assert_eq!(v["problems"].as_array().unwrap().len(), 3, "{v}");
        assert_eq!(v["problems"][0]["file"], "sub/b.md");
    }

    /// R-1. A folder of good blocks exits 0.
    #[test]
    fn check_clean_exit_0() {
        let tmp = tempfile::tempdir().unwrap();
        let d = docs(
            tmp.path(),
            &[
                ("a.md", &block("structure --level file")),
                ("b.md", "no blocks here\n"),
            ],
        );
        let out = cli(tmp.path(), &["view", "check", d.to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        let one = cli(
            tmp.path(),
            &["view", "check", d.join("a.md").to_str().unwrap()],
        );
        assert_eq!(one.status.code(), Some(0), "{one:?}");
    }

    /// R-1. The check runs where there is no project, no index and no kindex.
    #[test]
    fn check_needs_no_index() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tempfile::tempdir().unwrap();
        let d = docs(tmp.path(), &[("a.md", &block("callers x"))]);
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(empty.path())
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .env_remove("KANBANR_DATA_DIR")
            .env("HOME", empty.path())
            .args(["view", "check", d.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{out:?}");
    }

    /// R-2. Each distinct block gets a Markdown and a JSON file named by its view ID.
    #[test]
    fn snapshot_writes_each_view() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let d = docs(
            tmp.path(),
            &[(
                "a.md",
                &format!(
                    "{}\n{}",
                    block("structure --level file"),
                    block("contracts")
                ),
            )],
        );
        let out = tmp.path().join("snap");
        let r = snapshot(&repo, &d, &out, &[]);
        assert_eq!(r.status.code(), Some(0), "{r:?}");
        let idx = index_of(&out);
        let blocks = idx["blocks"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        for b in blocks {
            let id = b["view_id"].as_str().unwrap();
            assert!(out.join(format!("{id}.md")).is_file());
            let body: Value =
                serde_json::from_slice(&std::fs::read(out.join(format!("{id}.json"))).unwrap())
                    .unwrap();
            assert_eq!(body["view_id"], id);
            assert_eq!(body["exact"], true);
        }
        assert_eq!(files_in(&out).len(), 5, "{:?}", files_in(&out));
    }

    /// R-2. Two blocks asking one question, spelled differently, share one pair of files.
    #[test]
    fn same_question_one_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let d = docs(
            tmp.path(),
            &[
                ("a.md", &block("structure")),
                (
                    "b.md",
                    &block("structure --level package --max-nodes 60 --inferred false"),
                ),
            ],
        );
        let out = tmp.path().join("snap");
        let r = snapshot(&repo, &d, &out, &[]);
        assert_eq!(r.status.code(), Some(0), "{r:?}");
        let idx = index_of(&out);
        assert_eq!(idx["blocks"][0]["view_id"], idx["blocks"][1]["view_id"]);
        assert_eq!(files_in(&out).len(), 3, "{:?}", files_in(&out));
    }

    /// R-2. The Markdown holds front matter with the view ID and a Mermaid block.
    #[test]
    fn snapshot_is_mermaid_block() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let d = docs(tmp.path(), &[("a.md", &block("structure --level file"))]);
        let out = tmp.path().join("snap");
        assert_eq!(snapshot(&repo, &d, &out, &[]).status.code(), Some(0));
        let id = index_of(&out)["blocks"][0]["view_id"]
            .as_str()
            .unwrap()
            .to_string();
        let md = std::fs::read_to_string(out.join(format!("{id}.md"))).unwrap();
        assert!(
            md.starts_with(&format!("---\nview_id: {id}\nquery: \"structure")),
            "{md}"
        );
        assert!(md.contains("\n---\n\n```mermaid\nflowchart"), "{md}");
        assert!(md.contains("partial: false"), "{md}");
    }

    /// R-2. `--at` targets a named position, not the current checkout.
    #[test]
    fn snapshot_at_named_position() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let c1 = head(&repo);
        ok(&repo, &["context", "add", "--commit", &c1]);
        std::fs::write(repo.join("extra.tx"), "sym fn extra\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c2"]);
        ok(&repo, &["index"]);
        let d = docs(tmp.path(), &[("a.md", &block("structure --level file"))]);
        let (now, then) = (tmp.path().join("now"), tmp.path().join("then"));
        assert_eq!(snapshot(&repo, &d, &now, &[]).status.code(), Some(0));
        let r = snapshot(&repo, &d, &then, &["--at", &c1]);
        assert_eq!(r.status.code(), Some(0), "{r:?}");
        let (inow, ithen) = (index_of(&now), index_of(&then));
        assert_eq!(ithen["positions"][0]["commit"], c1.as_str());
        assert_eq!(inow["positions"][0]["commit"], head(&repo).as_str());
        assert_ne!(inow["blocks"][0]["view_id"], ithen["blocks"][0]["view_id"]);
        let (a, b) = (
            inow["blocks"][0]["view_id"].as_str().unwrap(),
            ithen["blocks"][0]["view_id"].as_str().unwrap(),
        );
        let (ja, jb) = (
            std::fs::read_to_string(now.join(format!("{a}.json"))).unwrap(),
            std::fs::read_to_string(then.join(format!("{b}.json"))).unwrap(),
        );
        assert!(ja.contains("extra.tx") && !jb.contains("extra.tx"));
    }

    /// R-3. `index.json` lists the blocks sorted by document and line, with sorted positions.
    #[test]
    fn index_lists_blocks_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let d = docs(
            tmp.path(),
            &[
                ("z.md", &block("cycles")),
                (
                    "a.md",
                    &format!("{}\n{}", block("contracts"), block("structure")),
                ),
                ("m/b.md", &block("structure --level file")),
            ],
        );
        let out = tmp.path().join("snap");
        assert_eq!(snapshot(&repo, &d, &out, &[]).status.code(), Some(0));
        let idx = index_of(&out);
        let order: Vec<(String, u64)> = idx["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| {
                (
                    b["doc"].as_str().unwrap().to_string(),
                    b["line"].as_u64().unwrap(),
                )
            })
            .collect();
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted);
        assert_eq!(order.len(), 4);
        assert_eq!(order[0], ("a.md".to_string(), 5));
        let repos: Vec<&str> = idx["positions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["repo"].as_str().unwrap())
            .collect();
        let mut s = repos.clone();
        s.sort();
        assert_eq!(repos, s);
    }

    /// R-3. A second snapshot at the same position repeats every file byte for byte.
    #[test]
    fn snapshot_bytes_repeat() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let d = docs(
            tmp.path(),
            &[(
                "a.md",
                &format!("{}\n{}", block("contracts"), block("callers target")),
            )],
        );
        let (one, two) = (tmp.path().join("one"), tmp.path().join("two"));
        assert_eq!(snapshot(&repo, &d, &one, &[]).status.code(), Some(0));
        assert_eq!(snapshot(&repo, &d, &two, &[]).status.code(), Some(0));
        let names = files_in(&one);
        assert_eq!(names, files_in(&two));
        for n in names {
            assert_eq!(
                std::fs::read(one.join(&n)).unwrap(),
                std::fs::read(two.join(&n)).unwrap(),
                "{n}"
            );
        }
    }

    /// R-4. A view that is not exact ends the run, names the block and writes nothing.
    #[test]
    fn not_exact_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        // A question not asked before, read without waiting for the index: not exact.
        let d = docs(
            tmp.path(),
            &[
                ("a.md", &block("structure")),
                ("b.md", &block("structure --max-nodes 59")),
            ],
        );
        let out = tmp.path().join("snap");
        let r = snapshot(&repo, &d, &out, &["--consistency", "stale-ok"]);
        assert_eq!(r.status.code(), Some(5), "{r:?}");
        let err = String::from_utf8_lossy(&r.stderr);
        assert!(err.contains("b.md:5"), "{err}");
        assert!(!out.exists());
        let leftovers: Vec<String> = files_in(tmp.path())
            .into_iter()
            .filter(|n| n.contains("snapshot-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// R-4. A position no context holds is exit 6, with nothing written.
    #[test]
    fn position_unavailable_exit_6() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let d = docs(tmp.path(), &[("a.md", &block("structure"))]);
        let out = tmp.path().join("snap");
        let none = "0".repeat(40);
        let r = snapshot(&repo, &d, &out, &["--at", &none]);
        assert_eq!(r.status.code(), Some(6), "{r:?}");
        assert!(
            String::from_utf8_lossy(&r.stderr).contains("a.md:5"),
            "{r:?}"
        );
        assert!(!out.exists());
    }

    /// R-5. A view cut by its cap is marked partial in `index.json` and in its Markdown.
    #[test]
    fn partial_marked_in_index() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let d = docs(
            tmp.path(),
            &[(
                "a.md",
                &format!(
                    "{}\n{}",
                    block("structure --level file --max-nodes 2"),
                    block("cycles")
                ),
            )],
        );
        let out = tmp.path().join("snap");
        let r = snapshot(&repo, &d, &out, &[]);
        assert_eq!(r.status.code(), Some(0), "{r:?}");
        let idx = index_of(&out);
        let by_source = |s: &str| {
            idx["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|b| b["source"] == s)
                .unwrap()
                .clone()
        };
        let cut = by_source("structure --level file --max-nodes 2");
        assert_eq!(cut["partial"], true, "{idx}");
        let md =
            std::fs::read_to_string(out.join(format!("{}.md", cut["view_id"].as_str().unwrap())))
                .unwrap();
        assert!(
            md.contains("partial: true") && md.contains("partial"),
            "{md}"
        );
        assert_eq!(by_source("cycles")["partial"], false);
    }

    /// R-6. The documents are byte-identical afterwards, and only `--out` has new files.
    #[test]
    fn docs_never_modified() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let d = docs(
            tmp.path(),
            &[
                ("a.md", &block("structure")),
                ("sub/b.md", &block("cycles")),
            ],
        );
        let before: Vec<(String, Vec<u8>)> = ["a.md", "sub/b.md"]
            .iter()
            .map(|n| (n.to_string(), std::fs::read(d.join(n)).unwrap()))
            .collect();
        let top_before = files_in(tmp.path());
        let out = tmp.path().join("snap");
        assert_eq!(snapshot(&repo, &d, &out, &[]).status.code(), Some(0));
        for (n, bytes) in before {
            assert_eq!(std::fs::read(d.join(&n)).unwrap(), bytes, "{n}");
        }
        assert_eq!(files_in(&d), vec!["a.md", "sub"]);
        let mut expect = top_before;
        expect.push("snap".to_string());
        expect.sort();
        assert_eq!(files_in(tmp.path()), expect);
    }

    /// R-6. The snapshot runs with no other tool on the path and no board.
    #[test]
    fn runs_without_kanbanr() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let d = docs(tmp.path(), &[("a.md", &block("structure"))]);
        let out = tmp.path().join("snap");
        let empty = tempfile::tempdir().unwrap();
        let r = std::process::Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(&repo)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .env_remove("KANBANR_DATA_DIR")
            .env("PATH", empty.path())
            .env("HOME", empty.path())
            .args([
                "view",
                "snapshot",
                "--docs",
                d.to_str().unwrap(),
                "--out",
                out.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert_eq!(r.status.code(), Some(0), "{r:?}");
        assert!(out.join("index.json").is_file());
    }

    /// R-8. No file of the snapshot holds the path of the repo, the docs or the temp folder.
    #[test]
    fn snapshot_has_no_absolute_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = project(tmp.path());
        let d = docs(
            tmp.path(),
            &[(
                "sub/a.md",
                &format!(
                    "{}\n{}",
                    block("structure --level file"),
                    block("contracts")
                ),
            )],
        );
        let out = tmp.path().join("snap");
        assert_eq!(snapshot(&repo, &d, &out, &[]).status.code(), Some(0));
        let root = tmp.path().to_str().unwrap().to_string();
        let mut seen = 0;
        for name in files_in(&out) {
            let text = std::fs::read_to_string(out.join(&name)).unwrap();
            seen += 1;
            for bad in [
                root.as_str(),
                repo.to_str().unwrap(),
                kx.to_str().unwrap(),
                "/home/",
                "/tmp/",
            ] {
                assert!(!text.contains(bad), "{name} holds `{bad}`");
            }
        }
        assert!(seen >= 3);
        assert_eq!(index_of(&out)["blocks"][0]["doc"], "sub/a.md");
    }

    /// R-9. `index.json` lists every repo the snapshot names, in sorted order, including a repo
    /// that is named in a view's body.
    #[test]
    fn index_lists_every_named_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let kx = tmp.path().join("kx");
        let (app, lib) = (tmp.path().join("app"), tmp.path().join("lib"));
        for (dir, files) in [
            (
                &app,
                &[(
                    "client.tx",
                    "sym fn fetch\nbind fetch consumes endpoint:orders\n",
                )][..],
            ),
            (
                &lib,
                &[(
                    "api.tx",
                    "sym fn serve\nbind serve provides endpoint:orders\n",
                )][..],
            ),
        ] {
            std::fs::create_dir_all(dir).unwrap();
            for (n, t) in files {
                std::fs::write(dir.join(n), t).unwrap();
            }
            git(dir, &["init", "-q", "-b", "main"]);
            git(dir, &["config", "user.name", "Test"]);
            git(dir, &["config", "user.email", "test@example.com"]);
            git(dir, &["config", "commit.gpgsign", "false"]);
            git(dir, &["add", "-A"]);
            git(dir, &["commit", "-q", "-m", "c1"]);
        }
        ok(tmp.path(), &["kindex", "init", kx.to_str().unwrap()]);
        ok(
            tmp.path(),
            &[
                "--kindex",
                kx.to_str().unwrap(),
                "project",
                "init",
                "demo",
                "--repo",
                &format!("app={}", app.display()),
                "--repo",
                &format!("lib={}", lib.display()),
                "--mark",
            ],
        );
        ok(&app, &["index"]);
        let d = docs(tmp.path(), &[("a.md", &block("structure --level file"))]);
        let out = tmp.path().join("snap");
        let r = snapshot(&app, &d, &out, &[]);
        assert_eq!(r.status.code(), Some(0), "{r:?}");
        let idx = index_of(&out);
        let repos: Vec<&str> = idx["repos"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap())
            .collect();
        assert_eq!(repos, vec!["app", "lib"], "{idx}");
        let id = idx["blocks"][0]["view_id"].as_str().unwrap();
        let body = std::fs::read_to_string(out.join(format!("{id}.json"))).unwrap();
        assert!(body.contains("\"group\":\"lib\""), "{body}");
    }

    /// R-7. The shared file's valid blocks parse, its invalid blocks do not, and it names no
    /// other tool.
    #[test]
    fn shared_block_file_parses() {
        use katalogr_dist::render::{ViewSpec, find_blocks};
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/contract/query-block-v1.md");
        let text = std::fs::read_to_string(path).unwrap();
        assert!(!text.to_lowercase().contains("kanbanr"));
        let (mut valid, mut invalid) = (Vec::new(), 0);
        for b in find_blocks(&text) {
            let heading = text[..b.start]
                .lines()
                .rev()
                .find(|l| l.starts_with("### "))
                .expect("a heading above every block");
            let parsed = ViewSpec::parse(&b.source);
            match heading {
                "### valid" => {
                    let spec = parsed.unwrap_or_else(|e| panic!("`{}`: {e}", b.source));
                    valid.push(spec.op);
                }
                "### invalid" => {
                    assert!(parsed.is_err(), "`{}` should not parse", b.source);
                    invalid += 1;
                }
                other => panic!("heading `{other}`"),
            }
        }
        valid.sort();
        valid.dedup();
        assert_eq!(
            valid,
            vec![
                "callers",
                "contracts",
                "cycles",
                "impact",
                "path",
                "structure"
            ]
        );
        assert!(invalid >= 10, "{invalid}");
    }
}
