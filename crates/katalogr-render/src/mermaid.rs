//! Mermaid flowchart output.

use crate::graph::{ViewEdge, ViewGraph, ViewNode, node_id};
use std::collections::BTreeSet;
use std::fmt::Write;

/// Most characters of a label.
const LABEL_MAX: usize = 60;

/// The last two segments of a canonical name (split at `/` and `#`), joined with `/`, cut to
/// 60 characters. The full name stays in `ViewNode.name`.
pub(crate) fn short_label(name: &str) -> String {
    let segments: Vec<&str> = name.split(['/', '#']).filter(|s| !s.is_empty()).collect();
    let tail = &segments[segments.len().saturating_sub(2)..];
    let joined = if tail.is_empty() {
        name.to_string()
    } else {
        tail.join("/")
    };
    cut(&joined)
}

fn cut(s: &str) -> String {
    s.chars().take(LABEL_MAX).collect()
}

/// Escape text for a double-quoted Mermaid label: entity codes for the characters that end a
/// label or start markup, and a space for a line break.
pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("#amp;"),
            '"' => out.push_str("#quot;"),
            '<' => out.push_str("#lt;"),
            '>' => out.push_str("#gt;"),
            '#' => out.push_str("#35;"),
            ';' => out.push_str("#59;"),
            '{' => out.push_str("#123;"),
            '}' => out.push_str("#125;"),
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn node_line(n: &ViewNode, indent: &str) -> String {
    let text = if n.label.is_empty() {
        short_label(&n.name)
    } else {
        cut(&n.label)
    };
    format!("{indent}{}[\"{}\"]\n", n.id, escape(&text))
}

fn edge_line(e: &ViewEdge) -> String {
    let arrow = if e.inferred { "-.->" } else { "-->" };
    format!("  {} {arrow}|\"{}\"| {}\n", e.from, escape(&e.kind), e.to)
}

/// `flowchart LR`, a comment naming the query, the nodes, then the edges. A view from two or
/// more repositories draws one `subgraph` per repository; an inferred edge is dashed; a partial
/// view ends its nodes with a note saying how many are shown.
pub fn to_mermaid(g: &ViewGraph) -> String {
    let mut nodes: Vec<&ViewNode> = g.nodes.iter().collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let mut edges: Vec<&ViewEdge> = g.edges.iter().collect();
    edges.sort_by(|a, b| (&a.from, &a.to, &a.kind).cmp(&(&b.from, &b.to, &b.kind)));

    let groups: BTreeSet<&str> = nodes.iter().filter_map(|n| n.group.as_deref()).collect();
    let grouped = groups.len() >= 2;

    let mut out = String::from("flowchart LR\n");
    let title: String = g
        .title
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let _ = writeln!(out, "%% katalogr view: {title}");
    if grouped {
        for n in nodes.iter().filter(|n| n.group.is_none()) {
            out.push_str(&node_line(n, "  "));
        }
        for group in &groups {
            let gid = format!("g{}", &node_id(group)[1..]);
            let _ = writeln!(out, "  subgraph {gid}[\"{}\"]", escape(group));
            for n in nodes.iter().filter(|n| n.group.as_deref() == Some(*group)) {
                out.push_str(&node_line(n, "    "));
            }
            out.push_str("  end\n");
        }
    } else {
        for n in &nodes {
            out.push_str(&node_line(n, "  "));
        }
    }
    if g.total > g.shown {
        let _ = writeln!(out, "  partial[\"{} of {} nodes shown\"]", g.shown, g.total);
    }
    for e in edges {
        out.push_str(&edge_line(e));
    }
    out
}

#[cfg(test)]
fn node(name: &str, label: &str, group: Option<&str>) -> ViewNode {
    ViewNode {
        id: node_id(name),
        name: name.to_string(),
        label: label.to_string(),
        kind: "fn".to_string(),
        path: None,
        group: group.map(str::to_string),
    }
}

#[cfg(test)]
fn graph(nodes: Vec<ViewNode>, edges: Vec<ViewEdge>) -> ViewGraph {
    let n = nodes.len() as u64;
    ViewGraph {
        title: "structure --level package".to_string(),
        nodes,
        edges,
        shown: n,
        total: n,
    }
}

/// R-3 (FEAT-032): an id depends only on the canonical name, and has the documented shape.
#[cfg(test)]
#[test]
fn ids_are_stable() {
    // SHA-256 of "abc" begins ba7816bf8f01cfea.
    assert_eq!(node_id("abc"), "nba7816bf8f01");
    assert_eq!(node_id("abc"), node_id("abc"));
    assert_ne!(node_id("abc"), node_id("abd"));
    let id = node_id("r:src/a.tx#fn:hub");
    assert_eq!(id.len(), 13);
    assert!(id.starts_with('n'));
    assert!(id[1..].chars().all(|c| c.is_ascii_hexdigit()));
    let g = graph(vec![node("a", "", None), node("b", "", None)], vec![]);
    assert_eq!(to_mermaid(&g), to_mermaid(&g.clone()));
}

