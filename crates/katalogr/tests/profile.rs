//! Profile provenance through the CLI (FEAT-008 R-5).

mod profile {
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

    /// A project `demo` with one marked repo `app` holding the given `.tx` files; returns the
    /// kindex and the repo.
    fn project(tmp: &Path, files: &[(&str, &str)]) -> (PathBuf, PathBuf) {
        let kx = tmp.join("kx");
        let repo = tmp.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        for (name, text) in files {
            std::fs::write(repo.join(name), text).unwrap();
        }
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

    const PROFILE: &str = "\
profile: example
version: 1
rules:
  - id: bind-serve
    when: { extension: test, kind: fn, name: '^serve$' }
    emit:
      bind: { contract: { kind: channel, key: orders }, role: provides }
  - id: renders-template
    when: { extension: test, kind: fn, name: '^serve$' }
    emit:
      ref: { kind: renders, candidates: [ { ns: file, key: '$repo:tpl.tx' } ] }
";

    #[test]
    fn provenance_shown() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = project(
            tmp.path(),
            &[
                (
                    "api.tx",
                    "sym fn serve\nsym fn direct\nbind direct provides channel:other\n",
                ),
                ("tpl.tx", "sym fn body\n"),
            ],
        );
        let dir = kx.join("projects/demo/profiles");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("example.yaml"), PROFILE).unwrap();
        ok(&repo, &["index"]);

        // Links a rule produced name the profile and the rule; others name nothing.
        let text = ok(&repo, &["explain", "serve"]);
        let provides = text.lines().find(|l| l.contains("provides")).expect(&text);
        assert!(
            provides.contains("origin profile:example#bind-serve"),
            "{text}"
        );
        assert!(provides.contains("[Declared]"), "{text}");
        let renders = text.lines().find(|l| l.contains("renders")).expect(&text);
        assert!(
            renders.contains("origin profile:example#renders-template"),
            "{text}"
        );
        let direct = ok(&repo, &["explain", "direct"]);
        assert!(
            direct.contains("provides") && !direct.contains("origin"),
            "{direct}"
        );

        let json = ok(&repo, &["--json", "explain", "serve"]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let origins: Vec<&str> = v["result"]["outgoing"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|l| l["origin"].as_str())
            .collect();
        assert_eq!(
            origins,
            vec![
                "profile:example#bind-serve",
                "profile:example#renders-template"
            ]
        );

        // The contract the rule bound is on the incoming side of the other end, too.
        let contract = ok(&repo, &["explain", "serve", "--per-side", "1"]);
        assert!(contract.contains("contract:channel:orders"), "{contract}");

        // A refused profile is reported with its file and line; the valid one stays in force.
        std::fs::write(
            dir.join("example.yaml"),
            PROFILE.replace("'^serve$'", "'^(serve$'"),
        )
        .unwrap();
        let (code, out, err) = run(&repo, &["index"]);
        assert_eq!(code, 0, "{out}{err}");
        let all = format!("{out}{err}");
        assert!(
            all.contains("profiles/example.yaml:5") && all.contains("unclosed group"),
            "{all}"
        );
        let still = ok(&repo, &["explain", "serve"]);
        assert!(
            still.contains("origin profile:example#bind-serve"),
            "{still}"
        );
    }

    /// FEAT-014 R-12: a built-in profile is printed as it is embedded, and the listing marks
    /// the project files that replace a built-in.
    #[test]
    fn builtin_shown_by_cli() {
        let tmp = tempfile::tempdir().unwrap();
        let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../katalogr-dist/profiles");
        let want = std::fs::read_to_string(shipped.join("http-ts.yaml")).unwrap();
        // `show` needs no project and prints the file byte for byte.
        assert_eq!(ok(tmp.path(), &["profiles", "show", "http-ts"]), want);
        assert_eq!(ok(tmp.path(), &["profiles", "show", "http-ts.yaml"]), want);
        let json: serde_json::Value =
            serde_json::from_str(&ok(tmp.path(), &["--json", "profiles", "show", "http-ts"]))
                .unwrap();
        assert_eq!(json["text"], want.as_str());
        // An unknown name exits 4 with one line on stderr.
        let (code, out, err) = run(tmp.path(), &["profiles", "show", "nope"]);
        assert_eq!((code, out.as_str()), (4, ""), "{err}");
        assert_eq!(err.lines().count(), 1, "{err}");

        // Outside a project, `list` names the three built-ins.
        let list = ok(tmp.path(), &["profiles", "list"]);
        assert_eq!(
            list.lines().collect::<Vec<_>>(),
            [
                "builtin:generic.yaml",
                "builtin:http-rust.yaml",
                "builtin:http-ts.yaml"
            ]
        );

        // In a project, a copy of a built-in replaces it by name, and a refused file is listed.
        let (kx, repo) = project(tmp.path(), &[("a.tx", "sym fn f\n")]);
        let dir = kx.join("projects/demo/profiles");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ts.yaml"), want.replace("express-route", "mine")).unwrap();
        std::fs::write(
            dir.join("bad.yaml"),
            "profile: bad\nversion: 9\nrules: []\n",
        )
        .unwrap();
        let list = ok(&repo, &["profiles", "list"]);
        let lines: Vec<&str> = list.lines().collect();
        assert_eq!(
            lines[..3],
            [
                "builtin:generic.yaml",
                "builtin:http-rust.yaml",
                "builtin:http-ts.yaml"
            ]
        );
        assert_eq!(lines[3], "profiles/ts.yaml (replaces builtin:http-ts.yaml)");
        assert!(
            lines[4].starts_with("refused: profiles/bad.yaml:") && lines[4].contains("version"),
            "{list}"
        );
        let json: serde_json::Value =
            serde_json::from_str(&ok(&repo, &["--json", "profiles", "list"])).unwrap();
        assert_eq!(json["project"][0]["replaces"], "builtin:http-ts.yaml");
        assert_eq!(json["refused"][0]["file"], "profiles/bad.yaml");
    }
}
