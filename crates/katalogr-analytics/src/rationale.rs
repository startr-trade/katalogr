//! Rationale: the comment markers, ADR citations, board-item references and documents that
//! explain a symbol, a path or the top hubs (FEAT-030 section 6.7).

use crate::hubs::hubs;
use crate::options::{GraphOptions, HubOptions, RationaleScope};
use crate::result::{Level, RationaleEntry, RationaleReport};
use katalogr_model::annotate::{ADR_NS, ITEM_NS, REQ_NS};
use katalogr_model::insight::FACT_RATIONALE;
use katalogr_model::{Fact, Id, LinkKind, Name, Span};
use katalogr_query::RATIONALE_KINDS;
use katalogr_query::result::SymbolHit;
use katalogr_store::{Reader, StoreError};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// A node in scope: a symbol or an artifact.
struct Node {
    hit: SymbolHit,
    facts: BTreeMap<String, Fact>,
    artifact: Id,
}

fn line_span(line: u32) -> Span {
    Span {
        start_line: line,
        start_col: 1,
        end_line: line,
        end_col: 1,
    }
}

/// The entries of the nodes in `scope`, in file and line order, at most `limit`. For the hubs
/// scope, `uncovered` names the hubs without any entry.
pub fn rationale(
    tx: &impl Reader,
    scope: &RationaleScope,
    limit: usize,
) -> Result<RationaleReport, StoreError> {
    let symbols = tx.symbols()?;
    let artifacts = tx.artifacts()?;

    // Every node by id.
    let mut nodes: HashMap<Id, Node> = HashMap::with_capacity(symbols.len() + artifacts.len());
    for (id, row) in &symbols {
        nodes.insert(
            *id,
            Node {
                hit: SymbolHit {
                    name: row.name.canonical(),
                    kind: row.kind.clone(),
                    path: row.name.artifact.path.as_str().to_string(),
                    span: row.span,
                },
                facts: row.facts.clone(),
                artifact: row.artifact,
            },
        );
    }
    for (id, row) in &artifacts {
        nodes.insert(
            *id,
            Node {
                hit: SymbolHit {
                    name: row.name.canonical(),
                    kind: "artifact".to_string(),
                    path: row.name.path.as_str().to_string(),
                    span: line_span(1),
                },
                facts: row.facts.clone(),
                artifact: *id,
            },
        );
    }

    // The scope.
    let mut hub_order: Vec<Id> = Vec::new();
    let in_scope: HashSet<Id> = match scope {
        RationaleScope::All => nodes.keys().copied().collect(),
        RationaleScope::Path(prefix) => nodes
            .iter()
            .filter(|(_, n)| n.hit.path.starts_with(prefix.as_str()))
            .map(|(id, _)| *id)
            .collect(),
        RationaleScope::Symbol(root) => {
            let contains = LinkKind("contains".to_string());
            let mut seen: HashSet<Id> = HashSet::from([*root]);
            let mut stack = vec![*root];
            while let Some(v) = stack.pop() {
                for (key, _) in tx.links_out(v, Some(&contains))? {
                    if seen.insert(key.to) {
                        stack.push(key.to);
                    }
                }
            }
            seen
        }
        RationaleScope::Hubs(n) => {
            let report = hubs(
                tx,
                &HubOptions {
                    graph: GraphOptions::new(Level::Symbol, false),
                    min_degree: 0.0,
                    limit: *n,
                },
            )?;
            let by_name: HashMap<&str, Id> = nodes
                .iter()
                .map(|(id, n)| (n.hit.name.as_str(), *id))
                .collect();
            for h in &report.items {
                if let Some(id) = by_name.get(h.node.name.as_str()) {
                    hub_order.push(*id);
                }
            }
            hub_order.iter().copied().collect()
        }
    };

    // Targets that are decisions or board items.
    let ids_in = |ns: &[&str]| -> Result<BTreeSet<Id>, StoreError> {
        let mut out = BTreeSet::new();
        for n in ns {
            for (_, targets) in tx.exports_in(n)? {
                out.extend(targets);
            }
        }
        Ok(out)
    };
    let adrs = ids_in(&[ADR_NS])?;
    let items = ids_in(&[ITEM_NS, REQ_NS])?;

    let mut entries: Vec<(Id, RationaleEntry)> = Vec::new();
    // Comment markers.
    for id in &in_scope {
        let Some(node) = nodes.get(id) else { continue };
        let Some(Fact::List(list)) = node.facts.get(FACT_RATIONALE) else {
            continue;
        };
        for e in list {
            let Fact::Map(m) = e else { continue };
            let (Some(Fact::Str(marker)), Some(Fact::Int(line)), Some(Fact::Str(text))) =
                (m.get("marker"), m.get("line"), m.get("text"))
            else {
                continue;
            };
            let Ok(line) = u32::try_from(*line) else {
                continue;
            };
            entries.push((
                *id,
                RationaleEntry {
                    node: node.hit.clone(),
                    kind: marker.to_lowercase(),
                    line,
                    text: text.clone(),
                    target: None,
                },
            ));
        }
    }
    // Citations out of the node, and documents that link to it.
    let label_of = |id: Id| -> (String, String) {
        match nodes.get(&id) {
            Some(n) => {
                let label = n.hit.name.rsplit_once('#').map_or(&*n.hit.name, |(_, l)| l);
                (label.to_string(), n.hit.name.clone())
            }
            None => (id.to_hex(), id.to_hex()),
        }
    };
    for (key, row) in tx.links_all()? {
        let kind = key.kind.0.as_str();
        if RATIONALE_KINDS.contains(&kind) && in_scope.contains(&key.from) {
            let class = if adrs.contains(&key.to) {
                "adr"
            } else if items.contains(&key.to) {
                "item"
            } else {
                continue;
            };
            let Some(node) = nodes.get(&key.from) else {
                continue;
            };
            let line = row
                .entries
                .iter()
                .filter_map(|e| e.span.map(|s| s.start_line))
                .min()
                .unwrap_or(node.hit.span.start_line);
            let (label, target) = label_of(key.to);
            entries.push((
                key.from,
                RationaleEntry {
                    node: node.hit.clone(),
                    kind: class.to_string(),
                    line,
                    text: label,
                    target: Some(target),
                },
            ));
        } else if kind == "links-to" && in_scope.contains(&key.to) {
            let (Some(from), Some(to)) = (nodes.get(&key.from), nodes.get(&key.to)) else {
                continue;
            };
            if from.artifact == to.artifact || !symbols_contain(&symbols, key.from) {
                continue;
            }
            entries.push((
                key.to,
                RationaleEntry {
                    node: to.hit.clone(),
                    kind: "doc".to_string(),
                    line: to.hit.span.start_line,
                    text: from.hit.name.clone(),
                    target: Some(from.hit.name.clone()),
                },
            ));
        }
    }

    let uncovered: Vec<SymbolHit> = {
        let covered: HashSet<Id> = entries.iter().map(|(id, _)| *id).collect();
        hub_order
            .iter()
            .filter(|id| !covered.contains(id))
            .filter_map(|id| nodes.get(id).map(|n| n.hit.clone()))
            .collect()
    };

    let mut items: Vec<RationaleEntry> = entries.into_iter().map(|(_, e)| e).collect();
    items.sort_by(|a, b| {
        (&a.node.path, a.line, &a.kind, &a.text, &a.node.name).cmp(&(
            &b.node.path,
            b.line,
            &b.kind,
            &b.text,
            &b.node.name,
        ))
    });
    items.dedup();
    let entries_total = items.len() as u64;
    items.truncate(limit);
    Ok(RationaleReport {
        entries_total,
        uncovered,
        items,
    })
}

