//! `callers`: who calls a symbol, transitively.

use crate::refs::link_confidence;
use crate::resolve::hit;
use crate::result::{CallerHit, SymbolHit};
use katalogr_model::LinkKind;
use katalogr_model::{Id, Name};
use katalogr_store::{Reader, StoreError};
use std::collections::BTreeSet;

const CALLS: &str = "calls";
const PROVIDES: &str = "provides";
const CONSUMES: &str = "consumes";

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
            for key in tx.links_in(id)? {
                if key.kind.0 != CALLS || !seen.insert(key.from) {
                    continue;
                }
                let Some(row) = tx.symbol(key.from)? else {
                    continue;
                };
                out.push(CallerHit {
                    confidence: link_confidence(tx, &key)?,
                    caller: hit(&row),
                    depth: d,
                    via: None,
                });
                next.push(key.from);
            }
            // The contracts `id` provides (or `id` itself, when it is a contract), and their consumers.
            let mut contracts = vec![id];
            for (key, _) in tx.links_out(id, Some(&LinkKind(PROVIDES.to_string())))? {
                contracts.push(key.to);
            }
            for cid in contracts {
                let Some(contract) = tx.contract(cid)? else {
                    continue;
                };
                for key in tx.links_in(cid)? {
                    if key.kind.0 != CONSUMES || !seen.insert(key.from) {
                        continue;
                    }
                    let Some(row) = tx.symbol(key.from)? else {
                        continue;
                    };
                    out.push(CallerHit {
                        confidence: link_confidence(tx, &key)?,
                        caller: hit(&row),
                        depth: d,
                        via: Some(contract.name.canonical()),
                    });
                    next.push(key.from);
                }
            }
        }
        frontier = next;
    }
    out.sort_by(|a, b| (a.depth, &a.caller.name).cmp(&(b.depth, &b.caller.name)));
    Ok(out)
}
