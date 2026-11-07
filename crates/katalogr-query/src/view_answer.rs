//! The wire type of a generated view (FEAT-088): what `katalogr view` prints and
//! `GET /api/v1/views/<op>` answers. It holds no logic.
//!
//! The graph is a `serde_json::Value`, so this crate does not depend on the renderers.

use crate::position::PositionRef;
use crate::result::PeerState;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The question a view answers, with every default filled in. `op` is the view's op (for
/// example `structure`), not the op of its view ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ViewQueryJson {
    pub op: String,
    pub args: BTreeMap<String, String>,
}

/// A view at a position: Mermaid and the graph it was drawn from. Fields serialise in the
/// order declared; the `None` fields are left out.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ViewAnswer {
    /// `RESULT_SCHEMA_VERSION`.
    pub schema: u32,
    /// The view-ID format (chain decision CD-3).
    pub view_format: u32,
    /// Only when exact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view_id: Option<String>,
    pub exact: bool,
    /// What the answer reflects, one entry per repo sorted by repo.
    pub positions: Vec<PositionRef>,
    /// The op and arguments, with defaults filled.
    pub query: ViewQueryJson,
    pub mermaid: String,
    /// The view graph of the renderers.
    pub graph: serde_json::Value,
    /// The Pele version the Mermaid was checked with.
    pub pele: String,
    /// Only when not exact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watermark: Option<u64>,
    /// Only when not exact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<u64>,
    /// Only when not exact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
    /// The peers the view read and the positions it reflects of each (FEAT-020). Left out when the
    /// view read none, so a view of a project with no peers keeps its exact bytes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peers: Vec<PeerState>,
}

/// The JSON Schema of [`ViewAnswer`] (`schema/v1/view.schema.json`). It is not an envelope, so
/// it is not among `schemas`.
pub fn view_schema() -> serde_json::Value {
    schemars::schema_for!(ViewAnswer).to_value()
}
