//! Graph data types: spans, links, nodes, facts.

use crate::name::{ArtifactName, Id, SymbolName};
use crate::relpath::RelPath;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 1-based line/column range, end inclusive.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Span {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

/// Where a datum came from. `revision` is a git blob id, followed by `+<hex>` when the
/// extension has a project context.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub repo: String,
    pub path: RelPath,
    pub revision: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Confidence {
    Extracted,
    Inferred,
    Declared,
}

/// Core kinds: contains, references, calls, imports, implements, binds; extensions add more.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LinkKind(pub String);

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LinkTarget {
    Id(Id),
    Unresolved,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub from: Id,
    pub to: Id,
    pub kind: LinkKind,
    pub confidence: Confidence,
    pub span: Option<Span>,
    pub provenance: Provenance,
}

/// Deterministic value tree: no floats.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Fact {
    Str(String),
    Int(i64),
    Bool(bool),
    List(Vec<Fact>),
    Map(BTreeMap<String, Fact>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactNode {
    pub name: ArtifactName,
    pub facts: BTreeMap<String, Fact>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolNode {
    pub name: SymbolName,
    pub kind: String,
    pub span: Span,
    pub facts: BTreeMap<String, Fact>,
}
