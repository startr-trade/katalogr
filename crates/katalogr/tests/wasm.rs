//! The WASM bridge through the CLI, without an agent (FEAT-016 R-6, R-8): `callers` and
//! `refs` on a Rust export list its TypeScript call sites through the contract, also when
//! the crate and its consumers are in different repositories.

mod wasm {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/wasm")
    }

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap().flatten() {
            let (src, dst) = (e.path(), to.join(e.file_name()));
            if src.is_dir() {
                copy_dir(&src, &dst);
            } else if e.file_name() != "expected.yaml" {
                std::fs::copy(&src, &dst).unwrap();
            }
        }
    }

    fn katalogr(dir: &Path, args: &[&str]) -> (i32, String) {
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
        )
    }

    fn ok(dir: &Path, args: &[&str]) -> String {
        let (code, out) = katalogr(dir, args);
        assert_eq!(code, 0, "{args:?}: {out}");
        out
    }

    /// A project `demo` with the given marked repos, each indexed.
    fn project(tmp: &Path, repos: &[(&str, &Path)]) {
        let kx = tmp.join("kx");
        ok(tmp, &["kindex", "init", kx.to_str().unwrap()]);
        let mut args: Vec<String> = vec![
            "--kindex".into(),
            kx.to_str().unwrap().into(),
            "project".into(),
            "init".into(),
            "demo".into(),
            "--mark".into(),
        ];
        for (n, p) in repos {
            args.push("--repo".into());
            args.push(format!("{n}={}", p.display()));
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        ok(tmp, &refs);
        for (_, p) in repos {
            ok(p, &["index"]);
        }
    }

    const CONTRACT: &str = "contract:wasm_export:demo_wasm%2Fgreet";

    #[test]
    fn cli_callers_cross_language() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("app");
        copy_dir(&fixtures().join("exports-uncommitted"), &repo);
        project(tmp.path(), &[("app", &repo)]);

        let callers = ok(&repo, &["callers", "greet"]);
        let line = callers
            .lines()
            .find(|l| l.contains("web/src/app.ts"))
            .unwrap_or_else(|| panic!("{callers}"));
        assert!(line.contains(&format!("via {CONTRACT}")), "{line}");
        assert!(line.contains("#fn:main"), "{line}");

        let refs = ok(&repo, &["refs", CONTRACT]);
        assert!(
            refs.contains("provides") && refs.contains("src/lib.rs#fn:greet"),
            "{refs}"
        );
        assert!(
            refs.contains("consumes") && refs.contains("web/src/app.ts"),
            "{refs}"
        );
    }

    #[test]
    fn cli_matched_across_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let core = tmp.path().join("core");
        let web = tmp.path().join("web");
        std::fs::create_dir_all(core.join("src")).unwrap();
        std::fs::create_dir_all(&web).unwrap();
        let fx = fixtures().join("exports-uncommitted");
        std::fs::copy(fx.join("Cargo.toml"), core.join("Cargo.toml")).unwrap();
        std::fs::copy(fx.join("src/lib.rs"), core.join("src/lib.rs")).unwrap();
        std::fs::write(
            web.join("package.json"),
            r#"{ "name": "demo-web", "dependencies": { "demo-wasm": "file:../core/pkg" } }"#,
        )
        .unwrap();
        std::fs::copy(fx.join("web/src/app.ts"), web.join("app.ts")).unwrap();
        project(tmp.path(), &[("core", &core), ("web", &web)]);

        let refs = ok(&core, &["refs", CONTRACT]);
        assert!(
            refs.contains("core:src/lib.rs#fn:greet") && refs.contains("provides"),
            "{refs}"
        );
        assert!(
            refs.contains("web:app.ts") && refs.contains("consumes"),
            "{refs}"
        );
        let callers = ok(&core, &["callers", "greet"]);
        assert!(
            callers.contains("web:app.ts#fn:main") && callers.contains(CONTRACT),
            "{callers}"
        );
    }

    #[test]
    fn explain_wasm_export_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("app");
        copy_dir(&fixtures().join("exports-uncommitted"), &repo);
        project(tmp.path(), &[("app", &repo)]);

        let out = ok(&repo, &["explain", CONTRACT]);
        assert!(out.contains("kind: wasm_export"), "{out}");
        let at = |head: &str| out.lines().position(|l| l.starts_with(head)).expect(&out);
        let provider = out.lines().nth(at("providers") + 1).expect(&out);
        assert!(provider.contains("src/lib.rs#fn:greet"), "{out}");
        let consumer = out.lines().nth(at("consumers") + 1).expect(&out);
        assert!(consumer.contains("web/src/app.ts"), "{out}");
    }
}
