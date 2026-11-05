//! The graph a view hands to the renderers: plain data, sorted by the producer and again by
//! each renderer, so output never depends on the order it was built in.

use schemars::JsonSchema;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// One view: nodes and edges plus how much of the whole they show.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ViewGraph {
    /// The query line that produced the view, for example `structure --level package`.
    pub title: String,
    /// Sorted by id.
    pub nodes: Vec<ViewNode>,
    /// Sorted by from, to, kind.
    pub edges: Vec<ViewEdge>,
    pub shown: u64,
    /// More than `shown` means the view is partial.
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ViewNode {
    /// From [`node_id`] of the canonical name.
    pub id: String,
    /// Canonical name.
    pub name: String,
    /// Text to draw; when empty the renderers shorten `name`.
    pub label: String,
    pub kind: String,
    pub path: Option<String>,
    /// The repository, when the view spans several.
    pub group: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ViewEdge {
    /// A node id.
    pub from: String,
    /// A node id.
    pub to: String,
    pub kind: String,
    /// Drawn dashed.
    pub inferred: bool,
}

/// `n` plus the first 12 hex characters of the SHA-256 of the canonical name. It starts with a
/// letter and holds only hex digits, so it is a valid id in every output format and never a
/// keyword.
pub fn node_id(canonical: &str) -> String {
    let digest = Sha256::digest(canonical.as_bytes());
    let mut id = String::with_capacity(13);
    id.push('n');
    for b in &digest[..6] {
        id.push_str(&format!("{b:02x}"));
    }
    id
}
