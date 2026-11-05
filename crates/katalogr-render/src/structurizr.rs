//! Structurizr DSL output (FEAT-032 R-1). One `softwareSystem` per group, each node a
//! `container`, each edge a relationship with its kind.

use crate::graph::{ViewEdge, ViewGraph, ViewNode, node_id};
use crate::mermaid::short_label;
use std::collections::BTreeMap;
use std::fmt::Write;

/// Most characters of a label, as in the Mermaid renderer.
const LABEL_MAX: usize = 60;

/// Text to draw: the label, or the shortened canonical name when the label is empty.
fn node_text(n: &ViewNode) -> String {
    if n.label.is_empty() {
        short_label(&n.name)
    } else {
        n.label.chars().take(LABEL_MAX).collect()
    }
}

/// A double-quoted DSL string: backslash and double quote are escaped with a backslash, and a
/// line break becomes a space.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The identifier of the system for a group: `s` plus the group's node-id digits. Nodes with no
/// group sit in the system `ungrouped`.
fn system_id(group: &str) -> String {
    format!("s{}", &node_id(group)[1..])
}

/// `workspace` with the title and a description giving how many nodes are shown, then one
/// `softwareSystem` per group (nodes without a group in `ungrouped`) holding its nodes as
/// containers, then the edges as relationships with their kind. An inferred edge carries the
/// technology `inferred`.
pub fn to_structurizr(g: &ViewGraph) -> String {
    let mut nodes: Vec<&ViewNode> = g.nodes.iter().collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let mut edges: Vec<&ViewEdge> = g.edges.iter().collect();
    edges.sort_by(|a, b| (&a.from, &a.to, &a.kind).cmp(&(&b.from, &b.to, &b.kind)));

    // `None` (ungrouped) sorts before every `Some`, so ungrouped nodes come first.
    let mut by_group: BTreeMap<Option<&str>, Vec<&ViewNode>> = BTreeMap::new();
    for n in &nodes {
        by_group.entry(n.group.as_deref()).or_default().push(n);
    }

    let description = format!("{} of {} nodes shown", g.shown, g.total);
    let mut out = format!(
        "workspace {} {} {{\n    model {{\n",
        quote(&g.title),
        quote(&description)
    );
    for (group, members) in &by_group {
        match group {
            None => out.push_str("        ungrouped = softwareSystem \"ungrouped\" {\n"),
            Some(name) => {
                let _ = writeln!(
                    out,
                    "        {} = softwareSystem {} {{",
                    system_id(name),
                    quote(name)
                );
            }
        }
        for n in members {
            let _ = writeln!(
                out,
                "            {} = container {} {} {}",
                n.id,
                quote(&node_text(n)),
                quote(&n.name),
                quote(&n.kind)
            );
        }
        out.push_str("        }\n");
    }
    for e in edges {
        let technology = if e.inferred { " \"inferred\"" } else { "" };
        let _ = writeln!(
            out,
            "        {} -> {} {}{technology}",
            e.from,
            e.to,
            quote(&e.kind)
        );
    }
    out.push_str("    }\n}\n");
    out
}

/// R-1 (FEAT-032): the Structurizr text of a partial view with a grouped and an ungrouped node
/// and an inferred edge.
#[cfg(test)]
#[test]
fn golden_output() {
    let g = ViewGraph {
        title: "structure --level package".to_string(),
        nodes: vec![
            ViewNode {
                id: "n1".to_string(),
                name: "core/lib".to_string(),
                label: String::new(),
                kind: "fn".to_string(),
                path: None,
                group: Some("core".to_string()),
            },
            ViewNode {
                id: "n2".to_string(),
                name: "app/main#fn:run".to_string(),
                label: "run".to_string(),
                kind: "fn".to_string(),
                path: None,
                group: None,
            },
        ],
        edges: vec![
            ViewEdge {
                from: "n1".to_string(),
                to: "n2".to_string(),
                kind: "calls".to_string(),
                inferred: false,
            },
            ViewEdge {
                from: "n2".to_string(),
                to: "n1".to_string(),
                kind: "uses".to_string(),
                inferred: true,
            },
        ],
        shown: 2,
        total: 5,
    };
    let expected = format!(
        r#"workspace "structure --level package" "2 of 5 nodes shown" {{
    model {{
        ungrouped = softwareSystem "ungrouped" {{
            n2 = container "run" "app/main#fn:run" "fn"
        }}
        {} = softwareSystem "core" {{
            n1 = container "core/lib" "core/lib" "fn"
        }}
        n1 -> n2 "calls"
        n2 -> n1 "uses" "inferred"
    }}
}}
"#,
        format_args!("s{}", &node_id("core")[1..])
    );
    assert_eq!(to_structurizr(&g), expected);
}
