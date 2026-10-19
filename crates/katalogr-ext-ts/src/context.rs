//! The TS/JS project context: every `tsconfig.json`, `jsconfig.json` and `package.json` of a
//! repository, reduced to what specifier resolution needs (FEAT-006 sections 3 and 4).

use katalogr_ext_wasm::WasmPackages;
use katalogr_model::{ExtractError, RelPath};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const TSCONFIG: &str = "tsconfig.json";
pub const JSCONFIG: &str = "jsconfig.json";
pub const PACKAGE_JSON: &str = "package.json";
pub use katalogr_ext_wasm::CARGO_TOML;

/// Serialised as compact JSON with sorted keys and vectors, so equal repositories give equal bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TsContext {
    /// Sorted by `dir`.
    pub projects: Vec<TsProject>,
    /// The wasm packages of the repository (FEAT-016), from its `Cargo.toml` and
    /// `package.json` files. Absent in contexts written before the bridge existed.
    #[serde(default)]
    pub wasm: WasmPackages,
}

/// Fields are declared in key order so the JSON has sorted keys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TsProject {
    /// Effective `compilerOptions.baseUrl` as a repo-relative directory (`""` is the root).
    pub base_url: Option<String>,
    /// Repo-relative directory holding the config files (`""` is the repository root).
    pub dir: String,
    /// Repo-relative entry file of the package (`main`, else `exports`), as written.
    pub entry: Option<String>,
    /// `package.json` `name`.
    pub package_name: Option<String>,
    /// `(pattern, targets)` sorted by pattern. Targets are repo-relative (already joined to
    /// `baseUrl`, or to the directory of the config that declared them) and keep their `*`.
    pub paths: Vec<(String, Vec<String>)>,
}

impl TsContext {
    /// Parse context bytes; empty bytes give an empty context.
    pub fn from_bytes(bytes: &[u8]) -> Result<TsContext, ExtractError> {
        if bytes.is_empty() {
            return Ok(TsContext::default());
        }
        serde_json::from_slice(bytes)
            .map_err(|e| ExtractError::Unsupported(format!("bad TS context: {e}")))
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("context serialises")
    }

    /// The project (longest-prefix directory) that carries `paths` or `baseUrl` for `file`.
    pub fn alias_project(&self, file: &str) -> Option<&TsProject> {
        self.projects
            .iter()
            .filter(|p| (p.base_url.is_some() || !p.paths.is_empty()) && within(&p.dir, file))
            .max_by_key(|p| p.dir.len())
    }

    /// The project whose `package.json` is named `name`.
    pub fn workspace_package(&self, name: &str) -> Option<&TsProject> {
        self.projects
            .iter()
            .find(|p| p.package_name.as_deref() == Some(name))
    }
}

