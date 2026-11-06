//! FEAT-028 T6: `katalogr impact` on the command line, over a synthetic `.tx` repo.

mod impact_cli {
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

    /// A project `demo` with a marked repo `app`: `a` and `b` reach `target`, and `t1` is a test
    /// case calling it. No board is involved anywhere.
    fn project(tmp: &Path) -> PathBuf {
        let kx = tmp.join("kx");
        let repo = tmp.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("lib.tx"),
            "sym fn target\nsym fn a\nsym fn b\nsym fn t1\n\
             link a calls target\nlink b calls a\nlink t1 calls target\nfact t1 test case\n",
        )
        .unwrap();
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
    fn json_validates() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        let schema: Value = serde_json::from_str(
            &std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/v1/impact.schema.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let (code, out, err) = katalogr(&repo, &["--json", "impact", "target"]);
        assert_eq!(code, 0, "{err}");
        let v: Value = serde_json::from_str(&out).unwrap();
        validate(&schema, &schema, &v, "$").unwrap_or_else(|e| panic!("{e}\n{out}"));
        let r = &v["result"];
        assert_eq!(r["seeds"][0]["node"]["name"], "app:lib.tx#fn:target");
        assert_eq!(r["groups"][0]["kind"], "calls");
        assert_eq!(r["groups"][0]["hits"].as_array().unwrap().len(), 2);
        assert_eq!(r["tests"][0]["test"]["name"], "app:lib.tx#fn:t1");
        assert_eq!(r["projects"][0], "demo");
        assert!(v["exact"].is_boolean());
    }

    #[test]
    fn text_lists_groups_and_tests() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        let (code, out, err) = katalogr(&repo, &["impact", "target"]);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("app:lib.tx#fn:target (symbol)"), "{out}");
        assert!(out.contains("project demo"), "{out}");
        assert!(out.contains("calls [extracted] 2"), "{out}");
        assert!(out.contains("  app:lib.tx#fn:b  depth 2"), "{out}");
        assert!(out.contains("tests:"), "{out}");
        assert!(out.contains("app:lib.tx#fn:t1"), "{out}");
        // The test is listed under `tests`, not as a hit.
        assert!(
            !out.lines().any(|l| l.starts_with("  app:lib.tx#fn:t1")),
            "{out}"
        );
    }

    #[test]
    fn works_without_board() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path());
        // A file, with a path relative to the marked repo, and the depth flag.
        let (code, out, err) = katalogr(&repo, &["impact", "--file", "lib.tx", "--depth", "1"]);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("(file)"), "{out}");
        // An item the index does not hold is no match (exit 4); nothing needs a board.
        let (code, _, _) = katalogr(&repo, &["impact", "--item", "FEAT-999"]);
        assert_eq!(code, 4);
        // More than one way to say where to start is a usage error.
        let (code, _, _) = katalogr(&repo, &["impact", "target", "--file", "lib.tx"]);
        assert_eq!(code, 2);
        let (code, _, _) = katalogr(&repo, &["impact", "--range", "a..b", "--at", "abc"]);
        assert_eq!(code, 2);
        let (code, _, _) = katalogr(&repo, &["impact", "--range", "HEAD"]);
        assert_eq!(code, 2);
    }
}
