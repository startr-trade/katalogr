//! Deterministic text form of the whole graph (FEAT-001 section 4, requirement R-7).

use katalogr_model::{Id, Name};
use katalogr_store::{LinkOrigin, Reader};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// One JSON object per line, keys sorted, ending with a newline: first every node (artifacts,
/// then symbols, then contracts) sorted by canonical name, then every link entry sorted by
/// `(from, kind, to, confidence, origin, produced_by)`. Names are canonical names, never ids, so two
/// outputs can be compared with a text diff.
pub fn canonical_graph(snapshot: &impl Reader) -> String {
    render(snapshot).expect("reading a snapshot for export")
}

fn render(snap: &impl Reader) -> Result<String, katalogr_store::StoreError> {
    let artifacts = snap.artifacts()?;
    let symbols = snap.symbols()?;
    let contracts = snap.contracts()?;
    let mut names: BTreeMap<Id, String> = BTreeMap::new();
    let mut lines: Vec<String> = Vec::new();

    let mut nodes: Vec<(String, Value)> = Vec::new();
    for (id, a) in &artifacts {
        let name = a.name.canonical();
        names.insert(*id, name.clone());
        nodes.push((
            name.clone(),
            json!({
                "extension": a.extension,
                "facts": a.facts,
                "name": name,
                "node": "artifact",
                "revision": a.provenance.revision,
            }),
        ));
    }
    nodes.sort_by(|a, b| a.0.cmp(&b.0));
    let mut sym_nodes: Vec<(String, Value)> = Vec::new();
    for (id, s) in &symbols {
        let name = s.name.canonical();
        names.insert(*id, name.clone());
        sym_nodes.push((
            name.clone(),
            json!({
                "artifact": s.name.artifact.canonical(),
                "facts": s.facts,
                "kind": s.kind,
                "name": name,
                "node": "symbol",
                "span": s.span,
            }),
        ));
    }
    sym_nodes.sort_by(|a, b| a.0.cmp(&b.0));
    let mut contract_nodes: Vec<(String, Value)> = Vec::new();
    for (id, c) in &contracts {
        let name = c.name.canonical();
        names.insert(*id, name.clone());
        contract_nodes.push((
            name.clone(),
            json!({
                "consumers": c.consumers,
                "declarers": c.declarers,
                "key": c.name.key,
                "kind": c.name.kind,
                "name": name,
                "node": "contract",
                "providers": c.providers,
            }),
        ));
    }
    contract_nodes.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, v) in nodes
        .iter()
        .chain(sym_nodes.iter())
        .chain(contract_nodes.iter())
    {
        lines.push(text(v));
    }

    let label = |id: Id| names.get(&id).cloned().unwrap_or_else(|| id.to_hex());
    type LinkSortKey = (
        String,
        String,
        String,
        katalogr_model::Confidence,
        LinkOrigin,
        Option<String>,
    );
    let mut links: Vec<(LinkSortKey, Value)> = Vec::new();
    let ids = artifacts
        .iter()
        .map(|(i, _)| i)
        .chain(symbols.iter().map(|(i, _)| i));
    for id in ids {
        for (key, row) in snap.links_out(*id, None)? {
            for e in row.entries {
                let (from, to) = (label(key.from), label(key.to));
                let origin = match e.origin {
                    LinkOrigin::Intra => json!("intra"),
                    LinkOrigin::Ref(n) => json!(format!("ref:{n}")),
                    LinkOrigin::Bound { artifact, index } => {
                        json!(format!("bound:{}:{index}", label(artifact)))
                    }
                };
                let mut obj = json!({
                    "confidence": e.confidence,
                    "from": from,
                    "kind": key.kind.0,
                    "link": true,
                    "origin": origin,
                    "revision": e.provenance.revision,
                    "span": e.span,
                    "to": to,
                });
                if let Some(p) = &e.produced_by {
                    obj["produced_by"] = json!(p);
                }
                links.push((
                    (
                        from.clone(),
                        key.kind.0.clone(),
                        to.clone(),
                        e.confidence,
                        e.origin,
                        e.produced_by.clone(),
                    ),
                    obj,
                ));
            }
        }
    }
    links.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, v) in &links {
        lines.push(text(v));
    }
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    Ok(out)
}

/// Compact JSON with object keys sorted at every level.
fn text(v: &Value) -> String {
    let mut s = String::new();
    write_sorted(v, &mut s);
    s
}

fn write_sorted(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            let sorted: BTreeMap<&String, &Value> = m.iter().collect();
            out.push('{');
            for (i, (k, v)) in sorted.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                write_sorted(v, out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, v) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_sorted(v, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}
