//! Fixture corpus runner (FEAT-023): index a case, compare the canonical graph with its
//! `expected.yaml`, and report every difference.
//!
//! Interpretation: `katalogr-dist` exposes no registry or graph-export API (its `build_registry`
//! is private), so the runner takes a registry factory; [`full_registry`] holds the `.tx` test
//! extractor and the Rust, TypeScript, Markdown and board extensions.
//!
//! Setting `KATALOGR_BLESS=1` makes [`run_case`] rewrite the case's `expected.yaml` from the
//! actual result (sorted, keeping the case's mode) and pass.

pub mod privacy;
pub mod testfmt;

use camino::{Utf8Path, Utf8PathBuf};
use katalogr_engine::registry::ExtensionRegistry;
use katalogr_engine::{Consistency, Engine, NewEvent, QueryOptions, Scope};
use katalogr_store::{Kindex, ProjectConfig, ProjectName, RepoConfig};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// Repository name used for every case.
pub const REPO: &str = "fx";

#[derive(Debug, thiserror::Error)]
pub enum AccError {
    #[error("{path}: {message}")]
    Io { path: String, message: String },
    #[error("{path}: {message}")]
    Format { path: String, message: String },
    #[error("{0}")]
    Run(String),
}

impl AccError {
    pub(crate) fn io(path: impl std::fmt::Display, e: impl std::fmt::Display) -> AccError {
        AccError::Io {
            path: path.to_string(),
            message: e.to_string(),
        }
    }

    fn format(path: &Utf8Path, message: impl Into<String>) -> AccError {
        AccError::Format {
            path: path.to_string(),
            message: message.into(),
        }
    }

    fn run(e: impl std::fmt::Display) -> AccError {
        AccError::Run(e.to_string())
    }
}

/// The test extractor plus every shipped language extension.
pub fn full_registry() -> ExtensionRegistry {
    let mut reg = testfmt::registry();
    reg.register(Box::new(katalogr_ext_rust::RustExtractor));
    reg.register(Box::new(katalogr_ext_ts::TsExtractor));
    reg.register(Box::new(katalogr_ext_sql::SqlExtractor));
    // The board extension claims its own Markdown files before the general one.
    reg.register(Box::new(katalogr_ext_kanbanr::BoardExtractor));
    reg.register(Box::new(katalogr_ext_md::MarkdownExtractor));
    reg
}

