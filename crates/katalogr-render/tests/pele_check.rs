//! FEAT-032 R-12: every kind of diagram katalogr generates is drawn by the vendored Pele.
//! Runs the vendored build under `node` (the route chosen for the open question on how tests run
//! Pele); it needs `node` on the PATH and fails, naming the problem, when there is none.

mod pele_check {
    use katalogr_render::{ViewEdge, ViewGraph, ViewNode, node_id, to_mermaid};
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::process::Command;

    fn node(name: &str, kind: &str, label: &str, group: Option<&str>) -> ViewNode {
        ViewNode {
            id: node_id(name),
            name: name.to_string(),
            label: label.to_string(),
            kind: kind.to_string(),
            path: None,
            group: group.map(str::to_string),
        }
    }

    fn edge(from: &str, to: &str, kind: &str, inferred: bool) -> ViewEdge {
        ViewEdge {
            from: node_id(from),
            to: node_id(to),
            kind: kind.to_string(),
            inferred,
        }
    }

    fn graph(title: &str, nodes: Vec<ViewNode>, edges: Vec<ViewEdge>, total: u64) -> ViewGraph {
        let mut nodes = nodes;
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        let mut edges = edges;
        edges.sort_by(|a, b| (&a.from, &a.to, &a.kind).cmp(&(&b.from, &b.to, &b.kind)));
        ViewGraph {
            title: title.to_string(),
            shown: nodes.len() as u64,
            total: total.max(nodes.len() as u64),
            nodes,
            edges,
        }
    }

    /// One graph for each op, and for each edge case the Mermaid writer has to survive.
    fn cases() -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let mut add = |name: &str, g: ViewGraph| {
            out.insert(name.to_string(), to_mermaid(&g));
        };
        let pkgs = ["core/lib", "app/main", "app/util"];
        add(
            "structure",
            graph(
                "structure --level package",
                pkgs.iter().map(|p| node(p, "package", "", None)).collect(),
                vec![
                    edge("app/main", "core/lib", "uses", false),
                    edge("app/util", "core/lib", "uses", true),
                ],
                3,
            ),
        );
        add(
            "structure_partial",
            graph(
                "structure --max-nodes 3",
                pkgs.iter().map(|p| node(p, "package", "", None)).collect(),
                vec![edge("app/main", "core/lib", "uses", false)],
                212,
            ),
        );
        add(
            "contracts_two_repos",
            graph(
                "contracts",
                vec![
                    node("contract:endpoint:orders", "contract", "", None),
                    node("api:a.tx#fn:serve", "fn", "", Some("api")),
                    node("web:b.tx#fn:fetch", "fn", "", Some("web")),
                    node("web:c.tx#fn:other", "fn", "", Some("web")),
                ],
                vec![
                    edge(
                        "api:a.tx#fn:serve",
                        "contract:endpoint:orders",
                        "provides",
                        false,
                    ),
                    edge(
                        "web:b.tx#fn:fetch",
                        "contract:endpoint:orders",
                        "consumes",
                        false,
                    ),
                    edge(
                        "web:c.tx#fn:other",
                        "contract:endpoint:orders",
                        "declares",
                        true,
                    ),
                ],
                1,
            ),
        );
        add(
            "callers",
            graph(
                "callers target --depth 2",
                vec![
                    node("app:a.tx#fn:target", "fn", "", Some("app")),
                    node("app:a.tx#fn:mid", "fn", "", Some("app")),
                    node("app:a.tx#fn:top", "fn", "", Some("app")),
                ],
                vec![
                    edge("app:a.tx#fn:mid", "app:a.tx#fn:target", "calls", false),
                    edge("app:a.tx#fn:top", "app:a.tx#fn:mid", "via-contract", true),
                ],
                3,
            ),
        );
        add(
            "path",
            graph(
                "path alpha gamma",
                vec![
                    node("app:a.tx#fn:alpha", "fn", "", None),
                    node("app:a.tx#fn:beta", "fn", "", None),
                ],
                vec![edge(
                    "app:a.tx#fn:alpha",
                    "app:a.tx#fn:beta",
                    "calls",
                    false,
                )],
                2,
            ),
        );
        add(
            "cycles",
            graph(
                "cycles --level file",
                vec![
                    node("app:f1.tx", "artifact", "", None),
                    node("app:f2.tx", "artifact", "", None),
                ],
                vec![
                    edge("app:f1.tx", "app:f2.tx", "references", false),
                    edge("app:f2.tx", "app:f1.tx", "references", false),
                ],
                2,
            ),
        );
        add("empty", graph("cycles --level file", vec![], vec![], 0));
        add(
            "hostile_labels",
            graph(
                "structure \"quoted\" <b>bold</b>; { } #",
                vec![
                    node("x/a", "fn", "say \"hi\" & <b>bye</b>; {a} #1", None),
                    node("x/b", "fn", "end", None),
                    node("x/c", "fn", "subgraph", None),
                    node("x/d", "fn", "graph TD\nflowchart LR", None),
                    node("x/e", "fn", "click call href", None),
                    node("x/f", "fn", "unicode \u{00e9}\u{4e2d}\u{1f600}", None),
                    node("x/g", "fn", &"long ".repeat(40), None),
                ],
                vec![
                    edge("x/a", "x/b", "a \"b\" <c> ; { }", false),
                    edge("x/b", "x/c", "end", true),
                    edge("x/c", "x/a", "plain", false),
                    edge("x/a", "x/a", "self", false),
                ],
                9,
            ),
        );
        out
    }

    /// R-12: each generated diagram, for every op and every edge case above, draws without an
    /// error in the same Pele build that ships.
    #[test]
    fn every_op_renders_in_pele() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let pele = manifest.join("assets/pele/pele.global.js");
        let script = manifest.join("tests/pele_check.mjs");
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
        std::fs::create_dir_all(dir).unwrap();
        let file = dir.join("pele_check_cases.json");
        let cases = cases();
        std::fs::write(&file, serde_json_text(&cases)).unwrap();
        let out = Command::new("node")
            .arg(&script)
            .arg(&pele)
            .arg(&file)
            .output()
            .expect("`node` is needed to run the vendored Pele; install it or fix the PATH");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "Pele refused a diagram:\n{text}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            text.contains(&format!("{0} of {0} rendered", cases.len())),
            "{text}"
        );
    }

    /// A JSON object of strings, written by hand so the crate takes no new dependency.
    fn serde_json_text(cases: &BTreeMap<String, String>) -> String {
        let quote = |s: &str| {
            let mut o = String::from("\"");
            for c in s.chars() {
                match c {
                    '"' => o.push_str("\\\""),
                    '\\' => o.push_str("\\\\"),
                    '\n' => o.push_str("\\n"),
                    '\r' => o.push_str("\\r"),
                    '\t' => o.push_str("\\t"),
                    c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
                    c => o.push(c),
                }
            }
            o.push('"');
            o
        };
        let items: Vec<String> = cases
            .iter()
            .map(|(k, v)| format!("{}:{}", quote(k), quote(v)))
            .collect();
        format!("{{{}}}", items.join(","))
    }
}
