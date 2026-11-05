//! Helpers shared by the FEAT-088 test files: a committed, indexed repository of `.tx` files,
//! the CLI, and a checker for the subset of JSON Schema the generated schemas use.
#![allow(dead_code)]

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub fn cli(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_katalogr"))
        .current_dir(dir)
        .env_remove("KATALOGR_KINDEX")
        .env_remove("KATALOGR_PROJECT")
        .env_remove("KATALOGR_CONTEXT")
        .env_remove("KANBANR_DATA_DIR")
        .args(args)
        .output()
        .expect("run katalogr")
}

pub fn ok(dir: &Path, args: &[&str]) -> String {
    let out = cli(dir, args);
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

pub fn git(root: &Path, args: &[&str]) {
    let st = Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .expect("run git");
    assert!(st.success(), "git {args:?} failed");
}

/// The files of the standard repository: a call chain, a contract with two ends and a cycle.
pub const FILES: &[(&str, &str)] = &[
    (
        "lib.tx",
        "sym fn target\nsym fn dup\nexport ns:target target\n",
    ),
    (
        "user.tx",
        "sym fn caller_a\nsym fn caller_b\nsym fn top\nref caller_a calls ns:target\n\
         link caller_b calls caller_a\nlink top calls caller_b\n",
    ),
    (
        "api.tx",
        "sym fn serve\nbind serve provides endpoint:orders\n",
    ),
    (
        "client.tx",
        "sym fn fetch\nbind fetch consumes endpoint:orders\n",
    ),
    ("ring1.tx", "sym fn p\nexport x:p p\nref p calls x:q\n"),
    ("ring2.tx", "sym fn q\nexport x:q q\nref q calls x:p\n"),
];

/// A clean, committed, indexed repository `app` holding `files`, as project `demo`. Returns
/// (kindex, repo).
pub fn project_with(tmp: &Path, files: &[(&str, &str)]) -> (PathBuf, PathBuf) {
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
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Test"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "c1"]);
    ok(&repo, &["index"]);
    (kx, repo)
}

pub fn project(tmp: &Path) -> (PathBuf, PathBuf) {
    project_with(tmp, FILES)
}

/// `katalogr --json view ...` parsed.
pub fn view_json(repo: &Path, args: &[&str]) -> Value {
    let mut all = vec!["--json", "view"];
    all.extend_from_slice(args);
    serde_json::from_str(&ok(repo, &all)).expect("a view answer is JSON")
}

/// The file the store keeps `id` in.
pub fn stored_path(kx: &Path, id: &str) -> PathBuf {
    kx.join("projects/demo/.cache/views")
        .join(&id[3..5])
        .join(format!("{id}.json"))
}

pub fn schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/v1")
}

pub fn view_schema() -> Value {
    serde_json::from_str(&std::fs::read_to_string(schema_dir().join("view.schema.json")).unwrap())
        .unwrap()
}

/// Check `instance` against the subset of JSON Schema the generated schemas use.
pub fn validate(root: &Value, schema: &Value, instance: &Value, at: &str) -> Result<(), String> {
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
