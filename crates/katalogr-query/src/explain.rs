//! `explain`: one symbol, compactly: signature, doc, its links and the rationale behind it.

use crate::refs::link_confidence;
use crate::resolve::hit;
use crate::result::{Explain, LinkBrief, SymbolHit};
use crate::show::fact_text;
use crate::why::RATIONALE_KINDS;
use katalogr_model::{Id, Name};
use katalogr_store::{LinkKey, Reader, StoreError};

fn node_name(tx: &impl Reader, id: Id) -> Result<String, StoreError> {
    if let Some(s) = tx.symbol(id)? {
        return Ok(s.name.canonical());
    }
    if let Some(a) = tx.artifact(id)? {
        return Ok(a.name.canonical());
    }
    if let Some(c) = tx.contract(id)? {
        return Ok(c.name.canonical());
    }
    Ok(id.to_hex())
}

/// The symbol with its signature and doc, at most `per_side` outgoing and `per_side` incoming
/// links (sorted by kind, then the other end's name; the totals say how many there are), and
/// the rationale targets it links to.
pub fn explain(tx: &impl Reader, s: &SymbolHit, per_side: usize) -> Result<Explain, StoreError> {
    let id = Id::of(&s.name);
    let row = tx
        .symbol(id)?
        .ok_or_else(|| StoreError::Row(format!("unknown symbol {}", s.name)))?;

    let mut outgoing: Vec<LinkBrief> = Vec::new();
    let mut items: Vec<String> = Vec::new();
    for (key, _) in tx.links_out(id, None)? {
        let other = node_name(tx, key.to)?;
        if RATIONALE_KINDS.contains(&key.kind.0.as_str()) {
            items.push(other.clone());
        }
        outgoing.push(LinkBrief {
            confidence: link_confidence(tx, &key)?,
            kind: key.kind.0,
            other,
        });
    }
    let mut incoming: Vec<LinkBrief> = Vec::new();
    for key in tx.links_in(id)? {
        let other = node_name(tx, key.from)?;
        incoming.push(LinkBrief {
            confidence: link_confidence(
                tx,
                &LinkKey {
                    from: key.from,
                    kind: key.kind.clone(),
                    to: key.to,
                },
            )?,
            kind: key.kind.0,
            other,
        });
    }
    let order = |a: &LinkBrief, b: &LinkBrief| (&a.kind, &a.other).cmp(&(&b.kind, &b.other));
    outgoing.sort_by(order);
    incoming.sort_by(order);
    items.sort();
    items.dedup();
    let (outgoing_total, incoming_total) = (outgoing.len() as u64, incoming.len() as u64);
    outgoing.truncate(per_side);
    incoming.truncate(per_side);
    Ok(Explain {
        symbol: hit(&row),
        signature: fact_text(&row, "signature"),
        doc: fact_text(&row, "doc"),
        outgoing,
        outgoing_total,
        incoming,
        incoming_total,
        items,
    })
}