/// True when `id` is a symbol (not an artifact).
fn symbols_contain(symbols: &[(Id, katalogr_store::SymbolRow)], id: Id) -> bool {
    symbols.binary_search_by_key(&id, |(i, _)| *i).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Fx;

    #[test]
    fn entry_kinds_classified() {
        let mut fx = Fx::new();
        let s = fx.sym_at("code.tx", "fn", "s", 5);
        let t = fx.sym_at("code.tx", "fn", "t", 9);
        let u = fx.sym_at("code.tx", "fn", "u", 12);
        let adr = fx.sym("adr.tx", "ADR-0001");
        let item = fx.sym("item.tx", "FEAT-001");
        let section = fx.sym_at("doc.tx", "section", "overview", 1);
        fx.export("board-adr", "ADR-0001", adr);
        fx.export("board-item", "FEAT-001", item);
        fx.note(s, "WHY", 3, "kept for the exporter");
        fx.link(s, "annotated-by", adr);
        fx.link(s, "refs-item", item);
        fx.link(s, "refs-item", t); // not a decision or item: ignored
        fx.link(section, "links-to", s);
        fx.link(t, "calls", s);
        fx.link(u, "calls", s);
        fx.link(u, "calls", t);
        let built = fx.build();
        let tx = built.read();

        let r = rationale(&tx, &RationaleScope::Path("code".into()), 100).unwrap();
        let kinds: Vec<(&str, &str)> = r
            .items
            .iter()
            .map(|e| (e.kind.as_str(), e.text.as_str()))
            .collect();
        assert_eq!(r.entries_total, 4);
        assert!(kinds.contains(&("why", "kept for the exporter")));
        assert!(kinds.contains(&("adr", "fn:ADR-0001")));
        assert!(kinds.contains(&("item", "fn:FEAT-001")));
        assert!(
            kinds.contains(&("doc", "r:doc.tx#section:overview")),
            "{kinds:?}"
        );
        let doc = r.items.iter().find(|e| e.kind == "doc").unwrap();
        assert_eq!(doc.target.as_deref(), Some("r:doc.tx#section:overview"));
        assert!(r.uncovered.is_empty());

        // The limit truncates after counting.
        let r = rationale(&tx, &RationaleScope::All, 1).unwrap();
        assert_eq!((r.entries_total, r.items.len()), (4, 1));

        // The top hubs without any entry are named: `s` has them, `t` does not.
        let r = rationale(&tx, &RationaleScope::Hubs(2), 100).unwrap();
        let names: Vec<&str> = r.uncovered.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, ["r:code.tx#fn:t"]);
    }

    #[test]
    fn symbol_scope_follows_contains() {
        let mut fx = Fx::new();
        let class = fx.sym_at("code.tx", "class", "C", 1);
        let method = fx.sym_at("code.tx", "method", "m", 3);
        let other = fx.sym_at("code.tx", "fn", "other", 9);
        fx.link(class, "contains", method);
        fx.note(method, "NOTE", 4, "inside");
        fx.note(other, "NOTE", 10, "outside");
        let built = fx.build();
        let r = rationale(&built.read(), &RationaleScope::Symbol(class), 10).unwrap();
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.items[0].text, "inside");
    }
}
