//! Golden set format 1: versioned queries with graded expected evidence.

use super::EvalError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const GOLDEN_FORMAT: u32 = 1;
pub const GOLDEN_FILE: &str = "golden.yaml";

/// The operation a query runs. `pack` is reserved for context packs (FEAT-038) and rejected.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Op {
    Search,
    Map,
    Refs,
    Explain,
}

/// How a query is expected to end.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Route {
    Found,
    Ambiguous,
    NoMatch,
}

impl Route {
    fn found() -> Route {
        Route::Found
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub name: String,
    /// 1..=3.
    pub grade: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeExpect {
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenQuery {
    pub id: String,
    pub op: Op,
    #[serde(default)]
    pub input: Option<String>,
    #[serde(default = "Route::found")]
    pub route: Route,
    #[serde(default)]
    pub scope: Option<ScopeExpect>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureRepo {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenSet {
    pub format: u32,
    pub set: String,
    pub version: u32,
    #[serde(default)]
    pub description: String,
    /// Fixture mode only: repositories relative to the set's directory.
    #[serde(default)]
    pub repos: Vec<FixtureRepo>,
    pub queries: Vec<GoldenQuery>,
}

fn fmt_err(file: &Path, message: impl Into<String>) -> EvalError {
    EvalError::Format {
        file: file.display().to_string(),
        message: message.into(),
    }
}

fn io_err(file: &Path, e: impl std::fmt::Display) -> EvalError {
    EvalError::Io {
        file: file.display().to_string(),
        message: e.to_string(),
    }
}

/// `[a-z0-9][a-z0-9-]*`.
fn valid_slug(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A repository name: a slug that may also use `_` and `.` after the first character.
fn valid_repo_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Parse and validate one golden file. Returns the set and the file's directory.
pub fn load_set(path: &Path) -> Result<(GoldenSet, PathBuf), EvalError> {
    let text = std::fs::read_to_string(path).map_err(|e| io_err(path, e))?;
    let set: GoldenSet =
        serde_yaml_ng::from_str(&text).map_err(|e| fmt_err(path, e.to_string()))?;
    validate(path, &set)?;
    let dir = path
        .parent()
        .map(Path::to_path_buf)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("."));
    Ok((set, dir))
}

fn validate(file: &Path, set: &GoldenSet) -> Result<(), EvalError> {
    if set.format != GOLDEN_FORMAT {
        return Err(fmt_err(
            file,
            format!(
                "format: {} is not supported (expected {GOLDEN_FORMAT})",
                set.format
            ),
        ));
    }
    if !valid_slug(&set.set) {
        return Err(fmt_err(
            file,
            format!("set: {:?} must match [a-z0-9][a-z0-9-]*", set.set),
        ));
    }
    if set.version < 1 {
        return Err(fmt_err(file, "version: must be an integer >= 1"));
    }
    for r in &set.repos {
        if !valid_repo_name(&r.name) {
            return Err(fmt_err(
                file,
                format!("repos: {:?} is not a valid repo name", r.name),
            ));
        }
        let p = Path::new(&r.path);
        if p.is_absolute()
            || r.path.starts_with('/')
            || p.components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(fmt_err(
                file,
                format!(
                    "repos: path {:?} must be relative and stay inside the set directory",
                    r.path
                ),
            ));
        }
    }
    let mut ids = BTreeSet::new();
    for q in &set.queries {
        let at = |msg: String| fmt_err(file, format!("query {}: {msg}", q.id));
        if !valid_slug(&q.id) {
            return Err(at(format!("id: {:?} must match [a-z0-9][a-z0-9-]*", q.id)));
        }
        if !ids.insert(q.id.as_str()) {
            return Err(at("id: duplicate query id".to_string()));
        }
        let needs_input = !matches!(q.op, Op::Map);
        match (&q.input, needs_input) {
            (None, true) => return Err(at("input: required for this op".to_string())),
            (Some(s), true) if s.trim().is_empty() => {
                return Err(at("input: must not be empty".to_string()));
            }
            (Some(_), false) => return Err(at("input: forbidden for map".to_string())),
            _ => {}
        }
        match (q.route, q.evidence.is_empty()) {
            (Route::Found, true) => {
                return Err(at(
                    "evidence: at least one entry is required when route is found".to_string(),
                ));
            }
            (Route::Ambiguous | Route::NoMatch, false) => {
                return Err(at("evidence: forbidden unless route is found".to_string()));
            }
            _ => {}
        }
        let mut names = BTreeSet::new();
        for e in &q.evidence {
            if !(1..=3).contains(&e.grade) {
                return Err(at(format!(
                    "evidence {:?}: grade {} is outside 1..=3",
                    e.name, e.grade
                )));
            }
            if !names.insert(e.name.as_str()) {
                return Err(at(format!("evidence {:?}: duplicate name", e.name)));
            }
        }
        if let Some(scope) = &q.scope {
            for g in &scope.paths {
                if let Err(e) = globset::Glob::new(g) {
                    return Err(at(format!("scope: invalid glob {g:?}: {e}")));
                }
            }
        }
    }
    Ok(())
}

/// `path` is a golden file, or a directory: its `golden.yaml` if present, else every
/// `*/golden.yaml` one level down, else every `*.yaml` file in it except `baseline.json`
/// (project mode); sorted by path. Duplicate `set` names across files are a Format error.
pub fn load_sets(path: &Path) -> Result<Vec<(GoldenSet, PathBuf)>, EvalError> {
    let files: Vec<PathBuf> = if path.is_dir() {
        let own = path.join(GOLDEN_FILE);
        if own.is_file() {
            vec![own]
        } else {
            let mut nested = Vec::new();
            let mut flat = Vec::new();
            let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
                .map_err(|e| io_err(path, e))?
                .map(|e| e.map(|e| e.path()).map_err(|e| io_err(path, e)))
                .collect::<Result<_, _>>()?;
            entries.sort();
            for p in entries {
                if p.is_dir() {
                    let g = p.join(GOLDEN_FILE);
                    if g.is_file() {
                        nested.push(g);
                    }
                } else if p.extension().is_some_and(|x| x == "yaml") {
                    flat.push(p);
                }
            }
            if nested.is_empty() { flat } else { nested }
        }
    } else {
        vec![path.to_path_buf()]
    };
    let mut out: Vec<(GoldenSet, PathBuf)> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut loaded: Vec<(GoldenSet, PathBuf, PathBuf)> = Vec::new();
    for f in &files {
        let (set, dir) = load_set(f)?;
        loaded.push((set, dir, f.clone()));
    }
    for (set, dir, f) in loaded {
        if !seen.insert(set.set.clone()) {
            return Err(fmt_err(
                &f,
                format!("set: duplicate set name {:?}", set.set),
            ));
        }
        out.push((set, dir));
    }
    Ok(out)
}
