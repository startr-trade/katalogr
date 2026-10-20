//! Diagnostic traces (FEAT-065): a bounded, deterministic account of how a query was routed,
//! retrieved, scored and packed, recorded without changing the answer.
//!
//! Each traced query has one body that takes a [`Diagnostic`]; the plain function calls it with
//! the collector off, so the two modes cannot differ. Recording sites sit behind
//! `if let Some(d) = diag.on()`.

use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeMap;

/// Hits (search) or symbols (map) with a component breakdown.
pub const DIAG_TOP: usize = 10;
/// Expanded terms listed per query token.
pub const DIAG_TERMS: usize = 20;
/// Incoming links listed per traced symbol.
pub const DIAG_WEIGHTS: usize = 5;
/// `refs` hits with a weight line.
pub const DIAG_REFS: usize = 10;
/// Pack entries listed.
pub const DIAG_PACK: usize = 20;

/// The collector. [`Diagnostic::off`] records nothing.
#[derive(Debug)]
pub struct Diagnostic {
    on: bool,
    trace: Trace,
}

impl Diagnostic {
    pub fn off() -> Diagnostic {
        Diagnostic {
            on: false,
            trace: Trace::default(),
        }
    }

    pub fn on_new() -> Diagnostic {
        Diagnostic {
            on: true,
            trace: Trace::default(),
        }
    }

    /// `Some` only when recording; the recording sites use it as their guard.
    pub fn on(&mut self) -> Option<&mut Trace> {
        self.on.then_some(&mut self.trace)
    }

    /// The finished trace; `None` when it was off.
    pub fn finish(self) -> Option<Trace> {
        self.on.then_some(self.trace)
    }
}

/// What `--diagnostic` reports.
#[derive(Debug, Clone, Default, PartialEq, Serialize, JsonSchema)]
pub struct Trace {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<RoutingTrace>,
    /// `search`: one per query token, in query order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terms: Vec<TermTrace>,
    /// `search`: the top hits, best first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ranking: Vec<RankTrace>,
    /// `map`: where the rank came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank_source: Option<RankSource>,
    /// `refs` and `map`: link weights.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub weights: Vec<WeightTrace>,
    /// Set by the wrapper that packs the output (`--budget`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packing: Option<PackTrace>,
    /// List name to the number of entries cut by a cap.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub omitted: BTreeMap<String, u64>,
}

impl Trace {
    /// Truncate `list` to `cap` entries and add the number cut to `omitted[name]`.
    pub fn cap<T>(&mut self, list: &mut Vec<T>, cap: usize, name: &str) {
        if list.len() > cap {
            let cut = (list.len() - cap) as u64;
            list.truncate(cap);
            self.add_omitted(name, cut);
        }
    }

    /// Add `n` to `omitted[name]` (nothing when `n` is 0).
    pub fn add_omitted(&mut self, name: &str, n: u64) {
        if n > 0 {
            *self.omitted.entry(name.to_string()).or_default() += n;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct RoutingTrace {
    /// `search`, `refs` or `map`.
    pub command: String,
    /// The user's text (not a path).
    pub input: String,
    /// `tokens`, `name` or `none`.
    pub kind: String,
    /// `search`: the query tokens, deduplicated, in order.
    pub tokens: Vec<String>,
    /// `search`: words of the input that produced no token.
    pub dropped: Vec<String>,
    /// `refs`: `contract`, `canonical`, `plain` or `plain_ci`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Symbols the winning tier matched before alias collapsing.
    pub candidates: u64,
    /// After re-export and impl collapsing.
    pub after_collapse: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TermTrace {
    pub token: String,
    pub exact_postings: u64,
    /// Documents matched by the token or any of its expansions.
    pub matching_docs: u64,
    /// Rounded to 6 decimals.
    pub idf: f64,
    pub prefix: PrefixTrace,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct PrefixTrace {
    /// The token has enough characters to be expanded.
    pub attempted: bool,
    /// Terms the expansion returned.
    pub expanded: u64,
    /// The expansion cap was reached.
    pub cap_hit: bool,
    pub factor: f64,
    /// At most `DIAG_TERMS`, in selection order.
    pub terms: Vec<ExpandedTerm>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ExpandedTerm {
    pub term: String,
    pub postings: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct RankTrace {
    /// 1-based.
    pub position: u64,
    /// Canonical name.
    pub symbol: String,
    /// As printed in the answer.
    pub score: f64,
    /// One per query token that contributed, in query order.
    pub parts: Vec<PartTrace>,
    /// Same score as the next hit, so the name order decided.
    pub tied_with_next: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct PartTrace {
    pub token: String,
    /// `exact` or `prefix`.
    pub via: String,
    /// The indexed term that gave the best value.
    pub term: String,
    /// Field-weighted, length-normalised frequency (6 decimals).
    pub t: f64,
    /// 1.0 for an exact term, the prefix factor for a prefix term.
    pub factor: f64,
    /// `factor * idf * t / (k1 + t)` (6 decimals).
    pub contribution: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct RankSource {
    /// `cached` or `live`.
    pub source: String,
    pub watermark: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank_watermark: Option<u64>,
    pub damping: f64,
    pub iterations: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct WeightTrace {
    /// The symbol the link arrives at.
    pub to: String,
    pub from: String,
    pub kind: String,
    /// `Extracted`, `Inferred` or `Declared`.
    pub confidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fanout: Option<u32>,
    /// The link row's weight (6 decimals).
    pub weight: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PackTrace {
    pub budget: u64,
    pub estimator: String,
    pub considered: u64,
    pub kept: u64,
    pub omitted: u64,
    pub used_tokens: u64,
    /// What the first omitted entry would have cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_omitted_tokens: Option<u64>,
    /// The first `DIAG_PACK` kept entries, plus the first omitted one.
    pub entries: Vec<PackEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PackEntry {
    pub index: u64,
    pub tokens: u64,
    pub kept: bool,
    /// Set by context packs (FEAT-038).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Set by context packs (FEAT-038).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
