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

    /// Run a query command and return `(exit code, stdout, stderr)`, stdout as printed.
    pub fn run_raw(repo: &Path, args: &[&str]) -> (i32, String, String) {
        let out = katalogr(repo, args);
        (
            out.status.code().expect("exit code"),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    }

    /// Like [`run_raw`], without the closing `next:` hint line of text output.
    pub fn run(repo: &Path, args: &[&str]) -> (i32, String, String) {
        let (code, out, err) = run_raw(repo, args);
        let body = match out.rfind("next: ") {
            Some(i) if (i == 0 || out.as_bytes()[i - 1] == b'\n') && !args.contains(&"--json") => {
                out[..i].to_string()
            }
            _ => out,
        };
        (code, body, err)
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
        assert_eq!(out.lines().count(), 8, "{out}");
        // In-degree is still shown, first on each line.
        let degrees: Vec<u64> = out
            .lines()
            .map(|l| l.split_whitespace().next().unwrap().parse().unwrap())
            .collect();
        assert_eq!(degrees.iter().sum::<u64>(), 6, "{out}");
        // Ordered by PageRank (FEAT-025), ties by name: the most linked-to symbol leads.
        assert_eq!(
            out.lines().next().unwrap(),
            "   2  fn  app:lib.tx#fn:target  lib.tx:1"
        );
        let (_, json, _) = run(&repo, &["--json", "map"]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let ranks: Vec<f64> = v["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["rank"].as_f64().unwrap())
            .collect();
        assert!(ranks.windows(2).all(|w| w[0] >= w[1]), "{ranks:?}");
        let total: f64 = ranks.iter().sum();
        assert!((total - 1.0).abs() < 1e-4, "ranks sum to one: {total}");
    }

    #[test]
    fn map_weights_inferred_links() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = setup(tmp.path());
        std::fs::write(
            repo.join("lib.tx"),
            "sym fn single\nsym fn m1\nsym fn m2\nsym fn m3\n\
             export ns:single single\nexport ns:multi m1\nexport ns:multi m2\n\
             export ns:multi m3\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("user.tx"),
            "sym fn caller\nref caller calls ns:single\nref caller calls ns:multi\n",
        )
        .unwrap();
        ok(&repo, &["index"]);
        let (code, out, _) = run(&repo, &["map"]);
        assert_eq!(code, 0, "{out}");
        let degree = |name: &str| -> String {
            out.lines()
                .find(|l| l.contains(&format!("fn:{name} ")))
                .unwrap_or_else(|| panic!("{name} in {out}"))
                .split_whitespace()
                .next()
                .unwrap()
                .to_string()
        };
        // The extracted reference weighs 1; each of the three inferred targets 1/3.
        assert_eq!(degree("single"), "1", "{out}");
        for m in ["m1", "m2", "m3"] {
            assert_eq!(degree(m), "0.33", "{out}");
        }
        // The fully weighted target leads.
        assert!(out.lines().next().unwrap().contains("fn:single "), "{out}");
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
    fn reexport_not_ambiguous() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = setup(tmp.path());
        std::fs::write(
            repo.join("lib.tx"),
            "sym fn target\nsym use Target\nlink Target reexports target\n",
        )
        .unwrap();
        ok(&repo, &["index"]);
        // Link ends resolve by first plain-name match, so the alias is named `Target`; the
        // case-insensitive tier puts both in one candidate set.
        let (code, _, err) = run(&repo, &["refs", "TARGET"]);
        assert_eq!(code, 0, "{err}");
    }

    #[test]
    fn impl_not_ambiguous_with_its_type() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = setup(tmp.path());
        std::fs::write(
            repo.join("lib.tx"),
            "sym struct Widget\nsym impl widget\nlink widget implements-for Widget\n",
        )
        .unwrap();
        ok(&repo, &["index"]);
        // The `.tx` extractor resolves link ends by first plain-name match, so the impl is
        // named `widget`; the case-insensitive tier puts both in one candidate set.
        let (code, _, err) = run(&repo, &["refs", "WIDGET"]);
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
        let cases: [(&str, &[&str]); 14] = [
            ("find", &["find", "call"]),
            ("refs", &["refs", "target"]),
            ("callers", &["callers", "target", "--depth", "3"]),
            ("why", &["why", "user.tx:1"]),
            ("why", &["why", "user.tx:3"]),
            ("map", &["map"]),
            ("map", &["map", "--budget", "30"]),
            // Ambiguous candidates are printed as a `find` answer.
            ("find", &["refs", "dup"]),
            ("show", &["show", "target"]),
            ("show", &["show", "target", "--signature"]),
            ("search", &["search", "caller"]),
            ("path", &["path", "top", "target"]),
            ("explain", &["explain", "target"]),
            ("explain", &["explain", "caller_a", "--budget", "20"]),
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

    /// Every query command ends 0 when answered, 3 when the text names several symbols and 4
    /// when it names none (R-4). Commands whose answer is a list (`find`, `search`, `map`,
    /// `why`) have no ambiguous case; `map` has no empty case here.
    #[test]
    fn exit_codes_consistent() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        // (ok, ambiguous, none)
        type Args = &'static [&'static str];
        let table: &[(Args, Option<Args>, Option<Args>)] = &[
            (&["find", "target"], None, Some(&["find", "zzz"])),
            (&["search", "target"], None, Some(&["search", "zzzq"])),
            (
                &["show", "target"],
                Some(&["show", "dup"]),
                Some(&["show", "zzz"]),
            ),
            (
                &["explain", "target"],
                Some(&["explain", "dup"]),
                Some(&["explain", "zzz"]),
            ),
            (
                &["refs", "target"],
                Some(&["refs", "dup"]),
                Some(&["refs", "zzz"]),
            ),
            (
                &["callers", "target"],
                Some(&["callers", "dup"]),
                Some(&["callers", "zzz"]),
            ),
            (
                &["path", "caller_a", "target"],
                Some(&["path", "dup", "target"]),
                Some(&["path", "zzz", "target"]),
            ),
            (&["map"], None, None),
            (&["why", "lib.tx:1"], None, Some(&["why", "lib.tx:99"])),
        ];
        for (good, ambiguous, none) in table {
            for json in [false, true] {
                for (args, want) in [(Some(good), 0), (ambiguous.as_ref(), 3), (none.as_ref(), 4)] {
                    let Some(args) = args else { continue };
                    let mut full: Vec<&str> = if json { vec!["--json"] } else { vec![] };
                    full.extend_from_slice(args);
                    let (code, out, err) = run_raw(&repo, &full);
                    assert_eq!(code, want, "{full:?}: {out} {err}");
                    if want == 4 {
                        assert_eq!(out, "", "{full:?}");
                        assert_eq!(err.lines().count(), 1, "{full:?}: {err}");
                    }
                    if want == 3 {
                        assert!(
                            out.lines().count() >= if json { 1 } else { 2 },
                            "{full:?}: candidates on stdout"
                        );
                    }
                }
            }
        }
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
        assert_eq!(fresh.len(), 10);
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

mod retrieval {
    use super::cli::{ok, query_project, run, run_raw, setup};
    use serde_json::Value;
    use std::path::{Path, PathBuf};

    const LIB_RS: &str = "/// Apply a rate code to an amount.\n///\n/// More detail that is not in the first paragraph.\npub fn apply_rate_code(amount: u32) -> u32 {\n    amount + 1\n}\n\npub fn rate(x: u32) -> u32 {\n    x\n}\n";

    /// The `.tx` query project plus a Rust file, indexed.
    fn project_with_rust(tmp: &Path) -> PathBuf {
        let (_kx, repo) = query_project(tmp);
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/lib.rs"), LIB_RS).unwrap();
        ok(&repo, &["index"]);
        repo
    }

    fn json(repo: &Path, args: &[&str]) -> Value {
        let mut full = vec!["--json"];
        full.extend_from_slice(args);
        let (_, out, err) = run_raw(repo, &full);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{args:?}: {e}: {out} {err}"))
    }

    #[test]
    fn show_prints_span() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project_with_rust(tmp.path());
        let (code, out, _) = run(&repo, &["show", "apply_rate_code"]);
        assert_eq!(code, 0, "{out}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "src/lib.rs:1-6");
        let want: Vec<&str> = LIB_RS.lines().take(6).collect();
        assert_eq!(&lines[1..], &want[..], "{out}");
        assert!(!out.contains("pub fn rate("), "only the span is printed");

        let v = json(&repo, &["show", "apply_rate_code"]);
        assert_eq!(v["result"]["lines"], serde_json::json!([1, 6]));
        assert_eq!(v["result"]["file"], "src/lib.rs");
        assert_eq!(
            v["result"]["text"].as_str().unwrap(),
            want.join("\n"),
            "{v}"
        );

        // A one-line symbol of another extension.
        let (code, out, _) = run(&repo, &["show", "target"]);
        assert_eq!(code, 0);
        assert_eq!(out, "lib.tx:1-1\nsym fn target\n");

        let (code, _, err) = run(&repo, &["show", "nothing_here"]);
        assert_eq!(code, 4, "{err}");
        let (code, _, _) = run(&repo, &["show", "dup"]);
        assert_eq!(code, 3);
        // The budget trims the text and says so.
        let (_, out, _) = run(&repo, &["show", "apply_rate_code", "--budget", "15"]);
        assert!(out.contains("more omitted"), "{out}");
    }

    #[test]
    fn show_signature_only() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project_with_rust(tmp.path());
        let (code, out, _) = run(&repo, &["show", "apply_rate_code", "--signature"]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(
            out,
            "src/lib.rs:1-6\npub fn apply_rate_code(amount: u32) -> u32\nApply a rate code to an amount.\n"
        );
        assert!(!out.contains("amount + 1") && !out.contains("More detail"));
        // JSDoc gives the TypeScript extractor's `doc` fact.
        std::fs::write(
            repo.join("util.ts"),
            "/**\n * Take a discount\n * off the price.\n *\n * @param p the price\n */\nexport function applyDiscount(p: number): number {\n  return p;\n}\n",
        )
        .unwrap();
        ok(&repo, &["index"]);
        let (code, ts, _) = run(&repo, &["show", "applyDiscount", "--signature"]);
        assert_eq!(code, 0, "{ts}");
        assert_eq!(
            ts,
            "util.ts:7-9\nfunction applyDiscount(p: number): number\nTake a discount off the price.\n"
        );
        // The signature needs no file: it still works while the file is being edited.
        std::fs::write(repo.join("src/lib.rs"), "changed\n").unwrap();
        let (code, out2, _) = run(&repo, &["show", "apply_rate_code", "--signature"]);
        assert_eq!(code, 0);
        assert_eq!(out, out2);
    }

    #[test]
    fn bm25_identifier_aware() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = setup(tmp.path());
        std::fs::write(
            repo.join("rates.tx"),
            "sym fn rate\nsym fn applyRateCode\nsym fn rateLimit\nsym fn code\nsym fn unrelated\n",
        )
        .unwrap();
        ok(&repo, &["index"]);
        let (code, out, _) = run(&repo, &["search", "rate code"]);
        assert_eq!(code, 0, "{out}");
        let names: Vec<&str> = out
            .lines()
            .map(|l| l.split_whitespace().nth(2).unwrap())
            .collect();
        assert_eq!(names[0], "app:rates.tx#fn:applyRateCode", "{out}");
        assert_eq!(names.len(), 4, "{out}");
        assert!(!out.contains("unrelated"));
        let pos = |n: &str| names.iter().position(|x| x.ends_with(n)).unwrap();
        assert!(
            pos("fn:rate") < pos("fn:rateLimit"),
            "a shorter name scores higher: {out}"
        );
        // Scores fall; words may be given separately, or as an identifier.
        let scores: Vec<f64> = out
            .lines()
            .map(|l| l.split_whitespace().next().unwrap().parse().unwrap())
            .collect();
        assert!(scores.windows(2).all(|w| w[0] >= w[1]), "{out}");
        let (_, split, _) = run(&repo, &["search", "rate", "code"]);
        assert_eq!(split, out);
        let (_, snake, _) = run(&repo, &["search", "apply_rate_code"]);
        assert_eq!(
            snake.lines().next().unwrap().split_whitespace().nth(2),
            Some("app:rates.tx#fn:applyRateCode")
        );
        let (_, limited, _) = run(&repo, &["search", "rate", "--limit", "1"]);
        assert_eq!(limited.lines().count(), 1, "{limited}");
        let (code, _, err) = run(&repo, &["search", "zzzz"]);
        assert_eq!(code, 4, "{err}");
        let (code, _, _) = run(&repo, &["search", "x"]);
        assert_eq!(code, 2, "a word with no searchable token is a usage error");
    }

    #[test]
    fn prefix_terms_match() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = setup(tmp.path());
        std::fs::write(repo.join("s.tx"), "sym fn Stitcher\nsym fn unrelated\n").unwrap();
        ok(&repo, &["index"]);
        let (code, out, _) = run(&repo, &["search", "stitc"]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(out.contains("app:s.tx#fn:Stitcher"), "{out}");
        // Below three characters there is no expansion.
        let (code, _, _) = run(&repo, &["search", "st"]);
        assert_eq!(code, 4);
        // The expansion is capped at the 50 most frequent terms, in a stable order.
        let mut src = String::new();
        for i in 0..60u8 {
            let (a, b) = (char::from(b'a' + i / 26), char::from(b'a' + i % 26));
            src.push_str(&format!("sym fn man{a}{b}\n"));
        }
        std::fs::write(repo.join("many.tx"), src).unwrap();
        ok(&repo, &["index"]);
        let (_, a, _) = run(&repo, &["search", "man", "--limit", "100"]);
        let (_, b, _) = run(&repo, &["search", "man", "--limit", "100"]);
        assert_eq!(a.lines().count(), 50, "{a}");
        assert_eq!(a, b);
    }

    #[test]
    fn exact_beats_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = setup(tmp.path());
        // Same field, same term frequency and length; `rates` only matches `rate` as a prefix.
        std::fs::write(repo.join("r.tx"), "sym fn rates\nsym fn rate\n").unwrap();
        ok(&repo, &["index"]);
        let v = json(&repo, &["search", "rate"]);
        let hits = v["result"].as_array().unwrap();
        assert_eq!(hits.len(), 2, "{v}");
        assert_eq!(hits[0]["symbol"]["name"], "app:r.tx#fn:rate");
        assert_eq!(hits[1]["symbol"]["name"], "app:r.tx#fn:rates");
        let (exact, prefix) = (
            hits[0]["score"].as_f64().unwrap(),
            hits[1]["score"].as_f64().unwrap(),
        );
        assert!((prefix - 0.5 * exact).abs() < 1e-5, "{exact} {prefix}");
    }

    #[test]
    fn prefix_marked_in_explanation() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = setup(tmp.path());
        std::fs::write(repo.join("r.tx"), "sym fn rates\nsym fn rate\n").unwrap();
        ok(&repo, &["index"]);
        let v = json(&repo, &["search", "rate"]);
        let hits = v["result"].as_array().unwrap();
        assert_eq!(hits[0]["matched"][0]["prefix"], false, "{v}");
        assert_eq!(hits[1]["matched"][0]["prefix"], true, "{v}");
        assert_eq!(hits[1]["matched"][0]["term"], "rate");
        let (_, out, _) = run(&repo, &["search", "rate"]);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].ends_with("[rate@name]"), "{out}");
        assert!(lines[1].ends_with("[rate*@name]"), "{out}");
    }

    #[test]
    fn hit_explains_match() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project_with_rust(tmp.path());
        let v = json(&repo, &["search", "amount code"]);
        let hits = v["result"].as_array().unwrap();
        let top = &hits[0];
        assert_eq!(top["symbol"]["name"], "app:src/lib.rs#fn:apply_rate_code");
        let matched: Vec<(String, String)> = top["matched"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                (
                    m["term"].as_str().unwrap().to_string(),
                    m["field"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(
            matched,
            [
                ("amount".to_string(), "doc".to_string()),
                ("code".to_string(), "name".to_string()),
                ("code".to_string(), "doc".to_string())
            ]
        );
        assert!(top["score"].as_f64().unwrap() > 0.0);
        let (_, out, _) = run(&repo, &["search", "amount code"]);
        assert!(
            out.lines()
                .next()
                .unwrap()
                .ends_with("[amount@doc code@name code@doc]"),
            "{out}"
        );
    }

    #[test]
    fn path_shortest_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        // top -references-> alias -reexports-> target is shorter than the three calls.
        let (code, out, _) = run(&repo, &["path", "top", "target"]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            [
                "app:user.tx#fn:top -references-> app:lib.tx#use:alias  [Extracted]",
                "app:lib.tx#use:alias -reexports-> app:lib.tx#fn:target  [Extracted]"
            ]
        );
        // Links are followed backwards too.
        let (code, out, _) = run(&repo, &["path", "target", "top"]);
        assert_eq!(code, 0);
        assert_eq!(
            out.lines().next().unwrap(),
            "app:lib.tx#fn:target <-reexports- app:lib.tx#use:alias  [Extracted]"
        );
        let v = json(&repo, &["path", "target", "top"]);
        assert_eq!(v["result"][0]["direction"], "Backward");
        assert_eq!(v["result"].as_array().unwrap().len(), 2);
        // The chain is bounded.
        let (code, _, err) = run(&repo, &["path", "top", "target", "--max-len", "1"]);
        assert_eq!(code, 4, "{err}");
        let (code, out, _) = run(&repo, &["path", "top", "target", "--max-len", "2"]);
        assert_eq!((code, out.lines().count()), (0, 2));
        let (code, _, _) = run(&repo, &["path", "dup", "top"]);
        assert_eq!(code, 3);
        let (code, _, _) = run(&repo, &["path", "nothing_here", "top"]);
        assert_eq!(code, 4);
    }

    #[test]
    fn explain_compact() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project_with_rust(tmp.path());
        let (code, out, _) = run(&repo, &["explain", "target"]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            [
                "fn  app:lib.tx#fn:target  lib.tx:1",
                "incoming (2):",
                "  calls  app:user.tx#fn:caller_a  [Extracted]",
                "  reexports  app:lib.tx#use:alias  [Extracted]",
            ]
        );
        let (_, out, _) = run(&repo, &["explain", "target", "--per-side", "1"]);
        assert_eq!(
            out.lines().collect::<Vec<_>>()[1..],
            [
                "incoming (first 1 of 2):",
                "  calls  app:user.tx#fn:caller_a  [Extracted]"
            ]
        );
        let (_, out, _) = run(&repo, &["explain", "caller_a"]);
        assert!(out.contains("items: app:user.tx#item:story"), "{out}");
        assert!(out.contains("outgoing (2):"), "{out}");
        let (_, out, _) = run(&repo, &["explain", "apply_rate_code"]);
        assert!(
            out.contains("signature: pub fn apply_rate_code(amount: u32) -> u32")
                && out.contains("doc: Apply a rate code to an amount."),
            "{out}"
        );
        assert!(out.lines().count() <= 6, "compact: {out}");
        let v = json(&repo, &["explain", "target", "--per-side", "1"]);
        assert_eq!(v["result"]["incoming_total"], 2);
        assert_eq!(v["result"]["incoming"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn ranked_map_within_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let v = json(&repo, &["map"]);
        let ranked: Vec<(String, f64)> = v["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["symbol"]["name"].as_str().unwrap().to_string(),
                    e["rank"].as_f64().unwrap(),
                )
            })
            .collect();
        assert_eq!(ranked.len(), 8);
        assert!(ranked.windows(2).all(|w| w[0].1 >= w[1].1), "{ranked:?}");
        // Rank flows along links: the symbol everything reaches leads; leaves of the call
        // chain, which nothing links to, rank last.
        assert_eq!(ranked[0].0, "app:lib.tx#fn:target");
        let rank = |n: &str| ranked.iter().find(|(x, _)| x == n).unwrap().1;
        assert!(rank("app:lib.tx#fn:target") > rank("app:user.tx#fn:caller_a"));
        assert!(rank("app:user.tx#fn:caller_a") > rank("app:user.tx#fn:top"));

        // Trimmed to a budget: the kept lines fit, the notice follows, the order is the same.
        let (_, full, _) = run(&repo, &["map"]);
        for budget in [20u64, 40, 60] {
            let b = budget.to_string();
            let (code, out, _) = run(&repo, &["map", "--budget", &b]);
            assert_eq!(code, 0);
            let mut lines: Vec<&str> = out.lines().collect();
            assert!(lines.last().unwrap().contains("more omitted"), "{out}");
            lines.pop();
            let body = lines.iter().map(|l| format!("{l}\n")).collect::<String>();
            assert!(
                katalogr_dist::query::budget::estimate_tokens(&body) <= budget,
                "{budget}: {body}"
            );
            assert!(!lines.is_empty());
            assert_eq!(
                lines[..],
                full.lines().take(lines.len()).collect::<Vec<_>>()[..]
            );
        }
        let v = json(&repo, &["map", "--budget", "60"]);
        let shown = v["truncated"]["shown"].as_u64().unwrap();
        assert_eq!(v["result"].as_array().unwrap().len() as u64, shown);
    }

    #[test]
    fn replace_span_and_reindex() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project_with_rust(tmp.path());
        let body = tmp.path().join("body.txt");
        std::fs::write(
            &body,
            "pub fn apply_rate_code_v2(amount: u32) -> u32 {\n    amount + 2\n}\n",
        )
        .unwrap();
        let body_s = body.to_str().unwrap();

        // A symbol named in two files is refused, and nothing is written.
        let tx_before = std::fs::read(repo.join("user.tx")).unwrap();
        let (code, _, _) = run(&repo, &["replace", "dup", "--file", body_s]);
        assert_eq!(code, 3);
        assert_eq!(std::fs::read(repo.join("user.tx")).unwrap(), tx_before);

        let (code, out, err) = run(&repo, &["replace", "apply_rate_code", "--file", body_s]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(out, "replaced src/lib.rs:1-6 with src/lib.rs:1-3\n");
        let want = "pub fn apply_rate_code_v2(amount: u32) -> u32 {\n    amount + 2\n}\n\npub fn rate(x: u32) -> u32 {\n    x\n}\n";
        assert_eq!(
            std::fs::read_to_string(repo.join("src/lib.rs")).unwrap(),
            want
        );

        // The file was queued: the next query sees the new symbol and not the old words.
        let (code, out, _) = run(&repo, &["find", "apply_rate_code_v2"]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(out.lines().count(), 1);
        let (code, _, _) = run(&repo, &["find", "apply_rate_code"]);
        assert_eq!(code, 0, "the substring still matches the new name");
        let (code, _, err) = run(&repo, &["search", "amount"]);
        assert_eq!(code, 4, "the old doc is gone from the index: {err}");
        let (_, out, _) = run(&repo, &["show", "apply_rate_code_v2"]);
        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            [
                "src/lib.rs:1-3",
                "pub fn apply_rate_code_v2(amount: u32) -> u32 {",
                "    amount + 2",
                "}"
            ]
        );
        let (_, out, _) = run(&repo, &["show", "rate"]);
        assert!(
            out.starts_with("src/lib.rs:5-7\n") || out.contains("src/lib.rs:5-"),
            "{out}"
        );

        // Line endings of the file are kept.
        std::fs::write(
            repo.join("crlf.rs"),
            "pub fn crlf_fn() -> u32 {\r\n    1\r\n}\r\n",
        )
        .unwrap();
        ok(&repo, &["index"]);
        std::fs::write(&body, "pub fn crlf_fn() -> u32 {\n    2\n    + 3\n}\n").unwrap();
        let (code, out, err) = run(&repo, &["replace", "crlf_fn", "--file", body_s]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(out, "replaced crlf.rs:1-3 with crlf.rs:1-4\n");
        assert_eq!(
            std::fs::read(repo.join("crlf.rs")).unwrap(),
            b"pub fn crlf_fn() -> u32 {\r\n    2\r\n    + 3\r\n}\r\n"
        );

        // A file that changed behind the index's back is never touched: exit 5.
        let edited = format!("// edited\n{want}");
        std::fs::write(repo.join("src/lib.rs"), &edited).unwrap();
        let (code, out, err) = run(&repo, &["replace", "rate", "--file", body_s]);
        assert_eq!(code, 5, "{out}{err}");
        assert!(err.contains("stale"), "{err}");
        assert_eq!(
            std::fs::read_to_string(repo.join("src/lib.rs")).unwrap(),
            edited
        );
        let (code, out, err) = run(&repo, &["show", "rate"]);
        assert_eq!(code, 5, "{out}");
        assert_eq!(out, "", "no span is printed for a stale file");
        assert!(err.contains("katalogr index"), "{err}");
        // Once the change is reported, the answer is fresh again.
        ok(&repo, &["notify", "src/lib.rs"]);
        let (code, out, _) = run(&repo, &["show", "rate"]);
        assert_eq!(code, 0);
        assert!(out.starts_with("src/lib.rs:6-8\n"), "{out}");

        // An empty replacement is a usage error.
        std::fs::write(&body, "\n").unwrap();
        let (code, _, _) = run(&repo, &["replace", "rate", "--file", body_s]);
        assert_eq!(code, 2);
    }

    #[test]
    fn next_query_hints() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let (code, out, _) = run_raw(&repo, &["find", "caller_a"]);
        assert_eq!(code, 0);
        let last = out.lines().last().unwrap();
        assert_eq!(
            last,
            "next: katalogr explain app:user.tx#fn:caller_a; katalogr show app:user.tx#fn:caller_a; katalogr refs app:user.tx#fn:caller_a"
        );
        let v = json(&repo, &["find", "caller_a"]);
        let next: Vec<&str> = v["next"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        assert_eq!(next.len(), 3);
        assert_eq!(next[0], "katalogr explain app:user.tx#fn:caller_a");
        // At most three, for every command that names a symbol.
        for args in [
            &["search", "caller"][..],
            &["refs", "target"],
            &["callers", "target"],
            &["why", "user.tx:1"],
            &["map"],
            &["explain", "target"],
            &["show", "target"],
            &["path", "top", "target"],
        ] {
            let (code, out, _) = run_raw(&repo, args);
            assert_eq!(code, 0, "{args:?}");
            let hint = out.lines().last().unwrap();
            assert!(hint.starts_with("next: katalogr "), "{args:?}: {out}");
            let n = hint.matches("katalogr ").count();
            assert!((1..=3).contains(&n), "{args:?}: {hint}");
            let mut full = vec!["--json"];
            full.extend_from_slice(args);
            let (_, j, _) = run_raw(&repo, &full);
            let v: Value = serde_json::from_str(&j).unwrap();
            assert_eq!(v["next"].as_array().unwrap().len(), n, "{args:?}");
        }
        // The hints come after the budget notice, and every hint is a runnable command.
        let (_, out, _) = run_raw(&repo, &["map", "--budget", "20"]);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[lines.len() - 2].contains("more omitted"), "{out}");
        assert!(lines[lines.len() - 1].starts_with("next: "));
        let first = next[0].strip_prefix("katalogr ").unwrap();
        let args: Vec<&str> = first.split(' ').collect();
        let (code, _, _) = run(&repo, &args);
        assert_eq!(code, 0);
        // No hint when there is nothing to follow up.
        let (_, out, _) = run_raw(&repo, &["refs", "dup"]);
        assert!(!out.contains("next:"), "ambiguous: {out}");
        let (_, out, _) = run_raw(&repo, &["find", "nothing_here"]);
        assert_eq!(out, "");
    }

    #[test]
    fn deterministic_results() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project_with_rust(tmp.path());
        let commands: Vec<Vec<&str>> = vec![
            vec!["find", "call"],
            vec!["refs", "target"],
            vec!["callers", "target", "--depth", "3"],
            vec!["why", "user.tx:1"],
            vec!["map"],
            vec!["map", "--budget", "40"],
            vec!["search", "rate", "code"],
            vec!["search", "amount"],
            vec!["show", "apply_rate_code"],
            vec!["show", "apply_rate_code", "--signature"],
            vec!["path", "top", "target"],
            vec!["path", "target", "top"],
            vec!["explain", "target"],
            vec!["explain", "apply_rate_code"],
        ];
        let mut first = Vec::new();
        for args in &commands {
            for json in [false, true] {
                let mut full = Vec::new();
                if json {
                    full.push("--json");
                }
                full.extend_from_slice(args);
                let a = run_raw(&repo, &full);
                let b = run_raw(&repo, &full);
                assert_eq!(a, b, "{full:?} differs between runs");
                assert_eq!(a.0, 0, "{full:?}: {}", a.2);
                assert!(!a.1.is_empty(), "{full:?}");
                if !json {
                    first.push((args.clone(), a.1));
                }
            }
        }
        // A cache rebuilt from the shards answers the same (text carries no watermark).
        let cache = std::fs::read_dir(repo.parent().unwrap().join("kx/projects/demo/.cache"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.file_name().is_some_and(|n| n == "index.redb"))
            .expect("cache file");
        std::fs::remove_file(cache).unwrap();
        for (args, want) in first {
            let (code, out, _) = run_raw(&repo, &args);
            assert_eq!(code, 0, "{args:?}");
            assert_eq!(out, want, "{args:?} differs after a cache rebuild");
        }
    }
}

