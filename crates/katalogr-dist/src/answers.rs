//! Stored answers (FEAT-096, ADR-0037): the whole-graph commands read their report from
//! `<project>/.cache/answers` when an entry for the same view ID and validity exists, and write
//! it when not. A stored answer is only ever served at a clean, exact position, so a hit is
//! byte-for-byte what a fresh computation would give.

use crate::CoverageOptions;
use camino::Utf8PathBuf;
use katalogr_analytics::{
    CommunityOptions, CycleOptions, DeadCodeOptions, DeadConfidence, GraphOptions, HubOptions,
    Level,
};
use katalogr_query::ViewQuery;
use katalogr_store::ProjectDir;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// The entry format in `validity`; the store's own header format is separate.
pub const ANSWER_FORMAT: u32 = katalogr_store::ANSWER_FORMAT;

/// A question this store knows how to run again. Options hold resolved values, settings applied.
#[derive(Clone, Debug)]
pub enum StoredQuery {
    Hubs(HubOptions),
    Communities(CommunityOptions),
    Cycles(CycleOptions),
    DeadCode(DeadCodeOptions),
    Map,
    Coverage(CoverageOptions),
}

fn json_str<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("a plain value serialises")
}

fn kebab<T: serde::Serialize>(v: &T) -> String {
    json_str(v).trim_matches('"').to_string()
}

fn from_kebab<T: serde::de::DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_str(&json_str(&s)).ok()
}

/// A sorted list as a compact JSON array.
fn list(items: &[String]) -> String {
    let mut v = items.to_vec();
    v.sort();
    json_str(&v)
}

fn graph_args(g: &GraphOptions, a: &mut BTreeMap<String, String>) {
    a.insert("level".into(), kebab(&g.level));
    a.insert("include_inferred".into(), g.include_inferred.to_string());
    a.insert("exclude_kinds".into(), list(&g.exclude_kinds));
}

fn graph_from(a: &BTreeMap<String, String>) -> Option<GraphOptions> {
    let level: Level = from_kebab(a.get("level")?)?;
    let mut g = GraphOptions::new(level, a.get("include_inferred")?.parse().ok()?);
    g.exclude_kinds = serde_json::from_str(a.get("exclude_kinds")?).ok()?;
    Some(g)
}

fn num<T: std::str::FromStr>(a: &BTreeMap<String, String>, k: &str) -> Option<T> {
    a.get(k)?.parse().ok()
}

impl StoredQuery {
    /// The table under Data model. Never holds a budget or output flag. Lists are sorted and held
    /// as a compact JSON array; floats use Rust's shortest `{}` form.
    pub fn to_view_query(&self) -> ViewQuery {
        let mut a = BTreeMap::new();
        let op = match self {
            StoredQuery::Hubs(o) => {
                graph_args(&o.graph, &mut a);
                a.insert("min_degree".into(), format!("{}", o.min_degree));
                a.insert("limit".into(), o.limit.to_string());
                "hubs"
            }
            StoredQuery::Communities(o) => {
                graph_args(&o.graph, &mut a);
                a.insert("seed".into(), o.seed.to_string());
                a.insert("resolution".into(), format!("{}", o.resolution));
                a.insert("min_size".into(), o.min_size.to_string());
                a.insert("members".into(), o.members.to_string());
                a.insert("limit".into(), o.limit.to_string());
                if let Some(of) = &o.of {
                    a.insert("of".into(), of.clone());
                }
                "communities"
            }
            StoredQuery::Cycles(o) => {
                graph_args(&o.graph, &mut a);
                a.insert("members".into(), o.members.to_string());
                a.insert("limit".into(), o.limit.to_string());
                "cycles"
            }
            StoredQuery::DeadCode(o) => {
                a.insert("exclude_kinds".into(), list(&o.exclude_kinds));
                a.insert("internal".into(), o.internal.to_string());
                a.insert("min_confidence".into(), kebab(&o.min_confidence));
                if let Some(p) = &o.path_prefix {
                    a.insert("path_prefix".into(), p.clone());
                }
                a.insert("limit".into(), o.limit.to_string());
                "dead-code"
            }
            StoredQuery::Map => "map",
            StoredQuery::Coverage(o) => {
                if let Some(r) = &o.repo {
                    a.insert("repo".into(), r.clone());
                }
                a.insert("suffix_top".into(), o.suffix_top.to_string());
                "coverage"
            }
        };
        ViewQuery {
            op: op.to_string(),
            args: a,
        }
    }

    /// The inverse. `None` for an unknown op or a bad value. A dead-code query comes back with
    /// no `code_kinds`: the registry supplies them.
    pub fn from_view_query(q: &ViewQuery) -> Option<StoredQuery> {
        let a = &q.args;
        Some(match q.op.as_str() {
            "hubs" => StoredQuery::Hubs(HubOptions {
                graph: graph_from(a)?,
                min_degree: num(a, "min_degree")?,
                limit: num(a, "limit")?,
            }),
            "communities" => StoredQuery::Communities(CommunityOptions {
                graph: graph_from(a)?,
                seed: num(a, "seed")?,
                resolution: num(a, "resolution")?,
                min_size: num(a, "min_size")?,
                members: num(a, "members")?,
                limit: num(a, "limit")?,
                of: a.get("of").cloned(),
            }),
            "cycles" => StoredQuery::Cycles(CycleOptions {
                graph: graph_from(a)?,
                members: num(a, "members")?,
                limit: num(a, "limit")?,
            }),
            "dead-code" => StoredQuery::DeadCode(DeadCodeOptions {
                code_kinds: BTreeMap::<String, BTreeSet<String>>::new(),
                exclude_kinds: serde_json::from_str(a.get("exclude_kinds")?).ok()?,
                internal: num(a, "internal")?,
                min_confidence: from_kebab::<DeadConfidence>(a.get("min_confidence")?)?,
                path_prefix: a.get("path_prefix").cloned(),
                limit: num(a, "limit")?,
            }),
            "map" => StoredQuery::Map,
            "coverage" => StoredQuery::Coverage(CoverageOptions {
                repo: a.get("repo").cloned(),
                suffix_top: num(a, "suffix_top")?,
            }),
            _ => return None,
        })
    }
}

/// Hex SHA-256 of compact JSON, keys sorted: `{"answer_format":1,"build":…,"inputs":…}`.
pub fn validity(build: &str, inputs: &BTreeMap<String, String>) -> String {
    let text = format!(
        "{{\"answer_format\":{ANSWER_FORMAT},\"build\":{},\"inputs\":{}}}",
        json_str(&build),
        json_str(inputs)
    );
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `<project_root>/.cache/answers`, shared by all contexts of the project.
pub(crate) fn answer_store_dir(p: &ProjectDir) -> Utf8PathBuf {
    p.project_root().join(".cache").join("answers")
}
