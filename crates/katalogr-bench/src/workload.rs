//! Frozen query workloads: which symbols and terms each corpus is queried with.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::manifest::Corpus;

pub const WORKLOAD_VERSION: u32 = 1;

pub const DEFAULT_TERMS: &[&[&str]] = &[
    &["error"],
    &["parse"],
    &["config"],
    &["buffer", "size"],
    &["test"],
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workload {
    pub version: u32,
    pub corpus: String,
    /// Canonical names.
    pub targets: Vec<String>,
    pub terms: Vec<Vec<String>>,
    pub paths: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum QueryOp {
    Find(String),
    Refs(String),
    Callers(String),
    Explain(String),
    Tests(String),
    Path(String, String),
    Search(Vec<String>),
    Map,
    Dangling,
    Hubs,
    Cycles,
    Communities,
    DeadCode,
    Mirrors,
    Coverage,
    Doctor { skip_git: bool },
    SessionStart,
}

impl QueryOp {
    /// "find", ..., "dead-code", "doctor", "session-start".
    pub fn command(&self) -> &'static str {
        match self {
            QueryOp::Find(_) => "find",
            QueryOp::Refs(_) => "refs",
            QueryOp::Callers(_) => "callers",
            QueryOp::Explain(_) => "explain",
            QueryOp::Tests(_) => "tests",
            QueryOp::Path(..) => "path",
            QueryOp::Search(_) => "search",
            QueryOp::Map => "map",
            QueryOp::Dangling => "dangling",
            QueryOp::Hubs => "hubs",
            QueryOp::Cycles => "cycles",
            QueryOp::Communities => "communities",
            QueryOp::DeadCode => "dead-code",
            QueryOp::Mirrors => "mirrors",
            QueryOp::Coverage => "coverage",
            QueryOp::Doctor { .. } => "doctor",
            QueryOp::SessionStart => "session-start",
        }
    }

    /// The name results are grouped under: `command`, with `doctor-skip-git` for the
    /// git-free doctor.
    pub fn label(&self) -> &'static str {
        match self {
            QueryOp::Doctor { skip_git: true } => "doctor-skip-git",
            _ => self.command(),
        }
    }

    /// CLI arguments after the global flags.
    pub fn args(&self) -> Vec<String> {
        let mut v = vec![self.command().to_string()];
        match self {
            QueryOp::Find(t)
            | QueryOp::Refs(t)
            | QueryOp::Callers(t)
            | QueryOp::Explain(t)
            | QueryOp::Tests(t) => v.push(t.clone()),
            QueryOp::Path(a, b) => {
                v.push(a.clone());
                v.push(b.clone());
            }
            QueryOp::Search(terms) => v.extend(terms.iter().cloned()),
            QueryOp::Doctor { skip_git: true } => v.push("--skip-git".into()),
            _ => {}
        }
        v
    }
}

/// The plain name of a canonical name: `core:src/lib.rs#fn:helper` gives `helper`,
/// `a:b.ts#method:Foo.bar` gives `bar`.
pub fn short_name(canonical: &str) -> String {
    let after_hash = canonical.rsplit('#').next().unwrap_or(canonical);
    let after_kind = after_hash
        .split_once(':')
        .map(|(_, r)| r)
        .unwrap_or(after_hash);
    let last = after_kind.rsplit("::").next().unwrap_or(after_kind);
    last.rsplit(['.', '/']).next().unwrap_or(last).to_string()
}