/// technologies x link patterns -> case directories (relative to the fixtures root).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Manifest {
    pub technologies: BTreeMap<String, BTreeMap<String, Vec<String>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchMode {
    Exact,
    /// Listed items must exist; extras are allowed.
    Subset,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedLink {
    pub from: String,
    pub kind: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub produced_by: Option<String>,
}

impl ExpectedLink {
    fn describe(&self) -> String {
        let mut out = format!("{} -{}-> {}", self.from, self.kind, self.to);
        if let Some(c) = &self.confidence {
            out.push_str(&format!(" [{c}]"));
        }
        if let Some(p) = &self.produced_by {
            out.push_str(&format!(" [produced_by={p}]"));
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    pub mode: MatchMode,
    pub symbols: BTreeSet<String>,
    pub contracts: BTreeSet<String>,
    pub links: BTreeSet<ExpectedLink>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExpected {
    mode: MatchMode,
    #[serde(default)]
    symbols: Vec<String>,
    #[serde(default)]
    contracts: Vec<String>,
    #[serde(default)]
    links: Vec<ExpectedLink>,
}

fn strictly_sorted<T: Ord>(v: &[T]) -> bool {
    v.windows(2).all(|w| w[0] < w[1])
}

impl Expected {
    /// Parse an `expected.yaml`; the lists must be sorted and free of duplicates.
    pub fn load(path: &Utf8Path) -> Result<Expected, AccError> {
        let text = std::fs::read_to_string(path).map_err(|e| AccError::io(path, e))?;
        let raw: RawExpected =
            serde_yaml_ng::from_str(&text).map_err(|e| AccError::format(path, e.to_string()))?;
        if !strictly_sorted(&raw.symbols) {
            return Err(AccError::format(path, "symbols are not sorted and unique"));
        }
        if !strictly_sorted(&raw.contracts) {
            return Err(AccError::format(
                path,
                "contracts are not sorted and unique",
            ));
        }
        if !strictly_sorted(&raw.links) {
            return Err(AccError::format(path, "links are not sorted and unique"));
        }
        Ok(Expected {
            mode: raw.mode,
            symbols: raw.symbols.into_iter().collect(),
            contracts: raw.contracts.into_iter().collect(),
            links: raw.links.into_iter().collect(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub case: String,
    pub missing_symbols: Vec<String>,
    pub unexpected_symbols: Vec<String>,
    pub missing_contracts: Vec<String>,
    pub unexpected_contracts: Vec<String>,
    pub missing_links: Vec<ExpectedLink>,
    pub unexpected_links: Vec<ExpectedLink>,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.missing_symbols.is_empty()
            && self.unexpected_symbols.is_empty()
            && self.missing_contracts.is_empty()
            && self.unexpected_contracts.is_empty()
            && self.missing_links.is_empty()
            && self.unexpected_links.is_empty()
    }

    /// A header line, then one line per difference.
    pub fn render(&self) -> String {
        let mut out = format!(
            "case {}: {}\n",
            self.case,
            if self.passed() { "ok" } else { "MISMATCH" }
        );
        for s in &self.missing_symbols {
            out.push_str(&format!("  missing symbol: {s}\n"));
        }
        for s in &self.unexpected_symbols {
            out.push_str(&format!("  unexpected symbol: {s}\n"));
        }
        for s in &self.missing_contracts {
            out.push_str(&format!("  missing contract: {s}\n"));
        }
        for s in &self.unexpected_contracts {
            out.push_str(&format!("  unexpected contract: {s}\n"));
        }
        for l in &self.missing_links {
            out.push_str(&format!("  missing link: {}\n", l.describe()));
        }
        for l in &self.unexpected_links {
            out.push_str(&format!("  unexpected link: {}\n", l.describe()));
        }
        out
    }
}

/// Read `<root>/manifest.yaml`.
pub fn load_manifest(root: &Utf8Path) -> Result<Manifest, AccError> {
    let path = root.join("manifest.yaml");
    let text = std::fs::read_to_string(&path).map_err(|e| AccError::io(&path, e))?;
    serde_yaml_ng::from_str(&text).map_err(|e| AccError::format(&path, e.to_string()))
}

/// What a canonical graph holds, for comparison.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedGraph {
    pub symbols: BTreeSet<String>,
    pub contracts: BTreeSet<String>,
    pub links: BTreeSet<ExpectedLink>,
}

/// The symbols, contracts and links of a canonical graph export.
pub fn parse_graph(graph: &str) -> Result<ParsedGraph, AccError> {
    let mut symbols = BTreeSet::new();
    let mut contracts = BTreeSet::new();
    let mut links = BTreeSet::new();
    for line in graph.lines() {
        let v: serde_json::Value = serde_json::from_str(line).map_err(AccError::run)?;
        let s = |k: &str| v[k].as_str().map(str::to_string);
        if v["node"] == "symbol" {
            symbols.extend(s("name"));
        } else if v["node"] == "contract" {
            contracts.extend(s("name"));
        } else if v["link"] == true {
            links.insert(ExpectedLink {
                from: s("from").unwrap_or_default(),
                kind: s("kind").unwrap_or_default(),
                to: s("to").unwrap_or_default(),
                confidence: s("confidence"),
                produced_by: s("produced_by"),
            });
        }
    }
    Ok(ParsedGraph {
        symbols,
        contracts,
        links,
    })
}

fn matches(expected: &ExpectedLink, actual: &ExpectedLink) -> bool {
    expected.from == actual.from
        && expected.kind == actual.kind
        && expected.to == actual.to
        && match (&expected.confidence, &actual.confidence) {
            (None, _) => true,
            (Some(e), Some(a)) => e.eq_ignore_ascii_case(a),
            (Some(_), None) => false,
        }
        && match (&expected.produced_by, &actual.produced_by) {
            (None, _) => true,
            (Some(e), Some(a)) => e == a,
            (Some(_), None) => false,
        }
}

/// Set differences on canonical strings (design section 6). An expected link without a
/// confidence matches any confidence; confidences compare ignoring case (the Rust fixtures
/// spell them in lower case). An expected link without `produced_by` matches any provenance.
pub fn compare(case: &str, expected: &Expected, actual: &ParsedGraph) -> Report {
    let (symbols, links) = (&actual.symbols, &actual.links);
    let exact = expected.mode == MatchMode::Exact;
    Report {
        case: case.to_string(),
        missing_symbols: expected.symbols.difference(symbols).cloned().collect(),
        unexpected_symbols: if exact {
            symbols.difference(&expected.symbols).cloned().collect()
        } else {
            Vec::new()
        },
        missing_contracts: expected
            .contracts
            .difference(&actual.contracts)
            .cloned()
            .collect(),
        unexpected_contracts: if exact {
            actual
                .contracts
                .difference(&expected.contracts)
                .cloned()
                .collect()
        } else {
            Vec::new()
        },
        missing_links: expected
            .links
            .iter()
            .filter(|e| !links.iter().any(|a| matches(e, a)))
            .cloned()
            .collect(),
        unexpected_links: if exact {
            links
                .iter()
                .filter(|a| !expected.links.iter().any(|e| matches(e, a)))
                .cloned()
                .collect()
        } else {
            Vec::new()
        },
    }
}

/// Canonical graph text of a case directory indexed as repository `fx` in a temporary kindex.
pub fn index_case(
    make_registry: &dyn Fn() -> ExtensionRegistry,
    case_dir: &Utf8Path,
) -> Result<String, AccError> {
    let case_dir = Utf8PathBuf::from_path_buf(
        std::fs::canonicalize(case_dir).map_err(|e| AccError::io(case_dir, e))?,
    )
    .map_err(|p| AccError::Run(format!("{}: not UTF-8", p.display())))?;
    let tmp = tempfile::tempdir().map_err(AccError::run)?;
    let kx = Utf8PathBuf::from_path_buf(tmp.path().join("kx"))
        .map_err(|p| AccError::Run(format!("{}: not UTF-8", p.display())))?;
    let name = ProjectName::new("fx").map_err(AccError::run)?;
    let dir = Kindex::init(&kx)
        .and_then(|k| {
            k.create_project(
                &name,
                ProjectConfig {
                    repos: vec![RepoConfig::new(REPO)],
                    ..Default::default()
                },
            )
        })
        .map_err(AccError::run)?;
    copy_case_profiles(&case_dir, &dir)?;
    let roots = BTreeMap::from([(REPO.to_string(), case_dir)]);
    let engine = Engine::open(
        dir,
        make_registry(),
        roots,
        Instant::now() + Duration::from_secs(30),
    )
    .map_err(AccError::run)?;
    engine
        .inbox()
        .append(&NewEvent::rescan(REPO))
        .map_err(AccError::run)?;
    let answer = engine
        .with_consistency(
            &QueryOptions {
                consistency: Consistency::WaitAll,
                timeout: Duration::from_secs(30),
                scope: Scope::Project,
            },
            |tx| katalogr_engine::canonical_graph(tx),
        )
        .map_err(AccError::run)?;
    if answer.stale {
        return Err(AccError::Run(
            "index did not drain before the timeout".into(),
        ));
    }
    Ok(answer.value)
}

/// Copy `<case>/_profiles/*.yaml` into the temporary project's profiles directory.
fn copy_case_profiles(
    case_dir: &Utf8Path,
    dir: &katalogr_store::ProjectDir,
) -> Result<(), AccError> {
    let src = case_dir.join("_profiles");
    if !src.is_dir() {
        return Ok(());
    }
    let dest = dir.root().join(katalogr_engine::profiles::PROFILES_DIR);
    std::fs::create_dir_all(&dest).map_err(|e| AccError::io(&dest, e))?;
    for entry in std::fs::read_dir(&src).map_err(|e| AccError::io(&src, e))? {
        let path = entry.map_err(|e| AccError::io(&src, e))?.path();
        if path.extension().is_some_and(|x| x == "yaml") {
            let target = dest.join(
                path.file_name()
                    .expect("file name")
                    .to_string_lossy()
                    .as_ref(),
            );
            std::fs::copy(&path, &target).map_err(|e| AccError::io(target, e))?;
        }
    }
    Ok(())
}

/// Index the case, wait-all, export the canonical graph and compare it with `expected.yaml`.
pub fn run_case(
    make_registry: &dyn Fn() -> ExtensionRegistry,
    case_dir: &Utf8Path,
) -> Result<Report, AccError> {
    let expected = Expected::load(&case_dir.join("expected.yaml"))?;
    let graph = index_case(make_registry, case_dir)?;
    let actual = parse_graph(&graph)?;
    if std::env::var_os("KATALOGR_BLESS").is_some_and(|v| v == "1") {
        bless(case_dir, expected.mode, &actual)?;
        return Ok(compare(
            &label_of(case_dir),
            &Expected::load(&case_dir.join("expected.yaml"))?,
            &actual,
        ));
    }
    Ok(compare(&label_of(case_dir), &expected, &actual))
}

fn label_of(case_dir: &Utf8Path) -> String {
    std::fs::canonicalize(case_dir)
        .ok()
        .and_then(|p| Utf8PathBuf::from_path_buf(p).ok())
        .and_then(|p| {
            let b = p.file_name()?.to_string();
            let a = p.parent()?.file_name()?.to_string();
            Some(format!("{a}/{b}"))
        })
        .unwrap_or_else(|| case_dir.to_string())
}

#[derive(Serialize)]
struct OutExpected<'a> {
    mode: MatchMode,
    symbols: Vec<&'a String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    contracts: Vec<&'a String>,
    links: Vec<&'a ExpectedLink>,
}

/// Rewrite `expected.yaml` from the actual result; lists come out sorted (sets are ordered).
fn bless(case_dir: &Utf8Path, mode: MatchMode, actual: &ParsedGraph) -> Result<(), AccError> {
    let out = OutExpected {
        mode,
        symbols: actual.symbols.iter().collect(),
        contracts: actual.contracts.iter().collect(),
        links: actual.links.iter().collect(),
    };
    let path = case_dir.join("expected.yaml");
    let text =
        serde_yaml_ng::to_string(&out).map_err(|e| AccError::format(&path, e.to_string()))?;
    std::fs::write(&path, text).map_err(|e| AccError::io(&path, e))
}
