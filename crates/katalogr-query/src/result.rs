//! Result types. Their serialised form is the stable JSON contract (`schema/v1`).

use crate::diagnostic::Trace;
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
    /// The contract (`contract:<kind>:<key>`) the call goes through, when it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RationaleLink {
    pub kind: String,
    /// Canonical name, or the hex id when the target is not a known symbol.
    pub target: String,
    pub label: String,
}

/// A commit that introduced the line and the board items its trailers name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CommitHit {
    /// Full commit id.
    pub sha: String,
    pub summary: String,
    /// Canonical names of the items and requirements the commit's trailers name.
    pub items: Vec<String>,
}

/// A rationale marker (`NOTE:`, `WHY:`, `RATIONALE:`) recorded on the symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Note {
    /// `NOTE`, `WHY` or `RATIONALE`.
    pub marker: String,
    pub line: u32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WhyResult {
    pub symbol: SymbolHit,
    pub rationale: Vec<RationaleLink>,
    /// The goals served by the items and requirements reached (kind `serves`).
    pub goals: Vec<RationaleLink>,
    /// The commit that introduced the line, when it carries item trailers.
    pub commits: Vec<CommitHit>,
    /// Why part of the answer is missing, for example that blame was unavailable.
    pub note: Option<String>,
    /// The comment markers recorded on the symbol, in line order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
}

/// A reference that names something no artifact exports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DanglingRef {
    /// The symbol (or artifact) the reference starts at.
    pub from: SymbolHit,
    /// Link kind.
    pub kind: String,
    /// The export keys that were looked for, `namespace:key`, most specific first.
    pub wanted: Vec<String>,
}

/// The structural facts of one field of a type that is declared more than once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FieldShape {
    pub field_type: Option<String>,
    pub required: Option<bool>,
    pub many: Option<bool>,
}

/// One difference between two copies of a field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FieldDiff {
    /// Normalised field path, `a/b` for nested fields.
    pub field: String,
    /// `missing-left`, `missing-right`, `type`, `required` or `many`.
    pub what: String,
    pub left: Option<FieldShape>,
    pub right: Option<FieldShape>,
}

/// Two declarers of one contract and how their fields differ.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct MirrorDrift {
    /// `contract:<kind>:<key>`.
    pub contract: String,
    /// The reference declarer: the first by canonical name.
    pub left: SymbolHit,
    pub right: SymbolHit,
    /// False when either side has no structural fields; then `differences` is empty.
    pub comparable: bool,
    pub differences: Vec<FieldDiff>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct MapEntry {
    pub symbol: SymbolHit,
    pub in_degree: f64,
    /// PageRank over all links, rounded to 6 decimal places.
    pub rank: f64,
}

/// One query term that matched, and the field it matched in (`name`, `doc`, `section`,
/// `contract`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Matched {
    pub term: String,
    pub field: String,
    /// The match is through an indexed term that `term` prefixes, not the term itself.
    pub prefix: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct SearchHit {
    pub symbol: SymbolHit,
    /// BM25 score, rounded to 6 decimal places.
    pub score: f64,
    pub matched: Vec<Matched>,
}

/// Which way a link was followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
pub enum Direction {
    /// The link points from `from` to `to`.
    Forward,
    /// The link points from `to` to `from`.
    Backward,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PathStep {
    pub from: String,
    pub kind: String,
    pub direction: Direction,
    pub to: String,
    #[schemars(with = "ConfidenceSchema")]
    pub confidence: Confidence,
}

/// One link of a symbol, seen from that symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct LinkBrief {
    pub kind: String,
    /// Canonical name of the other end (the hex id when it is not a known node).
    pub other: String,
    #[schemars(with = "ConfidenceSchema")]
    pub confidence: Confidence,
    /// The profile rule (`profile:<name>#<rule id>`) that produced the link, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Explain {
    pub symbol: SymbolHit,
    pub signature: Option<String>,
    pub doc: Option<String>,
    /// At most `per_side` links, sorted by kind then name.
    pub outgoing: Vec<LinkBrief>,
    pub outgoing_total: u64,
    pub incoming: Vec<LinkBrief>,
    pub incoming_total: u64,
    /// Rationale targets (board items, documents, commits), sorted.
    pub items: Vec<String>,
}

