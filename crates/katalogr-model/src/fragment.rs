//! Per-file contribution to the graph.

use crate::graph::{ArtifactNode, Confidence, Link, LinkKind, Provenance, Span, SymbolNode};
use crate::name::Id;
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
}
