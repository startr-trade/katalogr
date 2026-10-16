//! Result types. Their serialised form is the stable JSON contract (`schema/v1`).

use katalogr_model::{Confidence, Span};
use schemars::JsonSchema;
use serde::Serialize;

/// Outcome of resolving a piece of text to one symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution<T> {
    One(T),
    Ambiguous(Vec<SymbolHit>),
    None,
}

/// Schema-only mirror of [`katalogr_model::Span`].
#[derive(JsonSchema)]
#[schemars(rename = "Span")]
#[allow(dead_code)]
struct SpanSchema {
    start_line: u32,
    start_col: u32,
    end_line: u32,
    end_col: u32,
}

/// Schema-only mirror of [`katalogr_model::Confidence`].
#[derive(JsonSchema)]
#[schemars(rename = "Confidence")]
#[allow(dead_code)]
enum ConfidenceSchema {
    Extracted,
    Inferred,
    Declared,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SymbolHit {
    /// Canonical name.
    pub name: String,
    pub kind: String,
    /// Repository-relative path.
    pub path: String,
    #[schemars(with = "SpanSchema")]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RefHit {
    pub from: SymbolHit,
    /// Link kind.
    pub kind: String,
    #[schemars(with = "ConfidenceSchema")]
    pub confidence: Confidence,
    /// Re-export chain, nearest the target first; empty for a direct reference.
    pub via: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CallerHit {
    pub caller: SymbolHit,
    pub depth: u32,
    #[schemars(with = "ConfidenceSchema")]
    pub confidence: Confidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RationaleLink {
    pub kind: String,
    /// Canonical name, or the hex id when the target is not a known symbol.
    pub target: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WhyResult {
    pub symbol: SymbolHit,
    pub rationale: Vec<RationaleLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct MapEntry {
    pub symbol: SymbolHit,
    pub in_degree: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Truncation {
    pub shown: u64,
    pub omitted: u64,
    pub budget: u64,
}

/// What `--json` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Envelope<T> {
    pub schema: u32,
    pub watermark: u64,
    pub pending: u64,
    pub stale: bool,
    pub truncated: Option<Truncation>,
    pub result: T,
}
