//! `show`: what the index knows about where a symbol is, for reading its span.

use crate::result::SymbolHit;
use katalogr_model::{Fact, Id};
use katalogr_store::{Reader, StoreError, SymbolRow};

/// A symbol and the index facts needed to read or replace its span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub symbol: SymbolHit,
    pub repo: String,
    /// The revision the file was indexed at: a blob id, then `+<hex>` when the extension had
    /// a project context.
    pub revision: String,
    pub signature: Option<String>,
    pub doc: Option<String>,
}

pub(crate) fn fact_text(row: &SymbolRow, key: &str) -> Option<String> {
    match row.facts.get(key) {
        Some(Fact::Str(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// The index record of `symbol`. `None` when the symbol or its file is no longer indexed.
pub fn target(tx: &impl Reader, symbol: &SymbolHit) -> Result<Option<Target>, StoreError> {
    let Some(row) = tx.symbol(Id::of(&symbol.name))? else {
        return Ok(None);
    };
    let Some(artifact) = tx.artifact(row.artifact)? else {
        return Ok(None);
    };
    Ok(Some(Target {
        symbol: symbol.clone(),
        repo: row.name.artifact.repo.clone(),
        revision: artifact.provenance.revision,
        signature: fact_text(&row, "signature"),
        doc: fact_text(&row, "doc"),
    }))
}

/// The blob id part of a revision.
pub fn blob_of(revision: &str) -> &str {
    revision.split('+').next().unwrap_or(revision)
}