/// Split on non-alphanumerics and on lower/upper case boundaries, lowercased.
fn tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for ch in s.chars() {
        if !ch.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if ch.is_uppercase() && prev_lower && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = ch.is_lowercase() || ch.is_ascii_digit();
        cur.extend(ch.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// From the `hubs --level symbol --limit 50` JSON (the whole envelope): `result.items[].node.name`
/// in order, skipping kind `contract` and duplicates; the first `k` are the targets. Terms: for
/// each target, the tokens of its short name with 3 or more characters, the first two. Paths:
/// `(t[i], t[i+1])`. With no targets (text-only corpora): terms are the manifest overrides or
/// [`DEFAULT_TERMS`]. Manifest overrides replace the derived lists when not empty.
pub fn derive(corpus: &Corpus, hubs: &Value, k: usize) -> Workload {
    let mut targets: Vec<String> = Vec::new();
    if let Some(items) = hubs.pointer("/result/items").and_then(Value::as_array) {
        for it in items {
            let node = it.get("node").unwrap_or(it);
            let name = node.get("name").and_then(Value::as_str);
            let kind = node.get("kind").and_then(Value::as_str);
            if let Some(name) = name {
                if kind == Some("contract") || targets.iter().any(|t| t == name) {
                    continue;
                }
                targets.push(name.to_string());
                if targets.len() == k {
                    break;
                }
            }
        }
    }
    if !corpus.queries.targets.is_empty() {
        targets = corpus.queries.targets.clone();
    }
    let mut terms: Vec<Vec<String>> = targets
        .iter()
        .map(|t| {
            tokens(&short_name(t))
                .into_iter()
                .filter(|x| x.chars().count() >= 3)
                .take(2)
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
        .collect();
    if targets.is_empty() && terms.is_empty() {
        terms = DEFAULT_TERMS
            .iter()
            .map(|t| t.iter().map(|s| s.to_string()).collect())
            .collect();
    }
    if !corpus.queries.terms.is_empty() {
        terms = corpus.queries.terms.clone();
    }
    let paths = targets
        .windows(2)
        .map(|w| (w[0].clone(), w[1].clone()))
        .collect();
    Workload {
        version: WORKLOAD_VERSION,
        corpus: corpus.name.clone(),
        targets,
        terms,
        paths,
    }
}

/// Every op for the workload, in `QueryOp` order: per target Find (short name), Refs, Callers,
/// Explain, Tests; Path for each pair; Search for each term list; then the project-wide ops
/// once; Doctor both ways; SessionStart.
pub fn ops(w: &Workload) -> Vec<QueryOp> {
    let mut v = Vec::new();
    for t in &w.targets {
        v.push(QueryOp::Find(short_name(t)));
        v.push(QueryOp::Refs(t.clone()));
        v.push(QueryOp::Callers(t.clone()));
        v.push(QueryOp::Explain(t.clone()));
        v.push(QueryOp::Tests(t.clone()));
    }
    for (a, b) in &w.paths {
        v.push(QueryOp::Path(a.clone(), b.clone()));
    }
    for t in &w.terms {
        v.push(QueryOp::Search(t.clone()));
    }
    v.extend([
        QueryOp::Map,
        QueryOp::Dangling,
        QueryOp::Hubs,
        QueryOp::Cycles,
        QueryOp::Communities,
        QueryOp::DeadCode,
        QueryOp::Mirrors,
        QueryOp::Coverage,
        QueryOp::Doctor { skip_git: false },
        QueryOp::Doctor { skip_git: true },
        QueryOp::SessionStart,
    ]);
    v
}

#[cfg(test)]
fn corpus() -> Corpus {
    let m = crate::manifest::Manifest::parse(
        r#"
version: 1
corpora:
  - name: demo
    tier: S
    category: rust
    languages: [rust]
    primary: main
    repos:
      - { name: main, url: "file:///nowhere", commit: 0123456789abcdef0123456789abcdef01234567, depth: 1 }
    approx: { files: 1, tree_mib: 1, disk_gib: 0.1 }
"#,
    )
    .unwrap();
    m.corpora[0].clone()
}

#[cfg(test)]
fn node(name: &str, kind: &str) -> Value {
    serde_json::json!({"node": {"level": "symbol", "name": name, "kind": kind}})
}

#[cfg(test)]
#[test]
fn targets_from_hubs_deterministic() {
    let hubs = serde_json::json!({"schema": 1, "result": {"level": "symbol", "items": [
        node("main:src/a.rs#fn:parse_config", "fn"),
        node("main:src/a.rs#contract:orders", "contract"),
        node("main:src/a.rs#fn:parse_config", "fn"),
        node("main:src/b.rs#struct:BufferSize", "struct"),
        node("main:src/c.rs#fn:io", "fn"),
        node("main:src/d.rs#fn:helper", "fn"),
        node("main:src/e.rs#fn:other_one", "fn"),
        node("main:src/f.rs#fn:ignored_sixth", "fn"),
    ]}});
    let a = derive(&corpus(), &hubs, 5);
    let b = derive(&corpus(), &hubs, 5);
    assert_eq!(a, b);
    assert_eq!(a.targets.len(), 5);
    assert!(!a.targets.iter().any(|t| t.contains("contract")));
    assert_eq!(a.targets[0], "main:src/a.rs#fn:parse_config");
    assert_eq!(a.terms[0], vec!["parse", "config"]);
    assert_eq!(a.terms[1], vec!["buffer", "size"]);
    // `io` has no token of 3 characters, so it contributes no term list.
    assert_eq!(a.terms.len(), 4);
    assert_eq!(a.paths.len(), 4);
    assert_eq!(a.paths[0], (a.targets[0].clone(), a.targets[1].clone()));
    let all = ops(&a);
    assert_eq!(all[0], QueryOp::Find("parse_config".into()));
    assert_eq!(all[1].args(), vec!["refs", "main:src/a.rs#fn:parse_config"]);
    assert_eq!(all.last(), Some(&QueryOp::SessionStart));
    assert!(all.contains(&QueryOp::Doctor { skip_git: true }));
    assert_eq!(
        QueryOp::Doctor { skip_git: true }.label(),
        "doctor-skip-git"
    );
    assert_eq!(short_name("a:b.ts#method:Foo.bar"), "bar");
    assert_eq!(short_name("a:b.rs#fn:m::inner"), "inner");
}

#[cfg(test)]
#[test]
fn text_corpus_uses_terms() {
    let hubs =
        serde_json::json!({"schema": 1, "result": {"level": "symbol", "nodes": 4, "items": []}});
    let w = derive(&corpus(), &hubs, 5);
    assert!(w.targets.is_empty() && w.paths.is_empty());
    assert_eq!(w.terms.len(), DEFAULT_TERMS.len());
    assert_eq!(w.terms[3], vec!["buffer", "size"]);
    let ops = ops(&w);
    assert!(matches!(ops[0], QueryOp::Search(_)));
    // Overrides replace the defaults.
    let mut c = corpus();
    c.queries.terms = vec![vec!["goroutine".into()]];
    assert_eq!(
        derive(&c, &hubs, 5).terms,
        vec![vec!["goroutine".to_string()]]
    );
}
