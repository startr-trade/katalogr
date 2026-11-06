//! Cytoscape `elements` JSON for a [`ViewGraph`] (FEAT-034 R-11). The shape is what
//! Cytoscape.js takes directly. Compact, sorted, and with no timestamp.

use crate::graph::{ViewEdge, ViewGraph, ViewNode};
use crate::mermaid::node_text;
use serde::Serialize;

#[derive(Serialize)]
struct Doc<'a> {
    format: &'static str,
    format_version: u32,
    title: &'a str,
    shown: u64,
    total: u64,
    elements: Elements<'a>,
}

#[derive(Serialize)]
struct Elements<'a> {
    nodes: Vec<NodeEl<'a>>,
    edges: Vec<EdgeEl<'a>>,
}

#[derive(Serialize)]
struct NodeEl<'a> {
    data: NodeData<'a>,
}

#[derive(Serialize)]
struct NodeData<'a> {
    id: &'a str,
    name: &'a str,
    label: String,
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<&'a str>,
}

#[derive(Serialize)]
struct EdgeEl<'a> {
    data: EdgeData<'a>,
}

#[derive(Serialize)]
struct EdgeData<'a> {
    id: String,
    source: &'a str,
    target: &'a str,
    kind: &'a str,
    inferred: bool,
}

/// The graph in Cytoscape's `elements` JSON. Compact, sorted, and with no timestamp. Nodes are
/// sorted by id and edges by from, to, kind; a `None` path or group is left out.
pub fn to_cytoscape_json(g: &ViewGraph) -> String {
    let mut nodes: Vec<&ViewNode> = g.nodes.iter().collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let mut edges: Vec<&ViewEdge> = g.edges.iter().collect();
    edges.sort_by(|a, b| (&a.from, &a.to, &a.kind).cmp(&(&b.from, &b.to, &b.kind)));
    let doc = Doc {
        format: "katalogr-graph",
        format_version: 1,
        title: &g.title,
        shown: g.shown,
        total: g.total,
        elements: Elements {
            nodes: nodes
                .iter()
                .map(|n| NodeEl {
                    data: NodeData {
                        id: &n.id,
                        name: &n.name,
                        label: node_text(n),
                        kind: &n.kind,
                        path: n.path.as_deref(),
                        group: n.group.as_deref(),
                    },
                })
                .collect(),
            edges: edges
                .iter()
                .map(|e| EdgeEl {
                    data: EdgeData {
                        id: format!("{}-{}-{}", e.from, e.to, e.kind),
                        source: &e.from,
                        target: &e.to,
                        kind: &e.kind,
                        inferred: e.inferred,
                    },
                })
                .collect(),
        },
    };
    serde_json::to_string(&doc).expect("a graph always serialises")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, name: &str, path: Option<&str>, group: Option<&str>) -> ViewNode {
        ViewNode {
            id: id.into(),
            name: name.into(),
            label: String::new(),
            kind: "fn".into(),
            path: path.map(Into::into),
            group: group.map(Into::into),
        }
    }

    fn edge(from: &str, to: &str, kind: &str) -> ViewEdge {
        ViewEdge {
            from: from.into(),
            to: to.into(),
            kind: kind.into(),
            inferred: false,
        }
    }

    #[test]
    fn output_is_sorted() {
        let g = ViewGraph {
            title: "t".into(),
            nodes: vec![node("n2", "b", None, None), node("n1", "a", None, None)],
            edges: vec![edge("n2", "n1", "uses"), edge("n1", "n2", "calls")],
            shown: 2,
            total: 2,
        };
        let v: serde_json::Value = serde_json::from_str(&to_cytoscape_json(&g)).unwrap();
        let ids: Vec<&str> = v["elements"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["data"]["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["n1", "n2"]);
        let eids: Vec<&str> = v["elements"]["edges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["data"]["id"].as_str().unwrap())
            .collect();
        assert_eq!(eids, ["n1-n2-calls", "n2-n1-uses"]);
        assert_eq!(v["format"], "katalogr-graph");
        assert_eq!(v["format_version"], 1);
        let mut r = g.clone();
        r.nodes.reverse();
        r.edges.reverse();
        assert_eq!(to_cytoscape_json(&r), to_cytoscape_json(&g));
        assert!(!to_cytoscape_json(&g).contains('\n'));
    }

    #[test]
    fn missing_fields_left_out() {
        let g = ViewGraph {
            title: "t".into(),
            nodes: vec![
                node("n1", "core/lib", None, None),
                node("n2", "x", Some("p.rs"), Some("app")),
            ],
            edges: vec![],
            shown: 1,
            total: 5,
        };
        let v: serde_json::Value = serde_json::from_str(&to_cytoscape_json(&g)).unwrap();
        let n = &v["elements"]["nodes"];
        assert!(n[0]["data"].get("path").is_none());
        assert!(n[0]["data"].get("group").is_none());
        assert_eq!(n[1]["data"]["path"], "p.rs");
        assert_eq!(n[1]["data"]["group"], "app");
        assert_eq!(v["shown"], 1);
        assert_eq!(v["total"], 5);
    }
}