/// R-3 (FEAT-032): nodes and edges come out sorted whatever order they went in.
#[cfg(test)]
#[test]
fn output_is_sorted() {
    let names = ["delta", "alpha", "charlie", "bravo"];
    let nodes: Vec<ViewNode> = names.iter().map(|n| node(n, n, None)).collect();
    let edge = |a: &str, b: &str, k: &str| ViewEdge {
        from: node_id(a),
        to: node_id(b),
        kind: k.to_string(),
        inferred: false,
    };
    let edges = vec![
        edge("delta", "alpha", "calls"),
        edge("alpha", "bravo", "uses"),
        edge("alpha", "bravo", "calls"),
    ];
    let forward = to_mermaid(&graph(nodes.clone(), edges.clone()));
    let mut rn = nodes;
    rn.reverse();
    let mut re = edges;
    re.reverse();
    assert_eq!(forward, to_mermaid(&graph(rn, re)));

    let node_ids: Vec<&str> = forward
        .lines()
        .filter(|l| l.contains("[\""))
        .map(|l| l.trim().split('[').next().unwrap())
        .collect();
    let mut sorted = node_ids.clone();
    sorted.sort();
    assert_eq!(node_ids, sorted);
    assert_eq!(node_ids.len(), 4);
    let edge_lines: Vec<&str> = forward.lines().filter(|l| l.contains("-->")).collect();
    let mut sorted_edges = edge_lines.clone();
    sorted_edges.sort_by_key(|l| {
        let mut p = l.split_whitespace();
        (
            p.next().unwrap().to_string(),
            l.split("| ").last().unwrap().to_string(),
        )
    });
    assert_eq!(edge_lines.len(), 3);
    assert!(forward.starts_with("flowchart LR\n%% katalogr view: structure --level package\n"));
}

/// R-4 (FEAT-032): a view with fewer nodes than the whole says so, and a whole view does not.
#[cfg(test)]
#[test]
fn partial_note_shown() {
    let mut g = graph(vec![node("a", "a", None)], vec![]);
    assert!(!to_mermaid(&g).contains("partial"));
    g.shown = 40;
    g.total = 212;
    let text = to_mermaid(&g);
    assert!(
        text.contains("  partial[\"40 of 212 nodes shown\"]\n"),
        "{text}"
    );
}

/// R-5 (FEAT-032): every character that could end a label or start markup is an entity code.
#[cfg(test)]
#[test]
fn labels_escaped() {
    let g = graph(
        vec![node("x", "a&b \"q\" <i>#1; {k}\nnext", None)],
        vec![ViewEdge {
            from: node_id("x"),
            to: node_id("x"),
            kind: "ca\"lls".to_string(),
            inferred: true,
        }],
    );
    let text = to_mermaid(&g);
    assert!(
        text.contains("[\"a#amp;b #quot;q#quot; #lt;i#gt;#35;1#59; #123;k#125; next\"]"),
        "{text}"
    );
    assert!(text.contains("-.->|\"ca#quot;lls\"|"), "{text}");
    let label_part = text.lines().nth(2).unwrap();
    assert_eq!(label_part.matches('"').count(), 2);
}

/// R-5 (FEAT-032): labels that are Mermaid keywords or try to break out stay text, and no id is
/// a keyword.
#[cfg(test)]
#[test]
fn keyword_labels_safe() {
    let labels = [
        "end",
        "graph",
        "subgraph x",
        "flowchart LR",
        "style",
        "click",
        "\"]; A --> B; [\"",
    ];
    let nodes: Vec<ViewNode> = labels
        .iter()
        .enumerate()
        .map(|(i, l)| node(&format!("k{i}"), l, None))
        .collect();
    let text = to_mermaid(&graph(nodes, vec![]));
    for line in text.lines().skip(2) {
        let line = line.trim();
        let (id, rest) = line.split_once('[').unwrap();
        assert!(id.starts_with('n') && id[1..].chars().all(|c| c.is_ascii_hexdigit()));
        assert!(rest.starts_with('"') && rest.ends_with("\"]"));
        let inner = &rest[1..rest.len() - 2];
        assert!(!inner.contains('"') && !inner.contains(';') || inner.contains('#'));
        assert!(!inner.contains("-->") || inner.contains("#gt;"));
    }
    assert!(text.contains("[\"end\"]") && text.contains("[\"graph\"]"));
}

/// Two repositories draw one subgraph each; one repository draws none.
#[cfg(test)]
#[test]
fn groups_draw_subgraphs_from_two() {
    let two = graph(
        vec![node("a", "a", Some("r1")), node("b", "b", Some("r2"))],
        vec![],
    );
    let text = to_mermaid(&two);
    assert_eq!(text.matches("subgraph").count(), 2);
    assert_eq!(text.matches("  end\n").count(), 2);
    let one = graph(
        vec![node("a", "a", Some("r1")), node("b", "b", Some("r1"))],
        vec![],
    );
    assert!(!to_mermaid(&one).contains("subgraph"));
}
