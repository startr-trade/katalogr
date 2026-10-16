//! The Rust project context: every crate of a repository, built from its `Cargo.toml` files
//! alone (FEAT-005 sections 3 and 5). katalogr never runs `cargo metadata`.

use katalogr_model::{ExtractError, RelPath};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use toml::{Table, Value};

/// File name of a manifest.
pub const MANIFEST: &str = "Cargo.toml";

/// All crates of a repository. Serialised as compact JSON with sorted keys and sorted vectors,
/// so equal workspaces give equal bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceContext {
    /// Sorted by `dir`.
    pub crates: Vec<CrateInfo>,
}

/// Fields are declared in key order so the JSON has sorted keys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrateInfo {
    /// Sorted by `key`.
    pub deps: Vec<Dep>,
    /// Repo-relative, `/`-separated; empty for a crate at the repository root.
    pub dir: String,
    /// `[lib] name`, else the package name with `-` replaced by `_`.
    pub lib_name: String,
    /// The package name.
    pub name: String,
    /// Sorted by `(kind, name, root)`.
    pub targets: Vec<Target>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetKind {
    Lib,
    Bin,
    Test,
    Example,
    Bench,
}

impl TargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TargetKind::Lib => "lib",
            TargetKind::Bin => "bin",
            TargetKind::Test => "test",
            TargetKind::Example => "example",
            TargetKind::Bench => "bench",
        }
    }
}

/// Fields are declared in key order so the JSON has sorted keys.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Target {
    pub kind: TargetKind,
    pub name: String,
    /// Root file relative to the crate directory, e.g. `src/lib.rs`.
    pub root: String,
}

/// Fields are declared in key order so the JSON has sorted keys.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Dep {
    /// The name used in code: the table key (so after a `package =` rename), `-` as `_`.
    pub key: String,
    /// The real package name.
    pub package: String,
    /// Repo-relative directory of a path dependency inside the repository.
    pub path: Option<String>,
}

impl WorkspaceContext {
    /// Parse context bytes; empty bytes give an empty context.
    pub fn from_bytes(bytes: &[u8]) -> Result<WorkspaceContext, ExtractError> {
        if bytes.is_empty() {
            return Ok(WorkspaceContext::default());
        }
        serde_json::from_slice(bytes)
            .map_err(|e| ExtractError::Unsupported(format!("bad rust context: {e}")))
    }

    /// Compact JSON; deterministic.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a plain data tree always serialises")
    }

    /// The crate whose `dir` is the longest prefix of `path` (a repo-relative file path).
    pub fn crate_of(&self, path: &str) -> Option<&CrateInfo> {
        self.crates
            .iter()
            .filter(|c| {
                c.dir.is_empty()
                    || path
                        .strip_prefix(&c.dir)
                        .is_some_and(|r| r.starts_with('/'))
            })
            .max_by_key(|c| c.dir.len())
    }

    /// The crate at exactly this directory.
    pub fn crate_at(&self, dir: &str) -> Option<&CrateInfo> {
        self.crates.iter().find(|c| c.dir == dir)
    }
}

/// The directory part of a repo-relative file path (`""` for a top-level file).
pub fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// A directory as it appears in an export key: the repository root is `.`.
pub fn key_dir(dir: &str) -> String {
    if dir.is_empty() {
        ".".to_string()
    } else {
        dir.to_string()
    }
}

