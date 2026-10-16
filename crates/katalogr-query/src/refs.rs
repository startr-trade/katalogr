//! `refs`: everything that points at a symbol, including through re-exports.

use crate::resolve::hit;
use crate::result::{RefHit, SymbolHit};
use katalogr_model::{Confidence, Id};
use katalogr_store::{LinkKey, Reader, StoreError};
use std::collections::{BTreeSet, VecDeque};

const REEXPORT: &str = "reexports";

pub(crate) fn link_confidence(tx: &impl Reader, key: &LinkKey) -> Result<Confidence, StoreError> {
    tx.link(key)?
        .and_then(|r| r.entries.first().map(|e| e.confidence))
        .ok_or_else(|| StoreError::Row("link without entries".into()))
}

/// Links arriving at `target` (any kind), plus those arriving at anything that re-exports it,
/// transitively. Each hit names the re-export chain it came through. Cycle-safe. Sorted by
/// chain length, then referrer name, then link kind.
pub fn refs(tx: &impl Reader, target: &SymbolHit) -> Result<Vec<RefHit>, StoreError> {
    let root = Id::of(&target.name);
    let mut seen = BTreeSet::from([root]);
    let mut queue = VecDeque::from([(root, Vec::<String>::new())]);
    let mut out = Vec::new();
    while let Some((id, via)) = queue.pop_front() {
        for key in tx.links_in(id)? {
            let Some(row) = tx.symbol(key.from)? else {
                continue;
            };
            let from = hit(&row);
            if key.kind.0 == REEXPORT && seen.insert(key.from) {
                let mut next = via.clone();
                next.push(from.name.clone());
                queue.push_back((key.from, next));
            }
            out.push(RefHit {
                confidence: link_confidence(tx, &key)?,
                kind: key.kind.0,
                from,
                via: via.clone(),
            });
        }
    }
    out.sort_by(|a, b| {
        (a.via.len(), &a.from.name, &a.kind).cmp(&(b.via.len(), &b.from.name, &b.kind))
    });
    Ok(out)
}
