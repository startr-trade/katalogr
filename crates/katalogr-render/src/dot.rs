//! Graphviz DOT output (FEAT-032 R-1). Same node and edge order as the Mermaid renderer.

use crate::graph::{ViewEdge, ViewGraph, ViewNode};
use crate::mermaid::node_text;
use std::fmt::Write;

/// A double-quoted DOT string: backslash and double quote are escaped with a backslash, and a
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

/// `digraph katalogr`, a comment naming the query, `rankdir=LR`, the nodes, a `partial` note
/// when the view is partial, then the edges. An inferred edge is dashed.
pub fn to_dot(g: &ViewGraph) -> String {
    let mut nodes: Vec<&ViewNode> = g.nodes.iter().collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let mut edges: Vec<&ViewEdge> = g.edges.iter().collect();
    edges.sort_by(|a, b| (&a.from, &a.to, &a.kind).cmp(&(&b.from, &b.to, &b.kind)));

    let mut out = String::from("digraph katalogr {\n");
    let title: String = g
        .title
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let _ = writeln!(out, "// katalogr view: {title}");
    out.push_str("  rankdir=LR;\n");
    for n in &nodes {
        let _ = writeln!(out, "  {} [label={}];", quote(&n.id), quote(&node_text(n)));
    }
    if g.total > g.shown {
        let note = format!("{} of {} nodes shown", g.shown, g.total);
        let _ = writeln!(out, "  \"partial\" [label={}, shape=note];", quote(&note));
    }
    for e in edges {
        let style = if e.inferred { ", style=dashed" } else { "" };
        let _ = writeln!(
            out,
            "  {} -> {} [label={}{style}];",
            quote(&e.from),
            quote(&e.to),
            quote(&e.kind)
        );
    }
    out.push_str("}\n");
    out
}

/// R-1 (FEAT-032): the DOT text of a partial view, with escaped labels and a dashed inferred edge.
#[cfg(test)]
#[test]
fn golden_output() {
    let node = |id: &str, name: &str, label: &str| ViewNode {
        id: id.to_string(),
        name: name.to_string(),
        label: label.to_string(),
        kind: "fn".to_string(),
        path: None,
        group: None,
    };
    let g = ViewGraph {
        title: "structure --level package".to_string(),
        nodes: vec![
            node("n3", "x/c", "a<&>\"q\"\\b"),
            node("n1", "core/lib", ""),
            node("n2", "app/main#fn:run", "run"),
        ],
        edges: vec![
            ViewEdge {
                from: "n2".to_string(),
                to: "n3".to_string(),
                kind: "uses".to_string(),
                inferred: true,
            },
            ViewEdge {
                from: "n1".to_string(),
                to: "n2".to_string(),
                kind: "calls".to_string(),
                inferred: false,
            },
        ],
        shown: 3,
        total: 5,
    };
    let expected = r#"digraph katalogr {
// katalogr view: structure --level package
  rankdir=LR;
  "n1" [label="core/lib"];
  "n2" [label="run"];
  "n3" [label="a<&>\"q\"\\b"];
  "partial" [label="3 of 5 nodes shown", shape=note];
  "n1" -> "n2" [label="calls"];
  "n2" -> "n3" [label="uses", style=dashed];
}
"#;
    assert_eq!(to_dot(&g), expected);
}
