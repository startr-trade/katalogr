//! Hook tests (FEAT-026): Claude Code hooks, git events and install. Hooks run as the built
//! binary with hook JSON on stdin; settings go to temp dirs and a temp HOME.

mod hooks {
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};

    const BIN: &str = env!("CARGO_BIN_EXE_katalogr");

    fn fixture(name: &str) -> String {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/hooks")
            .join(name);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    fn cmd(dir: &Path, home: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        let path = format!(
            "{}:{}",
            Path::new(BIN).parent().unwrap().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        c.current_dir(dir)
            .env("HOME", home)
            .env("PATH", path)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(args);
        c
    }

    fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
        cmd(dir, home, args).output().expect("run katalogr")
    }

    fn ok(dir: &Path, home: &Path, args: &[&str]) -> String {
        let out = run(dir, home, args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Run a hook with `input` on stdin; asserts exit 0 and returns stdout.
    fn hook(dir: &Path, home: &Path, which: &str, input: &str) -> String {
        let mut child = cmd(dir, home, &["hook", which])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "hook {which} exited {:?}", out.status);
        String::from_utf8(out.stdout).unwrap()
    }

    fn fill(template: &str, pairs: &[(&str, &str)]) -> String {
        let mut s = template.to_string();
        for (k, v) in pairs {
            s = s.replace(&format!("{{{{{k}}}}}"), &v.replace('\\', "\\\\"));
        }
        s
    }

    struct Env {
        _tmp: tempfile::TempDir,
        home: PathBuf,
        kx: PathBuf,
        repo: PathBuf,
    }

    /// A project `demo` with repo `app` holding a small and a large `.tx` file, indexed.
    fn env() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let (kx, repo, home) = (base.join("kx"), base.join("app"), base.join("home"));
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        ok(&base, &home, &["kindex", "init", kx.to_str().unwrap()]);
        let spec = format!("app={}", repo.display());
        ok(
            &base,
            &home,
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
        let mut big = String::new();
        for i in 0..1800 {
            big.push_str(&format!("sym fn big_function_number_{i:04}\n"));
        }
        big.push_str("sym fn unique_target\nsym fn twin\n");
        std::fs::write(repo.join("big.tx"), &big).unwrap();
        std::fs::write(repo.join("small.tx"), "sym fn small_one\nsym fn twin\n").unwrap();
        ok(&repo, &home, &["index"]);
        Env {
            _tmp: tmp,
            home,
            kx,
            repo,
        }
    }

    fn json(s: &str) -> serde_json::Value {
        serde_json::from_str(s).unwrap_or_else(|e| panic!("not JSON ({e}): {s}"))
    }

    fn read_input(e: &Env, fixture_name: &str, file: &str) -> String {
        fill(
            &fixture(fixture_name),
            &[("CWD", e.repo.to_str().unwrap()), ("FILE", file)],
        )
    }

    #[test]
    fn large_read_blocked() {
        let e = env();
        let big = e.repo.join("big.tx");
        let out = hook(
            &e.repo,
            &e.home,
            "pre-read",
            &read_input(&e, "pre_tool_use_read_input.json", big.to_str().unwrap()),
        );
        let v = json(&out);
        // The shape matches the confirmed protocol fixture.
        let shape = json(&fixture("pre_tool_use_deny_output.json"));
        assert_eq!(
            v["hookSpecificOutput"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            shape["hookSpecificOutput"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>()
        );
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        let reason = v["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(reason.contains("katalogr show app:big.tx#fn:"), "{reason}");
        assert!(
            reason.contains("katalogr explain app:big.tx#fn:"),
            "{reason}"
        );
        assert!(reason.contains("offset/limit"), "{reason}");
        let listed = reason.lines().filter(|l| l.starts_with("- ")).count();
        assert_eq!(listed, 10, "{reason}");
        assert!(reason.contains("lines "), "{reason}");

        // offset/limit, small and unindexed files allow.
        let ranged = read_input(
            &e,
            "pre_tool_use_read_range_input.json",
            big.to_str().unwrap(),
        );
        assert_eq!(hook(&e.repo, &e.home, "pre-read", &ranged), "");
        let small = e.repo.join("small.tx");
        let small_in = read_input(&e, "pre_tool_use_read_input.json", small.to_str().unwrap());
        assert_eq!(hook(&e.repo, &e.home, "pre-read", &small_in), "");
        let other = e.repo.join("unindexed.dat");
        std::fs::write(&other, vec![b'x'; 40_000]).unwrap();
        let other_in = read_input(&e, "pre_tool_use_read_input.json", other.to_str().unwrap());
        assert_eq!(hook(&e.repo, &e.home, "pre-read", &other_in), "");

        // The decision is logged (path and decision only).
        let log = e.kx.join("projects/demo/.cache/hooks.log");
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("pre-read deny big.tx"), "{text}");
    }

    #[test]
    fn grep_enriched() {
        let e = env();
        let input = fill(
            &fixture("post_tool_use_grep_input.json"),
            &[
                ("CWD", e.repo.to_str().unwrap()),
                ("PATTERN", r"\bunique_target\b|twin"),
            ],
        );
        let v = json(&hook(&e.repo, &e.home, "post-search", &input));
        let shape = json(&fixture("post_tool_use_context_output.json"));
        assert_eq!(
            v["hookSpecificOutput"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            shape["hookSpecificOutput"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>()
        );
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        let ctx = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(ctx.contains("app:big.tx#fn:unique_target"), "{ctx}");
        assert!(
            ctx.contains("katalogr explain app:big.tx#fn:unique_target"),
            "{ctx}"
        );
        // `twin` is defined twice: a count, no detail.
        assert!(ctx.contains("twin: 2 symbols"), "{ctx}");
        assert!(!ctx.contains("small.tx#fn:twin"), "{ctx}");

        // Glob patterns without a known name print nothing.
        let glob = fill(
            &fixture("post_tool_use_glob_input.json"),
            &[("CWD", e.repo.to_str().unwrap()), ("PATTERN", "**/*.tx")],
        );
        assert_eq!(hook(&e.repo, &e.home, "post-search", &glob), "");
        let none = fill(
            &fixture("post_tool_use_grep_input.json"),
            &[
                ("CWD", e.repo.to_str().unwrap()),
                ("PATTERN", "no_such_name"),
            ],
        );
        assert_eq!(hook(&e.repo, &e.home, "post-search", &none), "");
    }

    /// FEAT-076 R-2: the hook finds a name through the case-insensitive index and keeps exactly
    /// the symbols a scan of every symbol kept: those whose plain name equals the identifier.
    #[test]
    fn search_brief_matches_full_scan() {
        let e = env();
        std::fs::write(
            e.repo.join("case.tx"),
            "sym fn Widget\nsym fn widget\nsym fn WIDGET_LONG\nsym fn Gadget\n",
        )
        .unwrap();
        std::fs::write(
            e.repo.join("case2.tx"),
            "sym fn Gadget\nsym fn widget_long\n",
        )
        .unwrap();
        ok(&e.repo, &e.home, &["index", "--full"]);
        let ask = |pattern: &str| {
            let input = fill(
                &fixture("post_tool_use_grep_input.json"),
                &[("CWD", e.repo.to_str().unwrap()), ("PATTERN", pattern)],
            );
            let out = hook(&e.repo, &e.home, "post-search", &input);
            if out.is_empty() {
                return String::new();
            }
            json(&out)["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .to_string()
        };
        // Exact case only: `Widget` is one symbol although `widget` shares its lowercase name.
        let ctx = ask("Widget");
        assert!(ctx.contains("app:case.tx#fn:Widget"), "{ctx}");
        assert!(!ctx.contains("#fn:widget"), "{ctx}");
        assert!(!ctx.contains("symbols have this name"), "{ctx}");
        // Two symbols of one name: a count, no detail.
        let ctx = ask("Gadget");
        assert!(ctx.contains("Gadget: 2 symbols"), "{ctx}");
        // Upper and lower case names are distinct names.
        let ctx = ask("WIDGET_LONG");
        assert!(ctx.contains("app:case.tx#fn:WIDGET_LONG"), "{ctx}");
        assert!(!ctx.contains("widget_long"), "{ctx}");
        // A name that exists only in another case prints nothing.
        assert_eq!(ask("GADGET"), "");
    }

    #[test]
    fn fail_open_when_stale() {
        let e = env();
        let big = e.repo.join("big.tx");
        let input = read_input(&e, "pre_tool_use_read_input.json", big.to_str().unwrap());
        assert!(hook(&e.repo, &e.home, "pre-read", &input).contains("deny"));

        // Pending change for the file: allow.
        ok(&e.repo, &e.home, &["notify", "big.tx"]);
        assert_eq!(hook(&e.repo, &e.home, "pre-read", &input), "");
        ok(&e.repo, &e.home, &["index"]);
        assert!(hook(&e.repo, &e.home, "pre-read", &input).contains("deny"));

        // The file changed on disk since it was indexed: allow.
        let mut text = std::fs::read_to_string(&big).unwrap();
        text.push_str("sym fn added_later\n");
        std::fs::write(&big, text).unwrap();
        assert_eq!(hook(&e.repo, &e.home, "pre-read", &input), "");
        ok(&e.repo, &e.home, &["index"]);

        // Corrupt cache: allow.
        let cache = e.kx.join("projects/demo/.cache/index.redb");
        std::fs::write(&cache, b"not a database at all, just garbage bytes").unwrap();
        assert_eq!(hook(&e.repo, &e.home, "pre-read", &input), "");
        assert_eq!(
            hook(
                &e.repo,
                &e.home,
                "post-search",
                &fill(
                    &fixture("post_tool_use_grep_input.json"),
                    &[
                        ("CWD", e.repo.to_str().unwrap()),
                        ("PATTERN", "unique_target")
                    ],
                )
            ),
            ""
        );

        // No kindex / no project at all, and unparsable input: allow.
        let lone = tempfile::tempdir().unwrap();
        let lone_in = fill(
            &fixture("pre_tool_use_read_input.json"),
            &[
                ("CWD", lone.path().to_str().unwrap()),
                ("FILE", big.to_str().unwrap()),
            ],
        );
        assert_eq!(hook(lone.path(), &e.home, "pre-read", &lone_in), "");
        assert_eq!(hook(&e.repo, &e.home, "pre-read", "not json"), "");
        assert_eq!(hook(&e.repo, &e.home, "pre-read", ""), "");
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    Path::new(BIN).parent().unwrap().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// `(path, kind)` of every inbox entry, sorted; the inbox is emptied.
    fn drain_inbox(e: &Env) -> Vec<(String, String)> {
        let dir = e.kx.join("projects/demo/.cache/inbox");
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for f in rd.flatten() {
                let v = json(&std::fs::read_to_string(f.path()).unwrap());
                out.push((
                    v["path"].as_str().unwrap_or("<rescan>").to_string(),
                    v["kind"].as_str().unwrap().to_string(),
                ));
                std::fs::remove_file(f.path()).unwrap();
            }
        }
        out.sort();
        out
    }

    fn pair(p: &str, k: &str) -> (String, String) {
        (p.to_string(), k.to_string())
    }

    #[test]
    fn git_events_enqueue() {
        let e = env();
        git(&e.repo, &["init", "-q", "-b", "main"]);
        ok(&e.repo, &e.home, &["hooks", "install"]);
        let _ = drain_inbox(&e);

        // First commit: the whole tree (a rescan of the repo).
        git(&e.repo, &["add", "-A"]);
        git(&e.repo, &["commit", "-q", "-m", "first"]);
        assert_eq!(drain_inbox(&e), [pair("<rescan>", "rescan")]);

        // Later commit: the paths of HEAD~1..HEAD, deletions included.
        std::fs::write(e.repo.join("new.tx"), "sym fn n\n").unwrap();
        std::fs::remove_file(e.repo.join("small.tx")).unwrap();
        git(&e.repo, &["add", "-A"]);
        git(&e.repo, &["commit", "-q", "-m", "second"]);
        assert_eq!(
            drain_inbox(&e),
            [pair("new.tx", "changed"), pair("small.tx", "deleted")]
        );

        // Checkout of another branch: old..new.
        git(&e.repo, &["checkout", "-q", "-b", "side"]);
        assert!(drain_inbox(&e).is_empty());
        std::fs::write(e.repo.join("side.tx"), "sym fn s\n").unwrap();
        git(&e.repo, &["add", "-A"]);
        git(&e.repo, &["commit", "-q", "-m", "side"]);
        assert_eq!(drain_inbox(&e), [pair("side.tx", "changed")]);
        git(&e.repo, &["checkout", "-q", "main"]);
        assert_eq!(drain_inbox(&e), [pair("side.tx", "deleted")]);

        // Merge: ORIG_HEAD..HEAD.
        git(&e.repo, &["merge", "-q", "--no-ff", "-m", "merge", "side"]);
        let queued = drain_inbox(&e);
        assert!(queued.contains(&pair("side.tx", "changed")), "{queued:?}");

        // A file checkout (flag 0) queues nothing; failures never fail the command.
        git(&e.repo, &["checkout", "-q", "--", "big.tx"]);
        assert!(drain_inbox(&e).is_empty());
        let out = run(
            &e.repo,
            &e.home,
            &["notify", "--from-git", "post-checkout", "x"],
        );
        assert!(out.status.success());
        let out = run(&e.repo, &e.home, &["notify", "--from-git", "bogus"]);
        assert!(out.status.success());
        let tmp = tempfile::tempdir().unwrap();
        let out = run(
            tmp.path(),
            &e.home,
            &["notify", "--from-git", "post-commit"],
        );
        assert!(out.status.success());
    }

    #[test]
    fn install_project_scope() {
        let e = env();
        git(&e.repo, &["init", "-q", "-b", "main"]);
        let project = e.repo.join(".claude/settings.json");
        let global = e.home.join(".claude/settings.json");
        let expected = json(&fixture("settings_hooks.json"));

        // A user entry and an existing git hook must survive.
        std::fs::create_dir_all(e.repo.join(".claude")).unwrap();
        std::fs::write(
            &project,
            r#"{"model":"x","hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"mine"}]}]}}"#,
        )
        .unwrap();
        let hooks_dir = e.repo.join(".git/hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(hooks_dir.join("post-commit"), "#!/bin/sh\necho user-hook\n").unwrap();

        ok(&e.repo, &e.home, &["hooks", "install"]);
        let after = json(&std::fs::read_to_string(&project).unwrap());
        assert_eq!(after["model"], "x");
        let pre = after["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2, "{after}");
        assert_eq!(pre[0]["hooks"][0]["command"], "mine");
        assert_eq!(pre[1], expected["hooks"]["PreToolUse"][0]);
        assert_eq!(
            after["hooks"]["PostToolUse"],
            expected["hooks"]["PostToolUse"]
        );
        assert_eq!(
            after["hooks"]["SessionStart"],
            expected["hooks"]["SessionStart"]
        );
        assert!(
            !global.exists(),
            "project scope must not write the global file"
        );

        // Idempotent.
        let first = std::fs::read_to_string(&project).unwrap();
        let hook_first = std::fs::read_to_string(hooks_dir.join("post-commit")).unwrap();
        ok(&e.repo, &e.home, &["hooks", "install"]);
        assert_eq!(std::fs::read_to_string(&project).unwrap(), first);
        assert_eq!(
            std::fs::read_to_string(hooks_dir.join("post-commit")).unwrap(),
            hook_first
        );

        // The existing git hook is kept and runs first; the others are new.
        assert!(
            hook_first.starts_with("#!/bin/sh\necho user-hook\n"),
            "{hook_first}"
        );
        assert!(hook_first.contains("katalogr notify --from-git post-commit"));
        for h in ["post-checkout", "post-merge"] {
            let s = std::fs::read_to_string(hooks_dir.join(h)).unwrap();
            assert!(s.contains("# katalogr hook") && s.contains(h), "{s}");
        }

        let status = ok(&e.repo, &e.home, &["hooks", "status"]);
        assert!(
            status.contains("project PreToolUse katalogr hook pre-read: installed"),
            "{status}"
        );
        assert!(
            status.contains("global PreToolUse katalogr hook pre-read: missing"),
            "{status}"
        );
        assert!(status.contains("git post-commit: installed"), "{status}");

        // --global writes the temp HOME only.
        let before = std::fs::read_to_string(&project).unwrap();
        ok(&e.repo, &e.home, &["hooks", "install", "--global"]);
        assert_eq!(json(&std::fs::read_to_string(&global).unwrap()), expected);
        assert_eq!(std::fs::read_to_string(&project).unwrap(), before);

        // Uninstall removes only katalogr's entries and lines.
        ok(&e.repo, &e.home, &["hooks", "uninstall"]);
        let gone = json(&std::fs::read_to_string(&project).unwrap());
        assert_eq!(gone["model"], "x");
        assert_eq!(gone["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
        assert!(gone["hooks"].get("PostToolUse").is_none());
        assert!(gone["hooks"].get("SessionStart").is_none());
        assert_eq!(
            std::fs::read_to_string(hooks_dir.join("post-commit")).unwrap(),
            "#!/bin/sh\necho user-hook\n"
        );
        assert!(!hooks_dir.join("post-merge").exists());
        assert!(
            global.exists(),
            "project uninstall must leave the global file alone"
        );
        ok(&e.repo, &e.home, &["hooks", "uninstall", "--global"]);
        assert!(
            json(&std::fs::read_to_string(&global).unwrap())
                .get("hooks")
                .is_none()
        );
    }
}
