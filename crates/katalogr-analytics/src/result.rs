//! Result types of the five insights. Their serialised form is the stable JSON contract
//! (`schema/v1`). `SymbolHit`, `PathStep` and `Direction` are the query crate's.

use katalogr_model::Confidence;
use katalogr_query::result::{PathStep, SymbolHit};
use schemars::JsonSchema;
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::BTreeMap;

/// Schema-only mirror of [`katalogr_model::Confidence`].
#[derive(JsonSchema)]
#[schemars(rename = "Confidence")]
#[allow(dead_code)]
enum ConfidenceSchema {
    Extracted,
    Inferred,
    Declared,
}

/// The granularity at which nodes are grouped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Level {
    Symbol,
    File,
    Package,
}

/// A node of the insight graph at some [`Level`].
///
/// Symbol: canonical name, symbol kind, path, start line. File: artifact canonical name,
/// `artifact`, path, no line. Package: the package fact value, `package`, the longest common
/// directory of its files when they share a repository (else none), no line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct NodeHit {
    pub level: Level,
    pub name: String,
    pub kind: String,
    pub path: Option<String>,
    pub line: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct HubEntry {
    pub node: NodeHit,
    /// Weighted in-degree, 2 decimal places.
    pub in_degree: f64,
    /// Weighted out-degree, 2 decimal places.
    pub out_degree: f64,
    /// In plus out, 2 decimal places.
    pub degree: f64,
    /// Degree z-score over the nodes that have links, 2 decimal places.
    pub z: f64,
    /// PageRank score (summed over the symbols of a file or package), 6 decimal places.
    pub rank: f64,
    /// Symbols in the node (1 at symbol level).
    pub members: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct HubReport {
    pub level: Level,
    /// Nodes of the graph at this level.
    pub nodes: u64,
    pub mean_degree: f64,
    pub items: Vec<HubEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct Community {
    pub id: u32,
    pub size: u64,
    pub label: NodeHit,
    /// Longest common directory of the members' paths.
    pub dir: Option<String>,
    /// Internal weight over internal plus boundary weight, 3 decimal places.
    pub cohesion: f64,
    pub internal_weight: f64,
    pub boundary_weight: f64,
    pub top: Vec<NodeHit>,
    pub bridges: Vec<NodeHit>,
    /// Distinct files among the members.
    pub files: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct CommunityReport {
    pub level: Level,
    pub seed: u64,
    pub resolution: f64,
    /// Modularity of the partition, 4 decimal places.
    pub modularity: f64,
    pub communities_total: u64,
    /// Nodes in no reported community (isolated, or in one below the minimum size).
    pub unclustered: u64,
    pub note: Option<String>,
    pub items: Vec<Community>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct Cycle {
    pub id: u32,
    pub size: u64,
    pub members: Vec<NodeHit>,
    pub members_total: u64,
    /// One shortest cycle through the first member by name.
    pub witness: Vec<PathStep>,
    /// The weakest confidence on the witness.
    #[schemars(with = "ConfidenceSchema")]
    pub confidence: Confidence,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct CycleReport {
    pub level: Level,
    pub include_inferred: bool,
    pub cycles_total: u64,
    pub cyclic_nodes: u64,
    /// Contracts with too many consumer and provider pairs to derive edges from.
    pub skipped_contracts: u64,
    pub items: Vec<Cycle>,
}

/// Why a symbol is a dead-code candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DeadClass {
    /// No usage link reaches it.
    Unreferenced,
    /// Only unreachable code uses it.
    Unreachable,
    /// Reached only through inferred or fan-out links.
    WeaklyReached,
}

/// How sure a candidate is. High is greater than medium, which is greater than low.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DeadConfidence {
    High,
    Medium,
    Low,
}

impl DeadConfidence {
    fn strength(self) -> u8 {
        match self {
            DeadConfidence::High => 2,
            DeadConfidence::Medium => 1,
            DeadConfidence::Low => 0,
        }
    }
}

impl PartialOrd for DeadConfidence {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DeadConfidence {
    fn cmp(&self, other: &Self) -> Ordering {
        self.strength().cmp(&other.strength())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DeadCandidate {
    pub symbol: SymbolHit,
    pub class: DeadClass,
    pub confidence: DeadConfidence,
    /// Usage links into the symbol from other nodes.
    pub incoming: u64,
    /// Of those, the weak ones.
    pub weak_incoming: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DeadCodeReport {
    /// Entry points counted.
    pub roots: u64,
    pub roots_by_reason: BTreeMap<String, u64>,
    pub candidates_total: u64,
    pub high: u64,
    pub medium: u64,
    pub low: u64,
    pub note: Option<String>,
    pub items: Vec<DeadCandidate>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RationaleEntry {
    pub node: SymbolHit,
    /// `note`, `why`, `rationale` (comment markers), `adr`, `item` or `doc`.
    pub kind: String,
    pub line: u32,
    pub text: String,
    pub target: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RationaleReport {
    pub entries_total: u64,
    /// Hubs that have no rationale (for the hubs scope).
    pub uncovered: Vec<SymbolHit>,
    pub items: Vec<RationaleEntry>,
}

/// A report whose `items` can be trimmed to a token budget by whole items.
pub trait Itemized {
    type Item: Serialize;
    fn items_mut(&mut self) -> &mut Vec<Self::Item>;
}

macro_rules! itemized {
    ($report:ty, $item:ty) => {
        impl Itemized for $report {
            type Item = $item;
            fn items_mut(&mut self) -> &mut Vec<$item> {
                &mut self.items
            }
        }
    };
}

itemized!(HubReport, HubEntry);
itemized!(CommunityReport, Community);
itemized!(CycleReport, Cycle);
itemized!(DeadCodeReport, DeadCandidate);
itemized!(RationaleReport, RationaleEntry);
