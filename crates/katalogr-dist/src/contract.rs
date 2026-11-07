//! What this binary serves to a client that links to it by contract (FEAT-049, CD-5):
//! `katalogr contract --json` and `GET /api/v1/meta` print the same [`ContractInfo`].

use katalogr_query::RESULT_SCHEMA_VERSION;
use schemars::JsonSchema;
use serde::Serialize;

/// The path major: `/api/v1`.
pub const API_VERSION: u32 = 1;
/// Raised by every additive change to the contract.
pub const CONTRACT_VERSION: u32 = 8;
/// The position format (FEAT-043 CD-2).
pub const POSITION_FORMAT: u32 = 1;
/// The view-id format (FEAT-043 CD-3).
pub const VIEW_FORMAT: u32 = 1;
/// The capabilities this binary has, sorted. Later items add names here and raise
/// [`CONTRACT_VERSION`].
pub const CAPABILITIES: &[&str] = &[
    "deps",
    "docs-affected",
    "impact",
    "position",
    "timeline",
    "type-lineage",
    "view-id",
    "views",
    "workspace",
];

/// Reported by a process that serves pages. Never in [`CAPABILITIES`].
pub const WEB_CAPABILITY: &str = "web-view";

/// What this binary serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ContractInfo {
    /// The path major of the HTTP API.
    pub api: u32,
    /// Raised by every additive change.
    pub contract: u32,
    /// The `--json` result schema version.
    pub schema: u32,
    /// The position format version.
    pub position_format: u32,
    /// The view-id format version.
    pub view_format: u32,
    /// The crate version.
    pub katalogr: String,
    /// The capability names, sorted.
    pub capabilities: Vec<String>,
}

/// The contract this binary serves.
pub fn contract_info() -> ContractInfo {
    let mut capabilities: Vec<String> = CAPABILITIES.iter().map(|c| c.to_string()).collect();
    capabilities.sort();
    ContractInfo {
        api: API_VERSION,
        contract: CONTRACT_VERSION,
        schema: RESULT_SCHEMA_VERSION,
        position_format: POSITION_FORMAT,
        view_format: VIEW_FORMAT,
        katalogr: env!("CARGO_PKG_VERSION").to_string(),
        capabilities,
    }
}

/// The JSON Schema of [`ContractInfo`].
pub fn schema() -> serde_json::Value {
    schemars::schema_for!(ContractInfo).to_value()
}

/// R-2.
#[cfg(test)]
#[test]
fn info_lists_capabilities_sorted() {
    let i = contract_info();
    let mut sorted = i.capabilities.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(i.capabilities, sorted);
    assert!(i.capabilities.iter().any(|c| c == "docs-affected"));
    assert!(i.capabilities.iter().any(|c| c == "position"));
    assert!(i.capabilities.iter().any(|c| c == "view-id"));
    assert_eq!(i.api, 1);
    assert!(i.capabilities.iter().any(|c| c == "views"));
    assert_eq!(i.contract, 8);
    assert!(i.capabilities.iter().any(|c| c == "workspace"));
    assert!(i.capabilities.iter().any(|c| c == "deps"));
    assert!(i.capabilities.iter().any(|c| c == "impact"));
    assert!(i.capabilities.iter().any(|c| c == "type-lineage"));
    assert_eq!(i.schema, RESULT_SCHEMA_VERSION);
}