/// What `show` prints: the symbol's lines (or, for `--signature`, its signature and doc).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ShowResult {
    pub symbol: SymbolHit,
    /// Repository-relative path.
    pub file: String,
    /// First and last line of the span.
    pub lines: (u32, u32),
    pub text: String,
}

/// What `replace` did: the symbol and the line range it now occupies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplaceResult {
    pub symbol: SymbolHit,
    pub file: String,
    /// First and last line of the replaced span, before the change.
    pub replaced: (u32, u32),
    /// First and last line of the new text.
    pub lines: (u32, u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Truncation {
    pub shown: u64,
    pub omitted: u64,
    pub budget: u64,
}

/// Names of caps, used in [`Partial::cap`].
pub mod cap {
    /// `--limit`, or a report's limit.
    pub const LIMIT: &str = "limit";
    /// `callers --depth`.
    pub const DEPTH: &str = "depth";
    /// `explain --per-side`.
    pub const PER_SIDE: &str = "per-side";
    /// `--budget` (tokens).
    pub const BUDGET: &str = "budget";
    /// Members listed per community or cycle.
    pub const MEMBERS: &str = "members";
}

/// Said with every partial answer.
pub const PARTIAL_NOTE: &str = "absence is not proof";

/// The answer was cut by a cap: more items exist (or may exist) than are shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Partial {
    /// Which cap: one of [`cap`]'s names.
    pub cap: String,
    /// The cap's value: N in "capped at N" (items, hops, or tokens for `budget`).
    pub at: u64,
    /// Items shown.
    pub shown: u64,
    /// Items there were before the cap, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// Always [`PARTIAL_NOTE`].
    pub note: String,
}

impl Partial {
    pub fn new(cap: &str, at: u64, shown: u64, total: Option<u64>) -> Partial {
        Partial {
            cap: cap.to_string(),
            at,
            shown,
            total,
            note: PARTIAL_NOTE.to_string(),
        }
    }

    /// Text form: `partial: capped at 20 (limit; 57 found); absence is not proof`, or without
    /// `; N found` when `total` is None. For `budget`: `capped at 400 tokens (budget; ...)`.
    pub fn line(&self) -> String {
        let unit = if self.cap == cap::BUDGET {
            " tokens"
        } else {
            ""
        };
        let found = self
            .total
            .map(|t| format!("; {t} found"))
            .unwrap_or_default();
        format!(
            "partial: capped at {}{unit} ({}{found}); {}",
            self.at, self.cap, self.note
        )
    }
}

/// What `--json` prints.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Envelope<T> {
    pub schema: u32,
    pub watermark: u64,
    pub pending: u64,
    pub stale: bool,
    pub truncated: Option<Truncation>,
    /// Present when a cap or budget cut the answer; one entry per cap that cut it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partial: Vec<Partial>,
    pub result: T,
    /// Suggested follow-up commands, at most three.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next: Vec<String>,
    /// The trace of the query, present only with `--diagnostic` (FEAT-065).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<Trace>,
}

#[cfg(test)]
#[test]
fn partial_line_format() {
    let p = Partial::new(cap::LIMIT, 20, 20, Some(57));
    assert_eq!(
        p.line(),
        "partial: capped at 20 (limit; 57 found); absence is not proof"
    );
    let p = Partial::new(cap::DEPTH, 2, 5, None);
    assert_eq!(
        p.line(),
        "partial: capped at 2 (depth); absence is not proof"
    );
    let p = Partial::new(cap::BUDGET, 400, 7, Some(12));
    assert_eq!(
        p.line(),
        "partial: capped at 400 tokens (budget; 12 found); absence is not proof"
    );
}