mod claude {
    use super::cli::{katalogr, ok, query_project, run_raw, setup};
    use std::path::Path;

    const BEGIN: &str = "<!-- katalogr:begin -->";
    const END: &str = "<!-- katalogr:end -->";

    fn with_home(dir: &Path, home: &Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(dir)
            .env("HOME", home)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(args)
            .output()
            .expect("run katalogr")
    }

    #[test]
    fn sync_marked_block() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = setup(tmp.path());
        let file = repo.join("CLAUDE.md");

        // --show prints the block and writes nothing.
        let shown = ok(&repo, &["claude", "sync", "--show"]);
        assert!(
            shown.starts_with(BEGIN) && shown.trim_end().ends_with(END),
            "{shown}"
        );
        assert!(!file.exists());

        // Creates the file.
        assert!(ok(&repo, &["claude", "sync"]).starts_with("created "));
        let created = std::fs::read_to_string(&file).unwrap();
        assert_eq!(created, shown, "file is the block");
        assert!(created.contains("katalogr skill install"));
        assert!(ok(&repo, &["claude", "sync"]).starts_with("unchanged "));

        // Refreshes a stale block and leaves the text outside the markers alone.
        let stale = format!("# Mine\n\nbefore\n\n{BEGIN}\nold text\n{END}\n\nafter\n");
        std::fs::write(&file, &stale).unwrap();
        assert!(ok(&repo, &["claude", "sync"]).starts_with("updated "));
        let now = std::fs::read_to_string(&file).unwrap();
        assert!(now.starts_with("# Mine\n\nbefore\n\n"), "{now}");
        assert!(now.ends_with("\n\nafter\n"), "{now}");
        assert!(!now.contains("old text"));
        assert_eq!(now.matches(BEGIN).count(), 1);

