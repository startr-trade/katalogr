//! `refs`: everything that points at a symbol, including through re-exports.

use crate::diagnostic::{DIAG_REFS, Diagnostic, WeightTrace};
use crate::resolve::hit;
use crate::result::{RefHit, SymbolHit};
use crate::search::round6;
use katalogr_model::{Confidence, Id};
use katalogr_store::{LinkKey, LinkRow, Reader, StoreError, link_weight, row_weight};
use std::collections::{BTreeSet, VecDeque};

const REEXPORT: &str = "reexports";

pub(crate) fn link_confidence(tx: &impl Reader, key: &LinkKey) -> Result<Confidence, StoreError> {
    tx.link(key)?
        .and_then(|r| r.entries.first().map(|e| e.confidence))
        .ok_or_else(|| StoreError::Row("link without entries".into()))
}

/// The rule that produced a link (`profile:<name>#<id>`), when one did.
pub(crate) fn link_origin(tx: &impl Reader, key: &LinkKey) -> Result<Option<String>, StoreError> {
    Ok(tx
        .link(key)?
        .and_then(|r| r.entries.into_iter().find_map(|e| e.produced_by)))
}

/// Links arriving at `target` (any kind), plus those arriving at anything that re-exports it,
/// transitively. Each hit names the re-export chain it came through. Cycle-safe. Sorted by
/// chain length, then referrer name, then link kind.
pub fn refs(tx: &impl Reader, target: &SymbolHit) -> Result<Vec<RefHit>, StoreError> {
    refs_traced(tx, target, &mut Diagnostic::off())
}

/// The weight line of the link `key`: the entry that gives its row weight.
pub(crate) fn weight_trace(
    to: &str,
    from: &str,
    key: &LinkKey,
    row: &LinkRow,
) -> Option<WeightTrace> {
    let mut best: Option<(&katalogr_store::LinkEntry, f64)> = None;
    for e in &row.entries {
        let w = link_weight(e);
        if best.is_none_or(|(_, b)| w > b) {
            best = Some((e, w));
        }
    }
    let (entry, _) = best?;
    Some(WeightTrace {
        to: to.to_string(),
        from: from.to_string(),
        kind: key.kind.0.clone(),
        confidence: format!("{:?}", entry.confidence),
        fanout: entry.fanout,
        weight: round6(row_weight(row)),
    })
}

/// What [`refs`] does, recording the link weights of the leading hits into `diag`. The answer
/// does not depend on `diag`.
pub fn refs_traced(
    tx: &impl Reader,
    target: &SymbolHit,
    diag: &mut Diagnostic,
) -> Result<Vec<RefHit>, StoreError> {
    let root = Id::of(&target.name);
    let mut seen = BTreeSet::from([root]);
    let mut queue = VecDeque::from([(root, Vec::<String>::new())]);
    let mut out: Vec<(RefHit, Id)> = Vec::new();
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
            out.push((
                RefHit {
                    confidence: link_confidence(tx, &key)?,
                    kind: key.kind.0,
                    from,
                    via: via.clone(),
                },
                id,
            ));
        }
    }
    out.sort_by(|a, b| {
        (a.0.via.len(), &a.0.from.name, &a.0.kind).cmp(&(b.0.via.len(), &b.0.from.name, &b.0.kind))
    });
    if let Some(d) = diag.on() {
        for (h, to) in out.iter().take(DIAG_REFS) {
            let key = LinkKey {
                from: Id::of(&h.from.name),
                kind: katalogr_model::LinkKind(h.kind.clone()),
                to: *to,
            };
            let to_name = match tx.symbol(*to)? {
                Some(row) => hit(&row).name,
                None => target.name.clone(),
            };
            if let Some(row) = tx.link(&key)?
                && let Some(w) = weight_trace(&to_name, &h.from.name, &key, &row)
            {
                d.weights.push(w);
            }
        }
        d.add_omitted("weights", out.len().saturating_sub(DIAG_REFS) as u64);
    }
    Ok(out.into_iter().map(|(h, _)| h).collect())
}
