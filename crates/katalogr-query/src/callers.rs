//! `callers`: who calls a symbol, transitively.

use crate::refs::link_confidence;
use crate::resolve::hit;
use crate::result::{CallerHit, SymbolHit};
use crate::step::incoming;
use katalogr_model::Id;
use katalogr_store::{Reader, StoreError};
use std::collections::BTreeSet;

/// Breadth-first over incoming `calls` links, up to `depth` (at least 1). A provider of a
/// contract is also called by the contract's consumers (`via` names the contract), and so is
/// the contract itself when it is the target. Each caller appears once, at its smallest depth.
/// Sorted by depth, then name.
pub fn callers(
    tx: &impl Reader,
    target: &SymbolHit,
    depth: u32,
) -> Result<Vec<CallerHit>, StoreError> {
    let root = Id::of(&target.name);
    let mut seen = BTreeSet::from([root]);
    let mut frontier = vec![root];
    let mut out = Vec::new();
    for d in 1..=depth.max(1) {
        let mut next = Vec::new();
        for id in frontier {
            for step in incoming(tx, id)? {
                if !seen.insert(step.from) {
                    continue;
                }
                let Some(row) = tx.symbol(step.from)? else {
                    continue;
                };
                out.push(CallerHit {
                    confidence: link_confidence(tx, &step.key)?,
                    caller: hit(&row),
                    depth: d,
                    via: step.via.map(|(name, _)| name),
                });
                next.push(step.from);
            }
        }
        frontier = next;
    }
    out.sort_by(|a, b| (a.depth, &a.caller.name).cmp(&(b.depth, &b.caller.name)));
    Ok(out)
}
