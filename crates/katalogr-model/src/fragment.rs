//! Per-file contribution to the graph.

use crate::graph::{ArtifactNode, Confidence, Link, LinkKind, Provenance, Span, SymbolNode};
use crate::name::{ContractName, Id};
use serde::{Deserialize, Serialize};

/// Namespace/key chosen by the extension.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportKey {
    pub namespace: String,
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Export {
    pub key: ExportKey,
    pub target: Id,
}

/// Outgoing reference to resolve against other files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutRef {
    pub from: Id,
    pub kind: LinkKind,
    pub span: Option<Span>,
    /// Ordered: most specific first.
    pub candidates: Vec<ExportKey>,
    /// As seen by the extractor; the stitcher may lower it.
    pub confidence: Confidence,
    /// What produced the reference when it was not the extractor, for example a profile rule
    /// (`profile:<name>#<rule id>`). Absent in shards written before profiles existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// When set, this reference holds a contract binding: the stitcher links each resolved
    /// target `--role--> contract`, or `from` (inferred) while it resolves to nothing (ADR-0022).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binds: Option<RefBinding>,
}

/// The contract binding a reference holds on whatever it resolves to (ADR-0022). The
/// reference's `kind` must equal `role.as_str()`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefBinding {
    pub contract: ContractName,
    pub role: ContractRole,
}

/// How a symbol relates to a contract node.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ContractRole {
    Provides,
    Consumes,
    Declares,
}

impl ContractRole {
    /// The link kind a binding of this role is stored under.
    pub fn as_str(self) -> &'static str {
        match self {
            ContractRole::Provides => "provides",
            ContractRole::Consumes => "consumes",
            ContractRole::Declares => "declares",
        }
    }
}

/// A symbol bound to a contract node. The contract's kind and key are opaque to the core: the
/// extension or profile that produced them normalises them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// A symbol of the same fragment (or the fragment's artifact).
    pub symbol: Id,
    pub contract: ContractName,
    pub role: ContractRole,
    pub span: Option<Span>,
    pub confidence: Confidence,
    /// What produced the binding when it was not the extractor, for example a profile rule
    /// (`profile:<name>#<rule id>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// Everything one file contributes. Holds intra-file links only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fragment {
    pub provenance: Provenance,
    /// Extension id.
    pub extension: String,
    pub artifact: ArtifactNode,
    /// Sorted by canonical name before storing.
    pub symbols: Vec<SymbolNode>,
    /// Both ends inside this file.
    pub links: Vec<Link>,
    pub refs: Vec<OutRef>,
    pub exports: Vec<Export>,
    /// Sorted by (contract canonical, role, symbol) before storing. Absent in shards written
    /// before contracts existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bindings: Vec<Binding>,
}
