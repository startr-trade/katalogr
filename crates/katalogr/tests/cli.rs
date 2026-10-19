//! CLI-level tests: run the built binary.

mod cli {
    use std::process::Command;

    fn version_output() -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .arg("--version")
            .output()
            .expect("run katalogr --version");
        assert!(
            out.status.success(),
            "--version exited with {:?}",
            out.status
        );
        String::from_utf8(out.stdout).expect("utf8 --version output")
    }

    /// Extracts `<sha>` from `katalogr <semver> (<sha> <date>)`.
    fn build_sha(line: &str) -> &str {
        let inner = line
            .split_once('(')
            .and_then(|(_, rest)| rest.strip_suffix(")\n").or_else(|| rest.strip_suffix(')')))
            .unwrap_or_else(|| panic!("unexpected version format: {line:?}"));
        inner
            .split_once(' ')
            .map(|(sha, _date)| sha)
            .unwrap_or_else(|| panic!("no date in version: {line:?}"))
    }

    use std::path::{Path, PathBuf};

    pub fn katalogr(dir: &Path, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(args)
            .output()
            .expect("run katalogr")
    }

    pub fn ok(dir: &Path, args: &[&str]) -> String {
        let out = katalogr(dir, args);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("utf8 stdout")
    }

    /// A kindex and a project `demo` with one marked repo `app` holding a file.
    pub fn setup(tmp: &Path) -> (PathBuf, PathBuf) {
        let kx = tmp.join("kx");
        let repo = tmp.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
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

    /// A project with the `.tx` fixtures below, indexed. Returns the kindex and repo paths.
    pub fn query_project(tmp: &Path) -> (PathBuf, PathBuf) {
        let (kx, repo) = setup(tmp);
        std::fs::write(
            repo.join("lib.tx"),
            "sym fn target\nsym use alias\nlink alias reexports target\n\
             export ns:alias alias\nexport ns:target target\nsym fn dup\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("user.tx"),
            "sym fn caller_a\nsym fn caller_b\nsym fn top\nsym item story\n\
             ref caller_a calls ns:target\nref top references ns:alias\n\
             link caller_b calls caller_a\nlink top calls caller_b\n\
             link caller_a refs-item story\nsym fn dup\n",
        )
        .unwrap();
        ok(&repo, &["index"]);
        (kx, repo)
    }

    /// Run a query command and return `(exit code, stdout, stderr)`.
    pub fn run(repo: &Path, args: &[&str]) -> (i32, String, String) {
        let out = katalogr(repo, args);
        (
            out.status.code().expect("exit code"),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    }

    #[test]
    fn kindex_and_project_init() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = setup(tmp.path());
        let project = kx.join("projects/demo");
        for sub in ["profiles", "shards", ".cache"] {
            assert!(project.join(sub).is_dir(), "missing {sub}");
        }
        assert!(kx.join(".git").exists());
        assert!(project.join("format.json").is_file());
        let config = std::fs::read_to_string(project.join("config.yaml")).unwrap();
        assert!(config.contains("name: app"), "{config}");
        assert!(
            !config.contains(repo.to_str().unwrap()),
            "root leaked into the kindex: {config}"
        );

        let roots = std::fs::read_to_string(project.join(".cache/roots.yaml")).unwrap();
        let canon = std::fs::canonicalize(&repo).unwrap();
        assert_eq!(roots, format!("app: {}\n", canon.display()));

        let marker = std::fs::read_to_string(repo.join(".katalogr")).unwrap();
        assert_eq!(marker, "kindex: ../kx\nproject: demo\nrepo: app\n");

        // The marker alone resolves everything: add-root works with no flags.
        ok(
            &repo,
            &["project", "add-root", "app", repo.to_str().unwrap()],
        );
    }

    #[test]
    fn notify_then_index() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = setup(tmp.path());
        let out = ok(&repo, &["notify", "a.txt"]);
        assert!(out.contains("queued 1"), "{out}");
        let json = ok(&repo, &["--json", "index"]);
        let v: serde_json::Value = serde_json::from_str(&json).expect("json");
        assert_eq!(v["pending"], 0, "{json}");
        assert_eq!(v["stale"], false, "{json}");
        assert!(v["watermark"].as_u64().unwrap() >= 1, "{json}");
    }

    #[test]
    fn version_names_commit() {
        let line = version_output();
        assert!(
            line.contains(env!("CARGO_PKG_VERSION")),
            "missing semver in {line:?}"
        );

        let sha = build_sha(&line);
        let is_12_hex = sha.len() == 12 && sha.bytes().all(|b| b.is_ascii_hexdigit());
        assert!(
            is_12_hex || sha == "unknown",
            "sha must be 12 hex chars or `unknown`, got {sha:?} in {line:?}"
        );

        if let Some(expected) = option_env!("KATALOGR_GIT_SHA") {
            assert!(
                line.contains(expected),
                "KATALOGR_GIT_SHA={expected} at build time, but output is {line:?}"
            );
        }
    }

    #[test]
    fn find_lists_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let (code, out, _) = run(&repo, &["find", "CALLER"]);
        assert_eq!(code, 0, "{out}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "{out}");
        assert_eq!(lines[0], "fn  app:user.tx#fn:caller_a  user.tx:1");
        assert_eq!(lines[1], "fn  app:user.tx#fn:caller_b  user.tx:2");
    }

    #[test]
    fn refs_lists_references() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let (code, out, _) = run(&repo, &["refs", "target"]);
        assert_eq!(code, 0, "{out}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "{out}");
        assert_eq!(
            lines[0],
            "reexports  app:lib.tx#use:alias  lib.tx:2  [Extracted]"
        );
        assert_eq!(
            lines[1],
            "calls  app:user.tx#fn:caller_a  user.tx:1  [Extracted]"
        );
        assert_eq!(
            lines[2],
            "references  app:user.tx#fn:top  user.tx:3  [Extracted]  via app:lib.tx#use:alias"
        );
    }

    #[test]
    fn callers_with_depth() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let depths = |d: &str| -> Vec<String> {
            let (code, out, _) = run(&repo, &["callers", "target", "--depth", d]);
            assert_eq!(code, 0, "{out}");
            out.lines()
                .map(|l| {
                    let name = l.split_whitespace().nth(1).unwrap().to_string();
                    let depth = l.rsplit(' ').next().unwrap();
                    format!("{name}@{depth}")
                })
                .collect()
        };
        assert_eq!(depths("1"), ["app:user.tx#fn:caller_a@1"]);
        assert_eq!(
            depths("2"),
            ["app:user.tx#fn:caller_a@1", "app:user.tx#fn:caller_b@2"]
        );
        assert_eq!(depths("9").len(), 3);
        // The default is depth 1.
        let (_, out, _) = run(&repo, &["callers", "target"]);
        assert_eq!(out.lines().count(), 1, "{out}");
    }

    #[test]
    fn why_traces_line() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let (code, out, _) = run(&repo, &["why", "user.tx:1"]);
        assert_eq!(code, 0, "{out}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "fn  app:user.tx#fn:caller_a  user.tx:1");
        assert_eq!(lines[1], "  refs-item  story  app:user.tx#item:story");
        assert_eq!(lines.len(), 2, "{out}");
        let (code, out, _) = run(&repo, &["why", "user.tx:3"]);
        assert_eq!((code, out.lines().count()), (0, 1), "{out}");
        let (code, _, err) = run(&repo, &["why", "user.tx:99"]);
        assert_eq!(code, 4, "{err}");
        let (code, _, _) = run(&repo, &["why", "user.tx"]);
        assert_eq!(code, 2);
    }

    #[test]
    fn map_outline() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let (code, out, _) = run(&repo, &["map"]);
        assert_eq!(code, 0, "{out}");
        let degrees: Vec<u64> = out
            .lines()
            .map(|l| l.split_whitespace().next().unwrap().parse().unwrap())
            .collect();
        assert_eq!(degrees.len(), 8, "{out}");
        assert!(degrees.windows(2).all(|w| w[0] >= w[1]), "{out}");
        assert_eq!(
            out.lines().next().unwrap(),
            "   2  fn  app:lib.tx#fn:target  lib.tx:1"
        );
    }

    #[test]
    fn ambiguous_exit_3() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let (code, out, err) = run(&repo, &["refs", "dup"]);
        assert_eq!(code, 3, "{err}");
        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            [
                "fn  app:lib.tx#fn:dup  lib.tx:6",
                "fn  app:user.tx#fn:dup  user.tx:10"
            ]
        );
        // A canonical name is not ambiguous.
        let (code, _, err) = run(&repo, &["refs", "app:lib.tx#fn:dup"]);
        assert_eq!(code, 0, "{err}");
    }

    #[test]
    fn no_match_exit_4() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        for cmd in ["refs", "callers", "find"] {
            let (code, out, err) = run(&repo, &[cmd, "nothing_here"]);
            assert_eq!(code, 4, "{cmd}: {err}");
            assert_eq!(out, "");
            assert_eq!(err.lines().count(), 1, "{err}");
        }
    }

    #[test]
    fn budget_truncates_with_notice() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let (_, full, _) = run(&repo, &["map"]);
        let (code, out, _) = run(&repo, &["map", "--budget", "25"]);
        assert_eq!(code, 0);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "{out}");
        assert_eq!(lines[..2], full.lines().take(2).collect::<Vec<_>>()[..]);
        assert_eq!(
            lines[2],
            "… 6 more omitted (use a larger --budget or narrow the query)"
        );
        // A budget that fits everything adds no notice.
        let (_, out, _) = run(&repo, &["map", "--budget", "1000"]);
        assert_eq!(out, full);
        // JSON truncates the result array and says so.
        let (_, out, _) = run(&repo, &["--json", "map", "--budget", "200"]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let shown = v["truncated"]["shown"].as_u64().unwrap();
        assert_eq!(v["result"].as_array().unwrap().len() as u64, shown);
        assert_eq!(v["truncated"]["omitted"].as_u64().unwrap(), 8 - shown);
        assert!(shown >= 1);
        assert_eq!(v["truncated"]["budget"], 200);
    }

    use serde_json::Value;

    /// Check `instance` against the subset of JSON Schema the generated schemas use: `$ref`,
    /// `type`, `enum`, `required`, `properties`, `items`, `anyOf`/`oneOf`.
    fn validate(root: &Value, schema: &Value, instance: &Value, at: &str) -> Result<(), String> {
        if let Some(r) = schema.get("$ref") {
            let target = r
                .as_str()
                .and_then(|r| r.strip_prefix('#'))
                .and_then(|p| root.pointer(p))
                .ok_or_else(|| format!("{at}: unresolvable $ref {r}"))?;
            return validate(root, target, instance, at);
        }
        for key in ["anyOf", "oneOf"] {
            if let Some(options) = schema.get(key).and_then(Value::as_array) {
                if options
                    .iter()
                    .any(|o| validate(root, o, instance, at).is_ok())
                {
                    return Ok(());
                }
                return Err(format!("{at}: matches none of {key}"));
            }
        }
        if let Some(t) = schema.get("type") {
            let names: Vec<&str> = match t {
                Value::String(s) => vec![s.as_str()],
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                _ => return Err(format!("{at}: bad type keyword")),
            };
            let ok = names.iter().any(|n| match *n {
                "null" => instance.is_null(),
                "boolean" => instance.is_boolean(),
                "string" => instance.is_string(),
                "array" => instance.is_array(),
                "object" => instance.is_object(),
                "integer" => instance.is_i64() || instance.is_u64(),
                "number" => instance.is_number(),
                _ => false,
            });
            if !ok {
                return Err(format!("{at}: expected {names:?}, got {instance}"));
            }
        }
        if let Some(values) = schema.get("enum").and_then(Value::as_array) {
            if !values.contains(instance) {
                return Err(format!("{at}: {instance} not in {values:?}"));
            }
        }
        if let Some(obj) = instance.as_object() {
            for k in schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if !obj.contains_key(k) {
                    return Err(format!("{at}: missing required key {k:?}"));
                }
            }
            if let Some(props) = schema.get("properties").and_then(Value::as_object) {
                for (k, v) in obj {
                    if let Some(p) = props.get(k) {
                        validate(root, p, v, &format!("{at}.{k}"))?;
                    }
                }
            }
        }
        if let (Some(items), Some(arr)) = (schema.get("items"), instance.as_array()) {
            for (i, v) in arr.iter().enumerate() {
                validate(root, items, v, &format!("{at}[{i}]"))?;
            }
        }
        Ok(())
    }

    fn schema_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/v1")
    }

    #[test]
    fn json_matches_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let cases: [(&str, &[&str]); 8] = [
            ("find", &["find", "call"]),
            ("refs", &["refs", "target"]),
            ("callers", &["callers", "target", "--depth", "3"]),
            ("why", &["why", "user.tx:1"]),
            ("why", &["why", "user.tx:3"]),
            ("map", &["map"]),
            ("map", &["map", "--budget", "30"]),
            // Ambiguous candidates are printed as a `find` answer.
            ("find", &["refs", "dup"]),
        ];
        for (name, args) in cases {
            let schema: Value = serde_json::from_str(
                &std::fs::read_to_string(schema_dir().join(format!("{name}.schema.json"))).unwrap(),
            )
            .unwrap();
            let mut full = vec!["--json"];
            full.extend_from_slice(args);
            let (_, out, err) = run(&repo, &full);
            let v: Value =
                serde_json::from_str(&out).unwrap_or_else(|e| panic!("{args:?}: {e}: {out} {err}"));
            validate(&schema, &schema, &v, "$").unwrap_or_else(|e| panic!("{args:?}: {e}\n{out}"));
            assert_eq!(v["schema"], 1);
        }
        // The validator rejects what it should.
        let schema: Value = serde_json::from_str(
            &std::fs::read_to_string(schema_dir().join("find.schema.json")).unwrap(),
        )
        .unwrap();
        let (_, out, _) = run(&repo, &["--json", "find", "call"]);
        let mut v: Value = serde_json::from_str(&out).unwrap();
        v["result"][0]["span"]["start_line"] = Value::String("x".into());
        assert!(validate(&schema, &schema, &v, "$").is_err());
        v["result"][0]["span"]["start_line"] = Value::from(1);
        v["result"][0].as_object_mut().unwrap().remove("kind");
        let err = validate(&schema, &schema, &v, "$").unwrap_err();
        assert!(err.contains("missing required key"), "{err}");
    }
}

mod query {
    use super::cli::{katalogr, setup};
    use fs4::FileExt;
    use std::path::Path;

    #[test]
    fn timeout_returns_stale_exit_code() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = setup(tmp.path());
        // Hold the project's processor lock from this process; the CLI cannot take it.
        let lock = kx.join("projects/demo/.cache/processor.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock)
            .unwrap();
        FileExt::try_lock(&file).expect("take the processor lock");

        let out = katalogr(&repo, &["--json", "index", "--timeout", "0.2"]);
        assert_eq!(out.status.code(), Some(5), "{out:?}");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
        assert_eq!(v["stale"], true);
        assert_eq!(v["pending"], 1);
    }

    #[test]
    fn schemas_current() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/v1");
        let fresh = katalogr_dist::query::schemas();
        assert_eq!(fresh.len(), 6);
        for (name, schema) in fresh {
            let mut want = serde_json::to_string_pretty(&schema).unwrap();
            want.push('\n');
            let file = dir.join(format!("{name}.schema.json"));
            let have = std::fs::read_to_string(&file)
                .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
            assert_eq!(
                have, want,
                "{name} schema is out of date: run `cargo run -p katalogr-query --bin gen-schema`"
            );
        }
    }
}