fn within(dir: &str, file: &str) -> bool {
    dir.is_empty() || file.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

pub fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// `tsconfig*.json` or `jsconfig*.json` (`tsconfig.json`, `tsconfig.base.json`, ...).
pub fn is_config_name(name: &str) -> bool {
    name.ends_with(".json") && (name.starts_with("tsconfig") || name.starts_with("jsconfig"))
}

pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Join `rel` to the repo-relative `dir` and normalise `.` and `..`. `None` when the result
/// leaves the repository or `rel` is absolute.
pub fn join_norm(dir: &str, rel: &str) -> Option<String> {
    if rel.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// Remove `//` and `/* */` comments, a BOM and trailing commas, leaving strings untouched.
pub fn strip_jsonc(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                out.push(c);
                i += 1;
                while i < chars.len() {
                    out.push(chars[i]);
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        out.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    i += 1;
                    if chars[i - 1] == '"' {
                        break;
                    }
                }
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                    i += 1;
                }
                i += 2;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    // Trailing commas: a comma followed (after whitespace) by `}` or `]`.
    let b: Vec<char> = out.chars().collect();
    let mut res = String::with_capacity(b.len());
    let mut in_str = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if in_str {
            res.push(c);
            if c == '\\' && i + 1 < b.len() {
                res.push(b[i + 1]);
                i += 1;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
            res.push(c);
        } else if c == ',' {
            let next = b[i + 1..].iter().find(|c| !c.is_whitespace());
            if !matches!(next, Some('}') | Some(']')) {
                res.push(c);
            }
        } else {
            res.push(c);
        }
        i += 1;
    }
    res
}

pub fn parse_jsonc(bytes: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(bytes).ok()?;
    serde_json::from_str(&strip_jsonc(text)).ok()
}

/// What a config contributes after following `extends`.
#[derive(Default, Clone)]
struct Effective {
    /// Repo-relative `baseUrl` directory.
    base_url: Option<String>,
    /// Raw `paths` with the directory of the config that declared them.
    paths: Option<(BTreeMap<String, Vec<String>>, String)>,
}

fn effective(
    path: &str,
    configs: &BTreeMap<String, Value>,
    depth: usize,
    seen: &mut Vec<String>,
) -> Effective {
    let Some(doc) = configs.get(path) else {
        return Effective::default();
    };
    if depth > 16 || seen.iter().any(|s| s == path) {
        return Effective::default();
    }
    seen.push(path.to_string());
    let dir = parent_dir(path);
    let mut eff = Effective::default();
    let extends: Vec<&str> = match doc.get("extends") {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    for ext in extends {
        // Only relative paths inside the repository; packages are not followed.
        if !(ext.starts_with("./") || ext.starts_with("../")) {
            continue;
        }
        let Some(joined) = join_norm(dir, ext) else {
            continue;
        };
        let found = [
            joined.clone(),
            format!("{joined}.json"),
            format!("{joined}/{TSCONFIG}"),
        ]
        .into_iter()
        .find(|c| configs.contains_key(c));
        if let Some(found) = found {
            let base = effective(&found, configs, depth + 1, seen);
            if base.base_url.is_some() {
                eff.base_url = base.base_url;
            }
            if base.paths.is_some() {
                eff.paths = base.paths;
            }
        }
    }
    seen.pop();
    let opts = doc.get("compilerOptions");
    if let Some(b) = opts.and_then(|o| o.get("baseUrl")).and_then(Value::as_str)
        && let Some(abs) = join_norm(dir, b)
    {
        eff.base_url = Some(abs);
    }
    if let Some(p) = opts.and_then(|o| o.get("paths")).and_then(Value::as_object) {
        let map = p
            .iter()
            .map(|(k, v)| {
                let targets = v
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                (k.clone(), targets)
            })
            .collect();
        eff.paths = Some((map, dir.to_string()));
    }
    eff
}

fn entry_of(doc: &Value) -> Option<String> {
    fn first(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            Value::Object(o) => ["import", "default", "require", "types"]
                .iter()
                .find_map(|k| o.get(*k).and_then(first)),
            _ => None,
        }
    }
    if let Some(m) = doc.get("main").and_then(Value::as_str) {
        return Some(m.to_string());
    }
    let exports = doc.get("exports")?;
    match exports {
        Value::Object(o) if o.keys().any(|k| k.starts_with('.')) => o.get(".").and_then(first),
        other => first(other),
    }
}

/// Build the context from the `tsconfig.json`, `jsconfig.json` and `package.json` files of a
/// repository. Files that do not parse are skipped.
pub fn build(files: &[(RelPath, Vec<u8>)]) -> TsContext {
    let mut configs: BTreeMap<String, Value> = BTreeMap::new();
    let mut packages: BTreeMap<String, Value> = BTreeMap::new();
    let mut cargo: Vec<(String, String)> = Vec::new();
    let mut package_texts: Vec<(String, String)> = Vec::new();
    for (p, bytes) in files {
        if let Ok(text) = std::str::from_utf8(bytes) {
            match file_name(p.as_str()) {
                CARGO_TOML => cargo.push((p.as_str().to_string(), text.to_string())),
                PACKAGE_JSON => package_texts.push((p.as_str().to_string(), text.to_string())),
                _ => {}
            }
        }
        let Some(doc) = parse_jsonc(bytes) else {
            continue;
        };
        let name = file_name(p.as_str());
        if name == CARGO_TOML {
            continue;
        }
        if is_config_name(name) {
            configs.insert(p.as_str().to_string(), doc);
        } else if name == PACKAGE_JSON {
            packages.insert(p.as_str().to_string(), doc);
        }
    }
    let mut dirs: BTreeMap<String, TsProject> = BTreeMap::new();
    let blank = |dir: &str| TsProject {
        base_url: None,
        dir: dir.to_string(),
        entry: None,
        package_name: None,
        paths: Vec::new(),
    };
    for path in configs.keys() {
        // Only the standard names define a project; other names are reached through `extends`.
        if ![TSCONFIG, JSCONFIG].contains(&file_name(path)) {
            continue;
        }
        let dir = parent_dir(path);
        // `tsconfig.json` wins over `jsconfig.json` in the same directory.
        if file_name(path) == JSCONFIG && configs.contains_key(&join_file(dir, TSCONFIG)) {
            continue;
        }
        let eff = effective(path, &configs, 0, &mut Vec::new());
        let mut project = blank(dir);
        project.paths = eff
            .paths
            .map(|(map, declared_in)| {
                let base = eff.base_url.clone().unwrap_or(declared_in);
                map.into_iter()
                    .map(|(pat, targets)| {
                        let joined = targets
                            .iter()
                            .filter_map(|t| join_norm(&base, t))
                            .collect::<Vec<_>>();
                        (pat, joined)
                    })
                    .collect()
            })
            .unwrap_or_default();
        project.base_url = eff.base_url;
        dirs.insert(dir.to_string(), project);
    }
    for (path, doc) in &packages {
        let dir = parent_dir(path);
        let project = dirs.entry(dir.to_string()).or_insert_with(|| blank(dir));
        project.package_name = doc.get("name").and_then(Value::as_str).map(str::to_string);
        project.entry = entry_of(doc).and_then(|e| join_norm(dir, &e));
    }
    cargo.sort();
    package_texts.sort();
    TsContext {
        projects: dirs.into_values().collect(),
        wasm: WasmPackages::build(&cargo, &package_texts),
    }
}

fn join_file(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, text: &str) -> (RelPath, Vec<u8>) {
        (RelPath::new(path).unwrap(), text.as_bytes().to_vec())
    }

    #[test]
    fn paths_and_base_url() {
        let files = vec![
            file(
                "tsconfig.base.json",
                "{}", // not a standard name: defines no project of its own
            ),
            file(
                "base/tsconfig.json",
                r#"{ // comment
                  "compilerOptions": {
                    /* block */ "baseUrl": "../src",
                    "paths": { "@lib/*": ["lib/*"], "@cfg": ["config/index.ts",], },
                  },
                }"#,
            ),
            file(
                "app/tsconfig.json",
                r#"{ "extends": "../base/tsconfig.json", "compilerOptions": { "strict": true } }"#,
            ),
            file(
                "pkg/tsconfig.json",
                r#"{ "compilerOptions": { "paths": { "~/*": ["./src/*"] } } }"#,
            ),
            file(
                "pkg/package.json",
                r#"{ "name": "@fx/pkg", "main": "./dist/../src/main.ts" }"#,
            ),
            file("bad/tsconfig.json", "{ not json"),
        ];
        let ctx = build(&files);
        let mut rev = files.clone();
        rev.reverse();
        assert_eq!(ctx.to_bytes(), build(&rev).to_bytes(), "order independent");
        assert_eq!(TsContext::from_bytes(&ctx.to_bytes()).unwrap(), ctx);

        let dirs: Vec<_> = ctx.projects.iter().map(|p| p.dir.as_str()).collect();
        assert_eq!(dirs, ["app", "base", "pkg"], "unparseable config skipped");

        let base = &ctx.projects[1];
        assert_eq!(base.base_url.as_deref(), Some("src"));
        assert_eq!(
            base.paths,
            [
                ("@cfg".to_string(), vec!["src/config/index.ts".to_string()]),
                ("@lib/*".to_string(), vec!["src/lib/*".to_string()]),
            ]
        );
        // `extends` inside the repository: paths and baseUrl are inherited.
        assert_eq!(ctx.projects[0].base_url.as_deref(), Some("src"));
        assert_eq!(ctx.projects[0].paths, base.paths);
        // No baseUrl: targets are relative to the declaring config.
        let pkg = &ctx.projects[2];
        assert_eq!(pkg.base_url, None);
        assert_eq!(
            pkg.paths,
            [("~/*".to_string(), vec!["pkg/src/*".to_string()])]
        );
        assert_eq!(pkg.package_name.as_deref(), Some("@fx/pkg"));
        assert_eq!(pkg.entry.as_deref(), Some("pkg/src/main.ts"));

        assert_eq!(ctx.alias_project("app/x/y.ts").unwrap().dir, "app");
        assert!(ctx.alias_project("other/y.ts").is_none());
        assert_eq!(ctx.workspace_package("@fx/pkg").unwrap().dir, "pkg");
    }

    #[test]
    fn cargo_toml_not_handled() {
        use crate::TsExtractor;
        use katalogr_model::Extractor;
        let cargo = RelPath::new("crates/x/Cargo.toml").unwrap();
        assert!(!TsExtractor.handles(&cargo));
        assert!(TsExtractor.is_context_file(&cargo));
        assert!(TsExtractor.handles(&RelPath::new("web/package.json").unwrap()));
        assert!(TsExtractor.handles(&RelPath::new("tsconfig.base.json").unwrap()));
        assert!(TsExtractor.handles(&RelPath::new("a.ts").unwrap()));
    }

    #[test]
    fn wasm_in_context_bytes() {
        let files = vec![
            file(
                "Cargo.toml",
                "[package]\nname = \"demo-wasm\"\n[dependencies]\nwasm-bindgen = \"0.2\"\n",
            ),
            file(
                "web/package.json",
                r#"{ "dependencies": { "demo-wasm": "file:../pkg" } }"#,
            ),
        ];
        let ctx = build(&files);
        assert_eq!(ctx.wasm.packages.len(), 1);
        assert_eq!(ctx.wasm.packages[0].key, "demo_wasm");
        let mut rev = files.clone();
        rev.reverse();
        assert_eq!(ctx.to_bytes(), build(&rev).to_bytes());
        let json = String::from_utf8(ctx.to_bytes()).unwrap();
        assert!(json.starts_with(r#"{"projects":["#) && json.contains(r#""wasm":{"packages":["#));
        // Contexts written before the bridge read with no wasm packages.
        let old = TsContext::from_bytes(br#"{"projects":[]}"#).unwrap();
        assert!(old.wasm.is_empty());
    }

    #[test]
    fn jsonc_strings_survive() {
        let s = strip_jsonc(r#"{"a": "x // not a comment, ]", "b": [1,2,], }"#);
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["a"], "x // not a comment, ]");
        assert_eq!(v["b"].as_array().unwrap().len(), 2);
    }
}
