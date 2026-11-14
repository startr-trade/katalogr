//! SVG output: the graph is written as DOT and laid out in pure Rust by `layout-rs`.

use crate::RenderError;
use crate::graph::ViewGraph;
#[cfg(test)]
use crate::graph::{ViewEdge, ViewNode, node_id};
use layout::backends::svg::SVGWriter;
use layout::gv::{DotParser, GraphBuilder};

/// Lays the graph out as SVG. Errors only if the layout crate refuses the text. The output holds
/// no script element: the writer escapes text and emits only shapes, paths and text.
pub fn to_svg(g: &ViewGraph) -> Result<String, RenderError> {
    // layout-rs gives different bytes from run to run when the graph sets `rankdir`, so the
    // attribute is dropped and the layout runs top to bottom.
    let dot = crate::dot::to_dot(g).replace("  rankdir=LR;\n", "");
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

#[cfg(test)]
#[test]
fn quote_in_label_parses() {
    let mut g = sample(3);
    g.nodes[0].label = "say \"hi\" \\ there".into();
    let svg = to_svg(&g).expect("layout");
    assert!(
        svg.contains("say &quot;hi&quot;") || svg.contains("say \"hi\""),
        "{svg}"
    );
}

/// FEAT-125: a narrow, deeply layered DAG (each node feeds the next three) has exponentially many
/// paths; reachability once enumerated them all. Layout must finish in seconds, even unoptimised.
#[cfg(test)]
#[test]
fn dense_graph_renders_quickly() {
    let count = 60usize;
    let id = |i: usize| node_id(&format!("pkg/m{i}"));
    let mut nodes: Vec<ViewNode> = (0..count)
        .map(|i| node(&format!("pkg/m{i}"), &format!("module {i}")))
        .collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let mut edges = Vec::new();
    for i in 0..count {
        for j in i + 1..(i + 4).min(count) {
            edges.push(ViewEdge {
                from: id(i),
                to: id(j),
                kind: "uses".into(),
                inferred: false,
            });
        }
    }
    edges.sort_by(|a, b| (&a.from, &a.to, &a.kind).cmp(&(&b.from, &b.to, &b.kind)));
    let g = ViewGraph {
        title: "structure".into(),
        shown: count as u64,
        total: count as u64,
        nodes,
        edges,
    };
    let start = std::time::Instant::now();
    let svg = to_svg(&g).expect("layout");
    let took = start.elapsed();
    println!(
        "dense_graph_renders_quickly: {} edges in {took:?}",
        g.edges.len()
    );
    assert!(svg.contains("<svg"));
    assert!(took < std::time::Duration::from_secs(2), "took {took:?}");
}
