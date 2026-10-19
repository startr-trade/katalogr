//! Options of the five insights, and the constants that shape the insight graph.

use crate::result::{DeadConfidence, Level};
use katalogr_model::Id;
use std::collections::{BTreeMap, BTreeSet};

/// Link kinds that are not usage: never edges of the insight graph.
pub const STRUCTURAL_KINDS: &[&str] = &[
    "contains",
    "declares",
    "provides",
    "consumes",
    "annotated-by",
    "refs-item",
    "documented-by",
    "changed-by",
    "links-to",
    "specifies",
    "affects",
    "serves",
    "touches",
];

/// Default seed of the community search; any fixed value gives reproducible output.
pub const DEFAULT_SEED: u64 = 0x2545_F491_4F6C_DD1D;
/// Most consumer-provider pairs of one contract that become derived edges.
pub const CONTRACT_EDGE_CAP: usize = 10_000;
/// Kind of an edge derived from a contract.
pub const CONTRACT_EDGE_KIND: &str = "via-contract";

/// How the insight graph is built from the links.
#[derive(Clone, Debug)]
pub struct GraphOptions {
    pub level: Level,
    /// Keep weak edges (inferred, or fan-out above one).
    pub include_inferred: bool,
    /// Link kinds dropped; default [`STRUCTURAL_KINDS`].
    pub exclude_kinds: Vec<String>,
}

impl GraphOptions {
    pub fn new(level: Level, include_inferred: bool) -> Self {
        GraphOptions {
            level,
            include_inferred,
            exclude_kinds: STRUCTURAL_KINDS.iter().map(|k| k.to_string()).collect(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct HubOptions {
    pub graph: GraphOptions,
    /// Smallest weighted degree reported.
    pub min_degree: f64,
    pub limit: usize,
}

#[derive(Clone, Debug)]
pub struct CommunityOptions {
    pub graph: GraphOptions,
    pub seed: u64,
    pub resolution: f64,
    /// Smaller communities are not reported.
    pub min_size: usize,
    /// Members listed per community.
    pub members: usize,
    pub limit: usize,
    /// Canonical name of a node (at `graph.level`): report only its community.
    pub of: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CycleOptions {
    pub graph: GraphOptions,
    /// Members listed per cycle.
    pub members: usize,
    pub limit: usize,
}

#[derive(Clone, Debug)]
pub struct DeadCodeOptions {
    /// Extension id to its code kinds; built by the composition root from the registry.
    pub code_kinds: BTreeMap<String, BTreeSet<String>>,
    /// Link kinds that are not usage; default [`STRUCTURAL_KINDS`].
    pub exclude_kinds: Vec<String>,
    /// Do not count `api` roots.
    pub internal: bool,
    pub min_confidence: DeadConfidence,
    /// Repository-relative path prefix filter on candidates (all repositories).
    pub path_prefix: Option<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RationaleScope {
    All,
    /// A symbol and every symbol it transitively contains.
    Symbol(Id),
    /// Artifacts whose repository-relative path starts with the prefix.
    Path(String),
    /// The first n hubs (symbol level, defaults of `hubs`).
    Hubs(usize),
}