/// Join `rel` onto `base`, resolving `.` and `..`. `None` when the result leaves the root.
pub fn join_norm(base: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split(['/', '\\']) {
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

/// Build the context from every `Cargo.toml` among `files`. A manifest that does not parse is
/// skipped, so one broken manifest never hides the others.
pub fn build(files: &[(RelPath, Vec<u8>)]) -> WorkspaceContext {
    let mut manifests: BTreeMap<String, Table> = BTreeMap::new();
    for (path, bytes) in files {
        let p = path.as_str();
        if p.rsplit('/').next() != Some(MANIFEST) {
            continue;
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            continue;
        };
        let Ok(doc) = toml::from_str::<Table>(text) else {
            continue;
        };
        manifests.insert(parent_dir(p).to_string(), doc);
    }
    let mut crates = Vec::new();
    for (dir, doc) in &manifests {
        let ws = workspace_for(dir, &manifests);
        if let Some(c) = crate_from(dir, doc, ws) {
            crates.push(c);
        }
    }
    crates.sort_by(|a, b| a.dir.cmp(&b.dir));
    WorkspaceContext { crates }
}

/// The nearest manifest at or above `dir` that has a `[workspace]` table, as
/// `(its directory, its [workspace.dependencies] table)`.
fn workspace_for<'a>(
    dir: &str,
    manifests: &'a BTreeMap<String, Table>,
) -> Option<(&'a str, Option<&'a Table>)> {
    let mut d = dir;
    loop {
        if let Some((k, m)) = manifests.get_key_value(d)
            && let Some(Value::Table(ws)) = m.get("workspace")
        {
            let deps = ws.get("dependencies").and_then(Value::as_table);
            return Some((k.as_str(), deps));
        }
        if d.is_empty() {
            return None;
        }
        d = parent_dir(d);
    }
}

/// One crate from a parsed manifest at `dir`. `ws` is the workspace the manifest belongs to
/// (its directory and `[workspace.dependencies]`), used for `workspace = true` dependencies.
/// `None` for a manifest without `[package]` (a virtual workspace).
pub fn crate_from(dir: &str, doc: &Table, ws: Option<(&str, Option<&Table>)>) -> Option<CrateInfo> {
    let package = doc.get("package")?.as_table()?;
    let name = package.get("name")?.as_str()?.to_string();
    let lib = doc.get("lib").and_then(Value::as_table);
    let lib_name = lib
        .and_then(|l| l.get("name"))
        .and_then(Value::as_str)
        .map_or_else(|| name.replace('-', "_"), str::to_string);

    let mut targets = Vec::new();
    let lib_root = lib
        .and_then(|l| l.get("path"))
        .and_then(Value::as_str)
        .map_or_else(|| Some("src/lib.rs".to_string()), |p| join_norm("", p));
    if let Some(root) = lib_root {
        targets.push(Target {
            kind: TargetKind::Lib,
            name: lib_name.clone(),
            root,
        });
    }
    declared_targets(doc, "bin", TargetKind::Bin, &name, &mut targets);
    if !targets
        .iter()
        .any(|t| t.kind == TargetKind::Bin && t.root == "src/main.rs")
    {
        targets.push(Target {
            kind: TargetKind::Bin,
            name: name.clone(),
            root: "src/main.rs".to_string(),
        });
    }
    declared_targets(doc, "test", TargetKind::Test, &name, &mut targets);
    declared_targets(doc, "example", TargetKind::Example, &name, &mut targets);
    declared_targets(doc, "bench", TargetKind::Bench, &name, &mut targets);
    targets.sort();
    targets.dedup_by(|a, b| a.kind == b.kind && a.root == b.root);

    Some(CrateInfo {
        deps: deps_of(dir, doc, ws),
        dir: dir.to_string(),
        lib_name,
        name,
        targets,
    })
}

/// `[[bin]]`, `[[test]]`, `[[example]]` and `[[bench]]` entries. A missing path takes the
/// Cargo default (`src/main.rs` for the package's own binary, else `src/bin/<name>.rs`,
/// `tests/<name>.rs`, `examples/<name>.rs`, `benches/<name>.rs`).
fn declared_targets(
    doc: &Table,
    section: &str,
    kind: TargetKind,
    package: &str,
    out: &mut Vec<Target>,
) {
    let Some(Value::Array(entries)) = doc.get(section) else {
        return;
    };
    for e in entries.iter().filter_map(Value::as_table) {
        let name = match e.get("name").and_then(Value::as_str) {
            Some(n) => n.to_string(),
            None if kind == TargetKind::Bin => package.to_string(),
            None => continue,
        };
        let default = match kind {
            TargetKind::Bin if name == package => "src/main.rs".to_string(),
            TargetKind::Bin => format!("src/bin/{name}.rs"),
            TargetKind::Test => format!("tests/{name}.rs"),
            TargetKind::Example => format!("examples/{name}.rs"),
            TargetKind::Bench => format!("benches/{name}.rs"),
            TargetKind::Lib => continue,
        };
        let path = e.get("path").and_then(Value::as_str).unwrap_or(&default);
        if let Some(root) = join_norm("", path) {
            out.push(Target { kind, name, root });
        }
    }
}

/// `[dependencies]`, `[dev-dependencies]` and `[build-dependencies]`, at the top level and
/// under `[target.<cfg>.…]`, one entry per key (the first wins).
fn deps_of(dir: &str, doc: &Table, ws: Option<(&str, Option<&Table>)>) -> Vec<Dep> {
    let mut tables: Vec<&Table> = Vec::new();
    let sections = ["dependencies", "dev-dependencies", "build-dependencies"];
    for s in sections {
        tables.extend(doc.get(s).and_then(Value::as_table));
    }
    if let Some(targets) = doc.get("target").and_then(Value::as_table) {
        for t in targets.values().filter_map(Value::as_table) {
            for s in sections {
                tables.extend(t.get(s).and_then(Value::as_table));
            }
        }
    }
    let mut out: BTreeMap<String, Dep> = BTreeMap::new();
    for table in tables {
        for (name, value) in table {
            let key = name.replace('-', "_");
            if out.contains_key(&key) {
                continue;
            }
            let (entry, base): (Option<&Table>, &str) = match value {
                Value::Table(t) if t.get("workspace").and_then(Value::as_bool) == Some(true) => {
                    let inherited = ws.and_then(|(wdir, wdeps)| {
                        wdeps.and_then(|d| d.get(name)).map(|v| (v, wdir))
                    });
                    match inherited {
                        Some((Value::Table(w), wdir)) => (Some(w), wdir),
                        Some((_, wdir)) => (None, wdir),
                        None => (None, dir),
                    }
                }
                Value::Table(t) => (Some(t), dir),
                _ => (None, dir),
            };
            let package = entry
                .and_then(|t| t.get("package"))
                .and_then(Value::as_str)
                .unwrap_or(name)
                .to_string();
            let path = entry
                .and_then(|t| t.get("path"))
                .and_then(Value::as_str)
                .and_then(|p| join_norm(base, p));
            out.insert(key.clone(), Dep { key, package, path });
        }
    }
    out.into_values().collect()
}
