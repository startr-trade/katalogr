//! The SQL extension's repository context (R-10, R-20, R-21): the dialect per directory and the
//! repository's default schema, read from Flyway, Spring Boot, sqlc and SQLFluff configuration.
//!
//! Secrets in those files (`flyway.password`, JDBC URLs with credentials) are never kept: the
//! readers keep only the JDBC scheme of `url`, `defaultSchema`, `schemas` and `locations`.

use crate::DEFAULT_SCHEMA;
use crate::dialect::Dialect;
use crate::names::{fold_schema, normalise_ident};
use katalogr_model::RelPath;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqlContext {
    /// Directories with a configured dialect, sorted by (dir, source rank, source).
    #[serde(default)]
    pub dirs: Vec<DialectDir>,
    /// The repository's default schema, normalised and folded; None = no configuration sets
    /// one, so `DEFAULT_SCHEMA` applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<SchemaChoice>,
    /// Other configurations that named a different schema and lost, sorted by source.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schema_ignored: Vec<SchemaChoice>,
    /// Format version: 2 (1 had no schema).
    #[serde(default)]
    pub version: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DialectDir {
    pub dialect: Dialect,
    /// Repo-relative, `""` = root, no trailing `/`.
    pub dir: String,
    /// The config path.
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SchemaChoice {
    /// Normalised, folded.
    pub name: String,
    /// The config path.
    pub source: String,
}

/// True for Maven/Gradle test resources (a `src/test/` segment pair): their configuration
/// neither sets the schema nor triggers a rescan.
pub fn is_test_resource(path: &str) -> bool {
    path.starts_with("src/test/") || path.contains("/src/test/")
}

/// The schema carried by embedded context bytes: the UTF-8 text when non-empty and valid,
/// else `DEFAULT_SCHEMA`.
pub fn schema_from_embedded(bytes: &[u8]) -> &str {
    match std::str::from_utf8(bytes) {
        Ok(s) if !s.is_empty() => s,
        _ => DEFAULT_SCHEMA,
    }
}

/// Rank of a config file by name, for ordering: sqlc < flyway.toml < flyway.conf <
/// application.properties < application.yml/yaml < .sqlfluff.
fn source_rank(path: &str) -> u8 {
    match file_name(path) {
        "flyway.toml" => 1,
        "flyway.conf" => 2,
        "application.properties" => 3,
        "application.yml" | "application.yaml" => 4,
        ".sqlfluff" => 5,
        _ => 0,
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl SqlContext {
    /// Never fails; unreadable configs are skipped.
    pub fn build(files: &[(RelPath, Vec<u8>)]) -> SqlContext {
        let mut dirs: Vec<DialectDir> = Vec::new();
        let mut candidates: Vec<(usize, u8, SchemaChoice)> = Vec::new();
        for (path, bytes) in files {
            let p = path.as_str();
            if is_test_resource(p) {
                continue;
            }
            let Ok(text) = std::str::from_utf8(bytes) else {
                continue;
            };
            let Some(read) = read_config(p, text) else {
                continue;
            };
            for (dir, dialect) in read.dirs {
                dirs.push(DialectDir {
                    dialect,
                    dir,
                    source: p.to_string(),
                });
            }
            if let Some(raw) = read.schema {
                let name = fold_schema(&normalise_ident(&raw));
                if !name.is_empty() {
                    candidates.push((
                        p.split('/').count(),
                        source_rank(p),
                        SchemaChoice {
                            name,
                            source: p.to_string(),
                        },
                    ));
                }
            }
        }
        dirs.sort_by(|a, b| {
            (&a.dir, source_rank(&a.source), &a.source, a.dialect).cmp(&(
                &b.dir,
                source_rank(&b.source),
                &b.source,
                b.dialect,
            ))
        });
        dirs.dedup();
        let (schema, schema_ignored) = resolve_schema(candidates);
        SqlContext {
            dirs,
            schema,
            schema_ignored,
            version: 2,
        }
    }

    /// Compact JSON; an empty Vec when dirs, schema and schema_ignored are all empty.
    pub fn to_bytes(&self) -> Vec<u8> {
        if self.dirs.is_empty() && self.schema.is_none() && self.schema_ignored.is_empty() {
            return Vec::new();
        }
        let mut c = self.clone();
        c.version = 2;
        serde_json::to_vec(&c).unwrap_or_default()
    }

    /// Empty or invalid bytes give the default context. Version 1 bytes (no schema) are read.
    pub fn from_bytes(b: &[u8]) -> SqlContext {
        if b.is_empty() {
            return SqlContext::default();
        }
        serde_json::from_slice(b).unwrap_or_default()
    }

    /// The longest directory that is a path prefix at a `/` boundary, with its config path.
    /// Equal directories resolve to the first in source-rank order.
    pub fn dialect_for(&self, path: &str) -> Option<(Dialect, &str)> {
        let mut best: Option<&DialectDir> = None;
        for d in &self.dirs {
            let hit = d.dir.is_empty()
                || (path.len() > d.dir.len()
                    && path.starts_with(d.dir.as_str())
                    && path.as_bytes()[d.dir.len()] == b'/');
            if hit && best.is_none_or(|b| d.dir.len() > b.dir.len()) {
                best = Some(d);
            }
        }
        best.map(|d| (d.dialect, d.source.as_str()))
    }

    /// `schema.name`, else `DEFAULT_SCHEMA`.
    pub fn default_schema(&self) -> &str {
        self.schema.as_ref().map_or(DEFAULT_SCHEMA, |s| &s.name)
    }

    /// The guest's embedded context: the UTF-8 bytes of the default schema when it is not
    /// `DEFAULT_SCHEMA`, else empty. Nothing else (dialect dirs, sources) is included.
    pub fn embedded_bytes(&self) -> Vec<u8> {
        let s = self.default_schema();
        if s == DEFAULT_SCHEMA {
            Vec::new()
        } else {
            s.as_bytes().to_vec()
        }
    }
}

/// Choose the repository's schema: candidates ordered by (config path depth, source rank, path);
/// the first wins, and later candidates naming a different schema are returned as ignored,
/// sorted by source.
pub fn resolve_schema(
    mut candidates: Vec<(usize, u8, SchemaChoice)>,
) -> (Option<SchemaChoice>, Vec<SchemaChoice>) {
    candidates.sort_by(|a, b| (a.0, a.1, &a.2.source).cmp(&(b.0, b.1, &b.2.source)));
    let mut it = candidates.into_iter().map(|c| c.2);
    let Some(winner) = it.next() else {
        return (None, Vec::new());
    };
    let mut ignored: Vec<SchemaChoice> = it.filter(|c| c.name != winner.name).collect();
    ignored.sort_by(|a, b| a.source.cmp(&b.source));
    ignored.dedup();
    (Some(winner), ignored)
}

// --- readers -------------------------------------------------------------------------------

struct Read {
    dirs: Vec<(String, Dialect)>,
    /// The raw schema value (defaultSchema, else first of schemas).
    schema: Option<String>,
}

/// The Flyway keys kept from any configuration form.
#[derive(Default)]
struct Flyway {
    url: Option<String>,
    locations: Option<Vec<String>>,
    default_schema: Option<String>,
    schemas: Vec<String>,
}

impl Flyway {
    fn schema(&self) -> Option<String> {
        let d = self.default_schema.as_deref().map(str::trim).unwrap_or("");
        if !d.is_empty() {
            return Some(d.to_string());
        }
        self.schemas
            .iter()
            .map(|s| s.trim())
            .find(|s| !s.is_empty())
            .map(str::to_string)
    }

    /// Feed one `key` (already stripped of its prefix and relaxed) and its values.
    fn put(&mut self, key: &str, values: Vec<String>) {
        let split = |vs: &[String]| -> Vec<String> {
            vs.iter()
                .flat_map(|v| v.split(','))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        match key {
            "url" => self.url = values.into_iter().next().map(|v| v.trim().to_string()),
            "defaultschema" => {
                self.default_schema = values.into_iter().next().map(|v| v.trim().to_string())
            }
            "schemas" => self.schemas = split(&values),
            "locations" => self.locations = Some(split(&values)),
            _ => {}
        }
    }
}

fn relax(key: &str) -> String {
    key.chars()
        .filter(|c| *c != '-' && *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

/// `jdbc:<scheme>:` to a dialect.
fn url_dialect(url: &str) -> Option<Dialect> {
    let rest = url.trim().strip_prefix("jdbc:")?;
    let scheme = rest.split(':').next()?;
    Dialect::from_name(scheme).filter(|d| *d != Dialect::Generic)
}

/// Directory of a config: its parent, or for `conf/flyway.conf` the grandparent.
fn config_dir(path: &str, flyway_conf: bool) -> String {
    let parent = path.rsplit_once('/').map_or("", |(p, _)| p);
    if flyway_conf {
        if parent == "conf" {
            return String::new();
        }
        if let Some(gp) = parent.strip_suffix("/conf") {
            return gp.to_string();
        }
    }
    parent.to_string()
}

/// Join `rel` onto `base` and normalise `.` and `..`; None when it leaves the repository.
fn join_dir(base: &str, rel: &str) -> Option<String> {
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

fn read_config(path: &str, text: &str) -> Option<Read> {
    let name = file_name(path);
    match name {
        "flyway.conf" => {
            let d = config_dir(path, true);
            let mut fw = Flyway::default();
            for (k, v) in properties(text) {
                if let Some(rest) = k.to_lowercase().strip_prefix("flyway.") {
                    fw.put(&relax(rest), vec![v]);
                }
            }
            Some(flyway_read(&d, &fw, false))
        }
        "application.properties" => {
            let d = config_dir(path, false);
            let mut fw = Flyway::default();
            for (k, v) in properties(text) {
                spring_put(&mut fw, &k, vec![v]);
            }
            Some(flyway_read(&d, &fw, true))
        }
        "application.yml" | "application.yaml" => {
            let d = config_dir(path, false);
            let mut fw = Flyway::default();
            for doc in yaml_docs(text) {
                let mut flat = Vec::new();
                flatten("", &doc, &mut flat);
                for (k, vs) in flat {
                    spring_put(&mut fw, &k, vs);
                }
            }
            Some(flyway_read(&d, &fw, true))
        }
        "flyway.toml" => read_flyway_toml(&config_dir(path, false), text),
        "sqlc.yaml" | "sqlc.yml" | "sqlc.json" => read_sqlc(&config_dir(path, false), text),
        ".sqlfluff" => read_sqlfluff(&config_dir(path, false), text),
        _ => None,
    }
}

fn spring_put(fw: &mut Flyway, key: &str, values: Vec<String>) {
    let lower = key.to_lowercase();
    let rest = lower
        .strip_prefix("spring.flyway.")
        .or_else(|| lower.strip_prefix("flyway."));
    if let Some(rest) = rest {
        fw.put(&relax(rest), values);
    }
}

/// Dirs and schema from the kept Flyway keys. `classpath` selects the Spring Boot location
/// rules; otherwise `filesystem:` locations apply.
fn flyway_read(d: &str, fw: &Flyway, classpath: bool) -> Read {
    let mut dirs = Vec::new();
    if let Some(dialect) = fw.url.as_deref().and_then(url_dialect) {
        dirs.push((d.to_string(), dialect));
        let prefix = if classpath {
            "classpath:"
        } else {
            "filesystem:"
        };
        match &fw.locations {
            Some(locs) => {
                for l in locs {
                    if let Some(p) = l.strip_prefix(prefix)
                        && let Some(dir) = join_dir(d, p)
                    {
                        dirs.push((dir, dialect));
                    }
                }
            }
            None if classpath => {
                if let Some(dir) = join_dir(d, "db/migration") {
                    dirs.push((dir, dialect));
                }
            }
            None => {}
        }
    }
    Read {
        dirs,
        schema: fw.schema(),
    }
}

/// Java properties: `key=value` or `key: value`, `#` and `!` comments.
fn properties(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with('!') {
            continue;
        }
        let Some(i) = l.find(['=', ':']) else {
            continue;
        };
        out.push((l[..i].trim().to_string(), l[i + 1..].trim().to_string()));
    }
    out
}

fn yaml_docs(text: &str) -> Vec<serde_yaml_ng::Value> {
    use serde::Deserialize;
    let mut out = Vec::new();
    for doc in serde_yaml_ng::Deserializer::from_str(text) {
        match serde_yaml_ng::Value::deserialize(doc) {
            Ok(v) => out.push(v),
            Err(_) => break,
        }
    }
    out
}

fn scalar(v: &serde_yaml_ng::Value) -> Option<String> {
    use serde_yaml_ng::Value;
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Flatten nested maps to dotted keys; a list value becomes its scalar items in order.
fn flatten(prefix: &str, v: &serde_yaml_ng::Value, out: &mut Vec<(String, Vec<String>)>) {
    use serde_yaml_ng::Value;
    match v {
        Value::Mapping(m) => {
            for (k, child) in m {
                let Some(k) = scalar(k) else { continue };
                let key = if prefix.is_empty() {
                    k
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(&key, child, out);
            }
        }
        Value::Sequence(items) => {
            let vs: Vec<String> = items.iter().filter_map(scalar).collect();
            out.push((prefix.to_string(), vs));
        }
        other => {
            if let Some(s) = scalar(other) {
                out.push((prefix.to_string(), vec![s]));
            }
        }
    }
}

fn read_flyway_toml(d: &str, text: &str) -> Option<Read> {
    let doc: toml::Table = text.parse().ok()?;
    let strs = |v: &toml::Value| -> Vec<String> {
        match v {
            toml::Value::String(s) => vec![s.clone()],
            toml::Value::Array(a) => a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
            _ => Vec::new(),
        }
    };
    let flyway = doc.get("flyway").and_then(|v| v.as_table());
    let envs = doc.get("environments").and_then(|v| v.as_table());
    let selected = envs.and_then(|e| {
        let named = flyway
            .and_then(|f| f.get("environment"))
            .and_then(|v| v.as_str())
            .and_then(|n| e.get(n).map(|t| (n.to_string(), t)));
        named.or_else(|| {
            let mut names: Vec<&String> = e.keys().collect();
            names.sort();
            names.first().map(|n| ((*n).clone(), &e[*n]))
        })
    });
    let env_table = selected.as_ref().and_then(|(_, t)| t.as_table());
    let mut fw = Flyway::default();
    if let Some(u) = env_table
        .and_then(|t| t.get("url"))
        .and_then(|v| v.as_str())
    {
        fw.url = Some(u.to_string());
    }
    if let Some(l) = flyway.and_then(|f| f.get("locations")) {
        fw.locations = Some(strs(l));
    }
    if let Some(s) = flyway
        .and_then(|f| f.get("defaultSchema"))
        .and_then(|v| v.as_str())
    {
        fw.default_schema = Some(s.to_string());
    }
    fw.schemas = match env_table.and_then(|t| t.get("schemas")) {
        Some(v) => strs(v),
        None => flyway
            .and_then(|f| f.get("schemas"))
            .map(strs)
            .unwrap_or_default(),
    };
    Some(flyway_read(d, &fw, false))
}

fn read_sqlc(d: &str, text: &str) -> Option<Read> {
    let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(text).ok()?;
    let mut dirs = Vec::new();
    for entry in doc.get("sql")?.as_sequence()? {
        let Some(dialect) = entry
            .get("engine")
            .and_then(|v| v.as_str())
            .and_then(Dialect::from_name)
        else {
            continue;
        };
        for key in ["schema", "queries"] {
            let Some(v) = entry.get(key) else { continue };
            let paths: Vec<String> = match v {
                serde_yaml_ng::Value::Sequence(s) => s.iter().filter_map(scalar).collect(),
                other => scalar(other).into_iter().collect(),
            };
            for p in paths {
                let rel = if p.to_ascii_lowercase().ends_with(".sql") {
                    p.rsplit_once('/').map_or("", |(a, _)| a).to_string()
                } else {
                    p
                };
                if let Some(dir) = join_dir(d, &rel) {
                    dirs.push((dir, dialect));
                }
            }
        }
    }
    Some(Read { dirs, schema: None })
}

fn read_sqlfluff(d: &str, text: &str) -> Option<Read> {
    let mut in_section = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('#') || l.starts_with(';') || l.is_empty() {
            continue;
        }
        if l.starts_with('[') {
            in_section = l.eq_ignore_ascii_case("[sqlfluff]");
            continue;
        }
        if in_section
            && let Some((k, v)) = l.split_once('=')
            && k.trim().eq_ignore_ascii_case("dialect")
            && let Some(dialect) = Dialect::from_name(v.trim())
        {
            return Some(Read {
                dirs: vec![(d.to_string(), dialect)],
                schema: None,
            });
        }
    }
    Some(Read {
        dirs: Vec::new(),
        schema: None,
    })
}
