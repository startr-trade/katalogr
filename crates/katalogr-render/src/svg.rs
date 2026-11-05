//! SVG output: the graph is written as DOT and laid out in pure Rust by `layout-rs`.

use crate::RenderError;
#[cfg(test)]
use crate::graph::{ViewEdge, node_id};
use crate::graph::{ViewGraph, ViewNode};
use crate::mermaid::short_label;
use layout::backends::svg::SVGWriter;
use layout::gv::{DotParser, GraphBuilder};
use std::collections::BTreeMap;
use std::fmt::Write;

/// Escape text for a double-quoted DOT string: backslash before `\` and `"`, a space for a line
/// break.
fn dot_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn label_of(n: &ViewNode) -> String {
    if n.label.is_empty() {
        short_label(&n.name)
    } else {
        n.label.clone()
    }
}

/// The DOT text the layout step reads: sorted nodes, then sorted edges, inferred edges dashed.
/// A partial view gets one more node noting how much is shown.
fn dot_text(g: &ViewGraph) -> String {
    let nodes: BTreeMap<&str, &ViewNode> = g.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut edges: Vec<_> = g.edges.iter().collect();
    edges.sort_by(|a, b| (&a.from, &a.to, &a.kind).cmp(&(&b.from, &b.to, &b.kind)));
    let mut out = String::from("digraph G {\n");
    for (id, n) in &nodes {
        let _ = writeln!(out, "  {id} [label=\"{}\"];", dot_escape(&label_of(n)));
    }
    for e in edges {
        let style = if e.inferred { ", style=dashed" } else { "" };
        let _ = writeln!(
            out,
            "  {} -> {} [label=\"{}\"{style}];",
            e.from,
            e.to,
            dot_escape(&e.kind)
        );
    }
    if g.total > g.shown {
        let _ = writeln!(
            out,
            "  partial [label=\"{}\"];",
            dot_escape(&format!("{} of {} nodes shown", g.shown, g.total))
        );
    }
    out.push_str("}\n");
    out
}

/// Lays the graph out as SVG. Errors only if the layout crate refuses the text. The output holds
/// no script element: the writer escapes text and emits only shapes, paths and text.
pub fn to_svg(g: &ViewGraph) -> Result<String, RenderError> {
    let dot = dot_text(g);
    let mut parser = DotParser::new(&dot);
    let ast = parser.process().map_err(RenderError::Layout)?;
    let mut builder = GraphBuilder::new();
    builder.visit_graph(&ast);
    let mut vg = builder.get();
    let mut svg = SVGWriter::new();
    vg.do_it(false, false, false, &mut svg);
    Ok(svg.finalize())
}

#[cfg(test)]
fn node(name: &str, label: &str) -> ViewNode {
    ViewNode {
        id: node_id(name),
        name: name.into(),
        label: label.into(),
        kind: "unit".into(),
        path: None,
        group: None,
    }
}

#[cfg(test)]
fn sample(count: usize) -> ViewGraph {
    let mut nodes: Vec<ViewNode> = (0..count)
        .map(|i| node(&format!("pkg/m{i}"), &format!("module {i}")))
        .collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let mut edges = Vec::new();
    for i in 0..count {
        for step in [1usize, 3] {
            edges.push(ViewEdge {
                from: node_id(&format!("pkg/m{i}")),
                to: node_id(&format!("pkg/m{}", (i + step) % count)),
                kind: "uses".into(),
                inferred: step == 3,
            });
        }
    }
    edges.sort_by(|a, b| (&a.from, &a.to, &a.kind).cmp(&(&b.from, &b.to, &b.kind)));
    ViewGraph {
        title: "structure".into(),
        shown: count as u64,
        total: count as u64 + 2,
        nodes,
        edges,
    }
}

#[cfg(test)]
#[test]
fn dot_to_svg_repeats() {
    // Every run builds fresh hash maps with their own random seed, so repeats in one
    // process are as telling as repeats across processes.
    let g = sample(24);
    let first = to_svg(&g).expect("layout");
    assert!(first.contains("<svg"));
    for _ in 0..20 {
        assert_eq!(first, to_svg(&g).expect("layout"), "layout bytes differ");
    }
}

#[cfg(test)]
#[test]
fn no_script_in_output() {
    let mut g = sample(3);
    g.nodes[0].label = "<script>alert(1)</script> & \"q\" \\ x".into();
    let svg = to_svg(&g).expect("layout");
    assert!(!svg.to_lowercase().contains("<script"), "{svg}");
    assert!(!svg.contains("onload") && !svg.contains("javascript:"));
    assert!(svg.contains("&lt;script&gt;"), "label text is escaped");
}
