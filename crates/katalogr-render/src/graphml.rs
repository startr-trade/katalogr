//! GraphML output (FEAT-032 R-1). Same node and edge order as the Mermaid renderer.

use crate::graph::{ViewEdge, ViewGraph, ViewNode};
use crate::mermaid::short_label;
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

/// XML text or attribute content: the five markup characters become entity references, and
/// any other control character becomes a space, since XML 1.0 forbids it.
fn xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

const HEADER: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<graphml xmlns="http://graphml.graphdrawing.org/xmlns">
  <key id="title" for="graph" attr.name="title" attr.type="string"/>
  <key id="shown" for="graph" attr.name="shown" attr.type="long"/>
  <key id="total" for="graph" attr.name="total" attr.type="long"/>
  <key id="name" for="node" attr.name="name" attr.type="string"/>
  <key id="label" for="node" attr.name="label" attr.type="string"/>
  <key id="kind" for="node" attr.name="kind" attr.type="string"/>
  <key id="path" for="node" attr.name="path" attr.type="string"/>
  <key id="group" for="node" attr.name="group" attr.type="string"/>
  <key id="ekind" for="edge" attr.name="kind" attr.type="string"/>
  <key id="inferred" for="edge" attr.name="inferred" attr.type="boolean"/>
  <graph id="G" edgedefault="directed">
"#;

/// A GraphML document: the graph's title, shown and total counts as graph data, then each node
/// with its name, displayed label, kind and, when set, path and group, then each edge with its
/// kind and whether it is inferred.
pub fn to_graphml(g: &ViewGraph) -> String {
    let mut nodes: Vec<&ViewNode> = g.nodes.iter().collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let mut edges: Vec<&ViewEdge> = g.edges.iter().collect();
    edges.sort_by(|a, b| (&a.from, &a.to, &a.kind).cmp(&(&b.from, &b.to, &b.kind)));

    let mut out = String::from(HEADER);
    let _ = writeln!(out, "    <data key=\"title\">{}</data>", xml(&g.title));
    let _ = writeln!(out, "    <data key=\"shown\">{}</data>", g.shown);
    let _ = writeln!(out, "    <data key=\"total\">{}</data>", g.total);
    for n in &nodes {
        let _ = writeln!(out, "    <node id=\"{}\">", xml(&n.id));
        let _ = writeln!(out, "      <data key=\"name\">{}</data>", xml(&n.name));
        let _ = writeln!(
            out,
            "      <data key=\"label\">{}</data>",
            xml(&node_text(n))
        );
        let _ = writeln!(out, "      <data key=\"kind\">{}</data>", xml(&n.kind));
        if let Some(path) = &n.path {
            let _ = writeln!(out, "      <data key=\"path\">{}</data>", xml(path));
        }
        if let Some(group) = &n.group {
            let _ = writeln!(out, "      <data key=\"group\">{}</data>", xml(group));
        }
        out.push_str("    </node>\n");
    }
    for e in edges {
        let _ = writeln!(
            out,
            "    <edge source=\"{}\" target=\"{}\">",
            xml(&e.from),
            xml(&e.to)
        );
        let _ = writeln!(out, "      <data key=\"ekind\">{}</data>", xml(&e.kind));
        let _ = writeln!(out, "      <data key=\"inferred\">{}</data>", e.inferred);
        out.push_str("    </edge>\n");
    }
    out.push_str("  </graph>\n</graphml>\n");
    out
}

/// R-1 (FEAT-032): the GraphML text of a partial view, with escaped labels, optional fields and
/// an inferred edge.
#[cfg(test)]
#[test]
fn golden_output() {
    let g = ViewGraph {
        title: "structure --level package".to_string(),
        nodes: vec![
            ViewNode {
                id: "n2".to_string(),
                name: "app/main#fn:run".to_string(),
                label: "run".to_string(),
                kind: "fn".to_string(),
                path: Some("src/main.rs".to_string()),
                group: Some("app".to_string()),
            },
            ViewNode {
                id: "n1".to_string(),
                name: "core/lib".to_string(),
                label: "a<&>\"'".to_string(),
                kind: "fn".to_string(),
                path: None,
                group: None,
            },
        ],
        edges: vec![
            ViewEdge {
                from: "n2".to_string(),
                to: "n1".to_string(),
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
        shown: 2,
        total: 5,
    };
    let expected = r#"<?xml version="1.0" encoding="UTF-8"?>
<graphml xmlns="http://graphml.graphdrawing.org/xmlns">
  <key id="title" for="graph" attr.name="title" attr.type="string"/>
  <key id="shown" for="graph" attr.name="shown" attr.type="long"/>
  <key id="total" for="graph" attr.name="total" attr.type="long"/>
  <key id="name" for="node" attr.name="name" attr.type="string"/>
  <key id="label" for="node" attr.name="label" attr.type="string"/>
  <key id="kind" for="node" attr.name="kind" attr.type="string"/>
  <key id="path" for="node" attr.name="path" attr.type="string"/>
  <key id="group" for="node" attr.name="group" attr.type="string"/>
  <key id="ekind" for="edge" attr.name="kind" attr.type="string"/>
  <key id="inferred" for="edge" attr.name="inferred" attr.type="boolean"/>
  <graph id="G" edgedefault="directed">
    <data key="title">structure --level package</data>
    <data key="shown">2</data>
    <data key="total">5</data>
    <node id="n1">
      <data key="name">core/lib</data>
      <data key="label">a&lt;&amp;&gt;&quot;&apos;</data>
      <data key="kind">fn</data>
    </node>
    <node id="n2">
      <data key="name">app/main#fn:run</data>
      <data key="label">run</data>
      <data key="kind">fn</data>
      <data key="path">src/main.rs</data>
      <data key="group">app</data>
    </node>
    <edge source="n1" target="n2">
      <data key="ekind">calls</data>
      <data key="inferred">false</data>
    </edge>
    <edge source="n2" target="n1">
      <data key="ekind">uses</data>
      <data key="inferred">true</data>
    </edge>
  </graph>
</graphml>
"#;
    assert_eq!(to_graphml(&g), expected);
}
