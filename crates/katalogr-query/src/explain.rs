//! `explain`: one symbol, compactly: signature, doc, its links and the rationale behind it.

use crate::refs::{link_confidence, link_origin};
use crate::resolve::hit;
use crate::result::{ContractEnd, ContractInfo, Explain, LinkBrief, SymbolHit};
use crate::show::fact_text;
use crate::why::RATIONALE_KINDS;
use katalogr_model::{Confidence, Id, Name, Span};
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

/// The three roles a contract's bindings can have, in output order.
const ROLES: [&str; 3] = ["provides", "consumes", "declares"];

/// The contract node `id`: its kind and key, and the ends bound to it by role (each role sorted
/// by name and cut to `per_side`; the totals say how many there are).
fn explain_contract(
    tx: &impl Reader,
    s: &SymbolHit,
    id: Id,
    kind: &str,
    key: &str,
    per_side: usize,
) -> Result<Explain, StoreError> {
    let mut roles: [Vec<ContractEnd>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut incoming: Vec<LinkBrief> = Vec::new();
    for lk in tx.links_in(id)? {
        let Some(role) = ROLES.iter().position(|r| *r == lk.kind.0) else {
            continue;
        };
        let (name, end_kind, path, own_span): (String, String, String, Span) =
            if let Some(row) = tx.symbol(lk.from)? {
                let h = hit(&row);
                (h.name, h.kind, h.path, h.span)
            } else if let Some(a) = tx.artifact(lk.from)? {
                let span = Span {
                    start_line: 1,
                    start_col: 1,
                    end_line: 1,
                    end_col: 1,
                };
                (
                    a.name.canonical(),
                    "artifact".to_string(),
                    a.name.path.as_str().to_string(),
                    span,
                )
            } else {
                continue;
            };
        let row = tx.link(&lk)?;
        let confidence: Confidence = link_confidence(tx, &lk)?;
        let span = row
            .as_ref()
            .and_then(|r| r.entries.first().and_then(|e| e.span))
            .unwrap_or(own_span);
        let origin = link_origin(tx, &lk)?;
        incoming.push(LinkBrief {
            kind: lk.kind.0.clone(),
            other: name.clone(),
            confidence,
            origin: origin.clone(),
        });
        roles[role].push(ContractEnd {
            name,
            kind: end_kind,
            path,
            span,
            confidence,
            origin,
        });
    }
    let totals = [
        roles[0].len() as u64,
        roles[1].len() as u64,
        roles[2].len() as u64,
    ];
    for ends in &mut roles {
        ends.sort_by(|a, b| a.name.cmp(&b.name));
        ends.truncate(per_side);
    }
    incoming.sort_by(|a, b| (&a.kind, &a.other).cmp(&(&b.kind, &b.other)));
    let incoming_total = incoming.len() as u64;
    incoming.truncate(per_side);
    let [providers, consumers, declarers] = roles;
    Ok(Explain {
        symbol: s.clone(),
        signature: None,
        doc: None,
        outgoing: Vec::new(),
        outgoing_total: 0,
        incoming,
        incoming_total,
        items: Vec::new(),
        contract: Some(ContractInfo {
            kind: kind.to_string(),
            key: key.to_string(),
            providers,
            providers_total: totals[0],
            consumers,
            consumers_total: totals[1],
            declarers,
            declarers_total: totals[2],
        }),
    })
}

/// The symbol with its signature and doc, at most `per_side` outgoing and `per_side` incoming
/// links (sorted by kind, then the other end's name; the totals say how many there are), and
/// the rationale targets it links to.
pub fn explain(tx: &impl Reader, s: &SymbolHit, per_side: usize) -> Result<Explain, StoreError> {
    let id = Id::of(&s.name);
    let Some(row) = tx.symbol(id)? else {
        return match tx.contract(id)? {
            Some(c) => explain_contract(tx, s, id, &c.name.kind, &c.name.key, per_side),
            None => Err(StoreError::Row(format!("unknown symbol {}", s.name))),
        };
    };

    let mut outgoing: Vec<LinkBrief> = Vec::new();
    let mut items: Vec<String> = Vec::new();
    for (key, _) in tx.links_out(id, None)? {
        let other = node_name(tx, key.to)?;
        if RATIONALE_KINDS.contains(&key.kind.0.as_str()) {
            items.push(other.clone());
        }
        outgoing.push(LinkBrief {
            confidence: link_confidence(tx, &key)?,
            origin: link_origin(tx, &key)?,
            kind: key.kind.0,
            other,
        });
    }
    let mut incoming: Vec<LinkBrief> = Vec::new();
    for key in tx.links_in(id)? {
        let other = node_name(tx, key.from)?;
        let full = LinkKey {
            from: key.from,
            kind: key.kind.clone(),
            to: key.to,
        };
        incoming.push(LinkBrief {
            confidence: link_confidence(tx, &full)?,
            origin: link_origin(tx, &full)?,
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
        contract: None,
    })
}
