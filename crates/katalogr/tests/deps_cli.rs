//! FEAT-007 T8: `katalogr deps` on the command line, over a synthetic lockfile repository.

mod deps_cli {
    use serde_json::Value;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn katalogr(dir: &Path, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(args)
            .output()
            .expect("run katalogr");
        (
            out.status.code().expect("exit code"),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    }

    fn ok(dir: &Path, args: &[&str]) -> String {
        let (code, out, err) = katalogr(dir, args);
        assert_eq!(code, 0, "{args:?}: {err}");
        out
    }

    const CARGO_TOML: &str = "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nalpha-lib = \"1\"\n";
    const CARGO_LOCK: &str = "version = 3\n\n[[package]]\nname = \"alpha-lib\"\nversion = \"1.2.3\"\nsource = \"registry+https://registry.example/index\"\n\n[[package]]\nname = \"alpha-lib\"\nversion = \"2.0.0\"\nsource = \"registry+https://registry.example/index\"\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"alpha-lib 1.2.3\",\n]\n\n[[package]]\nname = \"beta-lib\"\nversion = \"0.9.0\"\nsource = \"registry+https://registry.example/index\"\n";
    const LIB_RS: &str = "use alpha_lib::helper;\n\npub fn run() {\n    helper();\n}\n";

    /// A project `demo` with a marked repo `app`, indexed.
    fn project(tmp: &Path) -> PathBuf {
        let kx = tmp.join("kx");
        let repo = tmp.join("app");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("Cargo.toml"), CARGO_TOML).unwrap();
        std::fs::write(repo.join("Cargo.lock"), CARGO_LOCK).unwrap();
        std::fs::write(repo.join("src/lib.rs"), LIB_RS).unwrap();
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
        ok(&repo, &["index"]);
        repo
    }

    /// Checks `instance` against the parts of the schema that the answer relies on: `$ref`,
    /// `anyOf`, `type`, `required` and `properties`, recursively.
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
                return if options
                    .iter()
                    .any(|o| validate(root, o, instance, at).is_ok())
                {
                    Ok(())
                } else {
                    Err(format!("{at}: matches none of {key}"))
                };
            }
        }
        if let Some(t) = schema.get("type") {
            let names: Vec<&str> = match t {
                Value::String(s) => vec![s.as_str()],
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                _ => return Err(format!("{at}: bad type keyword")),
            };
            let fits = |n: &str| match n {
                "object" => instance.is_object(),
                "array" => instance.is_array(),
                "string" => instance.is_string(),
                "boolean" => instance.is_boolean(),
                "null" => instance.is_null(),
                "integer" => instance.is_i64() || instance.is_u64(),
                "number" => instance.is_number(),
                _ => true,
            };
            if !names.iter().any(|n| fits(n)) {
                return Err(format!("{at}: expected {names:?}, found {instance}"));
            }
        }
        if let (Some(obj), Some(req)) = (
            instance.as_object(),
            schema.get("required").and_then(Value::as_array),
        ) {
            for r in req.iter().filter_map(Value::as_str) {
                if !obj.contains_key(r) {
                    return Err(format!("{at}: missing `{r}`"));
                }
            }
        }
        if let (Some(obj), Some(props)) = (
            instance.as_object(),
            schema.get("properties").and_then(Value::as_object),
        ) {
            for (k, v) in obj {
                if let Some(s) = props.get(k) {
                    validate(root, s, v, &format!("{at}.{k}"))?;
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

    #[test]
    fn lists_versions() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        let out = ok(&repo, &["deps"]);
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("alpha-lib 1.2.3  cargo  app:Cargo.lock")),
            "{out}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("alpha-lib 2.0.0")),
            "{out}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("beta-lib 0.9.0")),
            "{out}"
        );
        // Sorted by package, then version; the pinning member and the users are indented.
        let first = lines
            .iter()
            .position(|l| l.starts_with("alpha-lib 1.2.3"))
            .unwrap();
        assert!(
            lines[first + 1]
                .trim_start()
                .starts_with("pinned by app:Cargo.lock#member:app"),
            "{out}"
        );
        assert!(
            lines[first + 2].trim_start().starts_with("used by "),
            "{out}"
        );
        assert!(lines[first + 2].contains("src/lib.rs"), "{out}");
    }

    #[test]
    fn json_validates() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        let schema: Value = serde_json::from_str(
            &std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/v1/deps.schema.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let out = ok(&repo, &["deps", "--json"]);
        let v: Value = serde_json::from_str(&out).unwrap();
        validate(&schema, &schema, &v, "$").unwrap();
        let hits = v["result"].as_array().unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0]["package"], "alpha-lib");
        assert_eq!(hits[0]["version"], "1.2.3");
        assert_eq!(hits[0]["dev"], false);
    }

    #[test]
    fn package_filter() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        let out = ok(&repo, &["deps", "--package", "beta-lib", "--json"]);
        let v: Value = serde_json::from_str(&out).unwrap();
        let hits = v["result"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["package"], "beta-lib");
        let out = ok(&repo, &["deps", "--package", "nothing", "--json"]);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v["result"].as_array().unwrap().is_empty());
    }

    #[test]
    fn limit_reports_partial() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        let out = ok(&repo, &["deps", "--limit", "2", "--json"]);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["result"].as_array().unwrap().len(), 2);
        let partial = &v["partial"][0];
        assert_eq!(partial["cap"], "limit");
        assert_eq!(partial["at"], 2);
        assert_eq!(partial["shown"], 2);
        assert_eq!(partial["total"], 3);
        let text = ok(&repo, &["deps", "--limit", "2"]);
        assert!(text.contains("partial: capped at 2"), "{text}");
    }
}
