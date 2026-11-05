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

    /// Runs katalogr with `stdin` fed in; returns the exit code and the raw stdout.
    fn run_stdin(dir: &Path, args: &[&str], stdin: &[u8]) -> (i32, Vec<u8>, String) {
        use std::io::Write;
        use std::process::Stdio;
        let mut child = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn katalogr");
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.code().expect("exit code"),
            out.stdout,
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    const BLOCK: &str = "```katalogr-query\nstructure --level file\n```";

    /// The mdBook input for a book rooted at `root`: one chapter at the top and one in a
    /// directory, each with `content`.
    fn book(root: &Path, content: &str) -> String {
        let chapter = |name: &str, path: &str, sub: Value| {
            serde_json::json!({"Chapter": {
                "name": name, "content": content, "number": null,
                "sub_items": sub, "path": path, "source_path": path, "parent_names": []
            }})
        };
        let deep = chapter("Deep", "part/deep.md", serde_json::json!([]));
        let top = chapter("Top", "top.md", serde_json::json!([deep]));
        serde_json::json!([
            {"root": root, "config": {"book": {"src": "src"}}, "renderer": "html",
             "mdbook_version": "0.4.40"},
            {"sections": [top, "Separator"], "__non_exhaustive": null}
        ])
        .to_string()
    }

    /// R-7: each chapter's block becomes a Mermaid block, the chapter gets the Pele tags with
    /// a link relative to its depth, and the assets are written into the book's source.
    #[test]
    fn mdbook_replaces_blocks() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        let root = tmp.path().join("book");
        std::fs::create_dir_all(root.join("src")).unwrap();
        let content = format!("# Title\n\n{BLOCK}\n\nafter\n");
        let (code, out, err) = run_stdin(
            &repo,
            &["render", "mdbook"],
            book(&root, &content).as_bytes(),
        );
        assert_eq!(code, 0, "{err}");
        let v: Value = serde_json::from_slice(&out).expect("a book");
        let top = &v["sections"][0]["Chapter"];
        let text = top["content"].as_str().unwrap();
        assert!(!text.contains("katalogr-query"), "{text}");
        assert!(text.contains("```mermaid\nflowchart LR\n"), "{text}");
        assert!(
            text.contains("href=\"katalogr-assets/pele-theme.css\""),
            "{text}"
        );
        assert!(text.contains("# Title") && text.contains("after"), "{text}");
        let deep = v["sections"][0]["Chapter"]["sub_items"][0]["Chapter"]["content"]
            .as_str()
            .unwrap();
        assert!(
            deep.contains("src=\"../katalogr-assets/pele.global.js\""),
            "{deep}"
        );
        assert_eq!(v["sections"][1], "Separator");
        for f in ["pele.global.js", "pele-init.js", "pele-theme.css"] {
            assert!(root.join("src/katalogr-assets").join(f).is_file(), "{f}");
        }
        // A book without a block is returned as it came, with no assets.
        let tmp2 = tempfile::tempdir().unwrap();
        let plain = book(tmp2.path(), "no blocks here\n");
        let (code, out, _) = run_stdin(tmp2.path(), &["render", "mdbook"], plain.as_bytes());
        assert_eq!(code, 0);
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["sections"][0]["Chapter"]["content"], "no blocks here\n");
        assert!(!tmp2.path().join("src/katalogr-assets").exists());
    }

    /// R-7: only the HTML renderer is supported, so a PDF book keeps its source blocks.
    #[test]
    fn mdbook_supports_html_only() {
        let tmp = tempfile::tempdir().unwrap();
        let (code, ..) = run_stdin(tmp.path(), &["render", "mdbook", "supports", "html"], b"");
        assert_eq!(code, 0);
        for other in ["pdf", "markdown", "epub"] {
            let (code, ..) = run_stdin(tmp.path(), &["render", "mdbook", "supports", other], b"");
            assert_eq!(code, 1, "{other}");
        }
    }

    /// R-7: the assets are written when they are missing or differ and left alone otherwise.
    #[test]
    fn mdbook_writes_assets_once() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        let root = tmp.path().join("book");
        std::fs::create_dir_all(root.join("src")).unwrap();
        let input = book(&root, &format!("{BLOCK}\n"));
        let assets = root.join("src/katalogr-assets");
        let stamp = || {
            ["pele.global.js", "pele-init.js", "pele-theme.css"].map(|f| {
                std::fs::metadata(assets.join(f))
                    .unwrap()
                    .modified()
                    .unwrap()
            })
        };
        run_stdin(&repo, &["render", "mdbook"], input.as_bytes());
        let first = stamp();
        std::thread::sleep(std::time::Duration::from_millis(50));
        run_stdin(&repo, &["render", "mdbook"], input.as_bytes());
        assert_eq!(first, stamp(), "unchanged files are not rewritten");
        // A changed file is put right; no temporary file is left behind.
        std::fs::write(assets.join("pele-init.js"), "// edited").unwrap();
        run_stdin(&repo, &["render", "mdbook"], input.as_bytes());
        assert_ne!(
            std::fs::read(assets.join("pele-init.js")).unwrap(),
            b"// edited"
        );
        let names: Vec<String> = std::fs::read_dir(&assets)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 3, "{names:?}");
    }

    /// R-8: Markdown in, Markdown out, with the block drawn and the rest untouched.
    #[test]
    fn filter_stdin_stdout() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        let md = format!("# Doc\n\nBefore.\n\n{BLOCK}\n\n```rust\nfn x() {{}}\n```\n\nAfter.\n");
        let (code, out, err) = run_stdin(&repo, &["render", "filter"], md.as_bytes());
        assert_eq!(code, 0, "{err}");
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.starts_with("# Doc\n\nBefore.\n\n"),
            "{}",
            &out[..60.min(out.len())]
        );
        assert!(out.contains("```mermaid\nflowchart LR\n"));
        assert!(out.contains("```rust\nfn x() {}\n```\n\nAfter.\n"));
        assert!(!out.contains("katalogr-query"));
    }

    /// R-8: a document with no block comes back byte for byte, with no assets and no project
    /// needed.
    #[test]
    fn filter_no_blocks_bytes_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let md: &[u8] = b"# T\r\n\r\n```text\r\nkatalogr-query\r\n```\r\n\xff\xfe no newline";
        for args in [
            &["render", "filter"][..],
            &["render", "filter", "--assets", "out"][..],
        ] {
            let (code, out, _) = run_stdin(tmp.path(), args, md);
            assert_eq!(code, 0);
            assert_eq!(out, md);
        }
        assert!(!tmp.path().join("out").exists());
    }

    /// R-8: assets inline by default, or written to a directory and linked.
    #[test]
    fn filter_assets_inline_or_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        let md = format!("{BLOCK}\n\n{BLOCK}\n");
        let (code, out, _) = run_stdin(&repo, &["render", "filter"], md.as_bytes());
        assert_eq!(code, 0);
        let inline = String::from_utf8(out).unwrap();
        assert_eq!(inline.matches("<style>").count(), 1, "inlined once");
        assert!(inline.contains("<script>") && inline.contains("var pele="));
        assert_eq!(inline.matches("```mermaid").count(), 2);
        assert!(!repo.join("katalogr-assets").exists());

        let (code, out, _) = run_stdin(
            &repo,
            &["render", "filter", "--assets", "docs/assets"],
            md.as_bytes(),
        );
        assert_eq!(code, 0);
        let linked = String::from_utf8(out).unwrap();
        assert_eq!(
            linked
                .matches("href=\"docs/assets/pele-theme.css\"")
                .count(),
            1
        );
        assert!(!linked.contains("var pele="), "nothing inlined");
        for f in ["pele.global.js", "pele-init.js", "pele-theme.css"] {
            assert!(repo.join("docs/assets").join(f).is_file(), "{f}");
        }
    }

    /// R-11: a block that fails shows its source and the error; the run still succeeds.
    #[test]
    fn failed_block_shows_source_and_error() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        let md = "```katalogr-query\ncallers nothing_like_this\n```\n\n```katalogr-query\nbogus <op>\n```\n\n".to_string() + BLOCK + "\n";
        let (code, out, err) = run_stdin(&repo, &["render", "filter"], md.as_bytes());
        assert_eq!(code, 0, "{err}");
        let out = String::from_utf8(out).unwrap();
        assert_eq!(out.matches("class=\"katalogr-error\"").count(), 2, "{out}");
        assert!(out.contains("callers nothing_like_this"), "source is shown");
        assert!(out.contains("no symbol matches"), "error is shown");
        assert!(out.contains("bogus &lt;op&gt;"), "source is escaped: {out}");
        assert!(out.contains("```mermaid"), "the good block is still drawn");
        assert!(err.contains("not drawn"), "{err}");
    }

    /// R-11: with `--strict` the first failure's code is the exit code, after the output.
    #[test]
    fn filter_strict_exits_nonzero() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        let missing = "```katalogr-query\ncallers nothing_like_this\n```\n";
        let (code, out, _) =
            run_stdin(&repo, &["render", "filter", "--strict"], missing.as_bytes());
        assert_eq!(code, 4);
        assert!(String::from_utf8(out).unwrap().contains("katalogr-error"));
        let bad = "```katalogr-query\nbogus\n```\n";
        let (code, ..) = run_stdin(&repo, &["render", "filter", "--strict"], bad.as_bytes());
        assert_eq!(code, 2);
        let good = format!("{BLOCK}\n");
        let (code, ..) = run_stdin(&repo, &["render", "filter", "--strict"], good.as_bytes());
        assert_eq!(code, 0);
    }

    /// FEAT-088 R-2: a second render of a view is drawn from the stored view, in any format; it
    /// is not a new computation.
    #[test]
    fn second_render_from_store() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        let views = tmp.path().join("kx/projects/demo/.cache/views");
        assert!(!views.exists());
        let first = ok(&repo, &["render", "structure", "--level", "file"]);
        // The first render stored its view.
        let mut stored = Vec::new();
        for bucket in std::fs::read_dir(&views).unwrap().flatten() {
            for f in std::fs::read_dir(bucket.path()).unwrap().flatten() {
                stored.push(f.path());
            }
        }
        assert_eq!(stored.len(), 1, "{stored:?}");
        let again = ok(&repo, &["render", "structure", "--level", "file"]);
        assert_eq!(again, first);
        // Change the stored graph: the next render, in another format, shows the change.
        let mut v: Value = serde_json::from_slice(&std::fs::read(&stored[0]).unwrap()).unwrap();
        v["graph"]["nodes"][0]["label"] = "FROMSTORE".into();
        std::fs::write(&stored[0], serde_json::to_vec(&v).unwrap()).unwrap();
        for format in ["mermaid", "dot", "graphml"] {
            let out = ok(
                &repo,
                &["render", "structure", "--level", "file", "--format", format],
            );
            assert!(out.contains("FROMSTORE"), "{format}: {out}");
        }
    }
}