        // Appends to a file with no markers, keeping its text.
        std::fs::write(&file, "# Mine\nhello").unwrap();
        ok(&repo, &["claude", "sync"]);
        let now = std::fs::read_to_string(&file).unwrap();
        assert!(
            now.starts_with("# Mine\nhello\n\n") && now.contains(BEGIN),
            "{now}"
        );

        // A lone marker is refused with exit 1 and the file is untouched.
        for lone in [format!("x\n{BEGIN}\ny\n"), format!("x\n{END}\ny\n")] {
            std::fs::write(&file, &lone).unwrap();
            let out = katalogr(&repo, &["claude", "sync"]);
            assert_eq!(out.status.code(), Some(1), "{out:?}");
            assert!(String::from_utf8_lossy(&out.stderr).contains("marker"));
            assert_eq!(std::fs::read_to_string(&file).unwrap(), lone);
        }
    }

    #[test]
    fn skill_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let target = home.join(".claude/skills/katalogr/SKILL.md");
        let run = |args: &[&str]| {
            let out = with_home(tmp.path(), &home, args);
            assert!(out.status.success(), "{out:?}");
            String::from_utf8(out.stdout).unwrap()
        };
        assert!(run(&["skill", "install"]).starts_with("installed "));
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            katalogr_dist::SKILL_MD
        );
        assert!(run(&["skill", "install"]).starts_with("unchanged "));
        std::fs::write(&target, "edited").unwrap();
        assert!(run(&["skill", "install"]).starts_with("updated "));
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            katalogr_dist::SKILL_MD
        );

        // --scope repo installs under the repository's .claude.
        let (_kx, repo) = setup(&tmp.path().join("p"));
        let out = with_home(&repo, &home, &["skill", "install", "--scope", "repo"]);
        assert!(
            String::from_utf8_lossy(&out.stdout).starts_with("installed "),
            "{out:?}"
        );
        assert!(repo.join(".claude/skills/katalogr/SKILL.md").is_file());
    }

    fn tokens(s: &str) -> u64 {
        (s.chars().count() as u64).div_ceil(4)
    }

    #[test]
    fn session_start_map_within_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let (code, full, err) = run_raw(&repo, &["session-start"]);
        assert_eq!(code, 0, "{err}");
        assert!(
            full.starts_with("katalogr: project demo (repos: app)"),
            "{full}"
        );
        assert!(
            full.contains("fn  app:lib.tx#fn:target  lib.tx:1"),
            "{full}"
        );
        assert!(
            full.trim_end()
                .ends_with("(find, search, explain, show, refs, why)."),
            "{full}"
        );
        for budget in [400u64, 120, 60] {
            let (code, out, _) =
                run_raw(&repo, &["session-start", "--budget", &budget.to_string()]);
            assert_eq!(code, 0);
            assert!(
                tokens(&out) <= budget,
                "{budget}: {} tokens\n{out}",
                tokens(&out)
            );
            assert!(out.starts_with("katalogr: project demo"), "{out}");
        }
        // A smaller budget shows fewer symbols, the most central first.
        let (_, small, _) = run_raw(&repo, &["session-start", "--budget", "60"]);
        assert!(small.lines().count() < full.lines().count());

        // No index yet: one line naming the setup command, exit 0.
        let empty = tempfile::tempdir().unwrap();
        let (_kx, repo2) = setup(empty.path());
        let (code, out, _) = run_raw(&repo2, &["session-start"]);
        assert_eq!(code, 0);
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(out.contains("katalogr index --full"), "{out}");
        // No project at all: the same.
        let bare = tempfile::tempdir().unwrap();
        let (code, out, _) = run_raw(bare.path(), &["session-start"]);
        assert_eq!(code, 0);
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(out.contains("katalogr project init"), "{out}");
    }

    #[test]
    fn playbooks_present() {
        let skill = katalogr_dist::SKILL_MD;
        for h in [
            "### 3.1", "### 3.2", "### 3.3", "### 3.4", "### 3.5", "### 3.6",
        ] {
            assert!(skill.lines().any(|l| l.starts_with(h)), "missing {h}");
        }
        for h in [
            "## 2. Query craft",
            "## 4. Cost routing",
            "## 5. Output contract",
        ] {
            assert!(skill.lines().any(|l| l.starts_with(h)), "missing {h}");
        }
        assert!(skill.starts_with("---\nname: katalogr\n"));
    }
}
