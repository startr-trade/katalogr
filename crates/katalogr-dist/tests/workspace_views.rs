//! FEAT-020 T7: the `contracts` and `impact` views draw peers' ends, and a snapshot names their
//! repos. Needs the test extractor: `cargo test -p katalogr-dist --features test-extractor`.
#![cfg(feature = "test-extractor")]

#[path = "support/ws.rs"]
mod ws;

mod workspace {
    use super::ws::*;
    use katalogr_dist::doc_views::SnapshotRequest;
    use katalogr_dist::render::{ViewGraph, ViewSpec};
    use katalogr_dist::{Katalogr, Options, Outcome, PEERS_SETTING, PeerSpec};
    use std::path::{Path, PathBuf};

    fn name(k: &Path, project: &str, peers: &[&str]) -> Katalogr {
        let specs: Vec<PeerSpec> = peers
            .iter()
            .map(|p| PeerSpec {
                kindex: None,
                project: p.to_string(),
            })
            .collect();
        open(k, project)
            .project()
            .set_setting(PEERS_SETTING, serde_yaml_ng::to_value(&specs).unwrap())
            .unwrap();
        open(k, project)
    }

    /// `core` provides `svc:load`; `app` consumes it in `call` (repo `r1`), which `handler`
    /// calls; repo `r2` of `app` has another consumer, but its root is not on this machine.
    fn world() -> (World, PathBuf) {
        let w = World::new();
        let kx = w.kindex("kx");
        w.project(&kx, "core", &[("r", &[("p.tx", &provider("svc:load"))])]);
        w.project(
            &kx,
            "app",
            &[
                (
                    "r1",
                    &[(
                        "c.tx",
                        "sym fn call\nbind call consumes svc:load\nsym fn handler\nlink handler calls call\n",
                    )],
                ),
                ("r2", &[("s.tx", "sym fn hidden\nbind hidden consumes svc:load\n")]),
            ],
        );
        std::fs::write(
            kx.join("projects/app/.cache/roots.yaml"),
            format!("r1: {}\n", w.repo_dir(&kx, "app", "r1").display()),
        )
        .unwrap();
        (w, kx)
    }

    fn graph(k: &Katalogr, line: &str) -> ViewGraph {
        let spec = ViewSpec::parse(line).unwrap();
        match k.view_graph(&spec, &Options::default()).unwrap().outcome {
            Outcome::Found(g) => g,
            other => panic!("{line}: {other:?}"),
        }
    }

    fn node<'a>(g: &'a ViewGraph, name: &str) -> &'a katalogr_dist::render::ViewNode {
        g.nodes
            .iter()
            .find(|n| n.name == name)
            .unwrap_or_else(|| panic!("no node {name} in {:?}", g.nodes))
    }

    #[test]
    fn contracts_view_shows_peer_ends() {
        let (_w, kx) = world();
        let core = name(&kx, "core", &["app"]);
        let g = graph(&core, "contracts");
        let call = node(&g, "app//r1:c.tx#fn:call");
        assert_eq!(call.group.as_deref(), Some("app//r1"));
        // The restricted end is one hidden node of the project.
        let hidden = node(&g, "app//hidden");
        assert_eq!(
            (hidden.kind.as_str(), hidden.label.as_str()),
            ("opaque", "hidden")
        );
        assert!(
            g.edges
                .iter()
                .any(|e| e.from == call.id && e.kind == "consumes")
        );
        assert!(
            g.nodes
                .iter()
                .all(|n| !n.name.contains("r2") && !n.name.contains("secret"))
        );
        // The local end is still drawn under its own name.
        assert!(
            g.nodes
                .iter()
                .any(|n| n.name == "core:p.tx#fn:serve" || n.name == "r:p.tx#fn:serve")
        );
        // Without peers the view is the old one.
        let alone = graph(&open(&kx, "app"), "contracts");
        assert!(alone.nodes.iter().all(|n| !n.name.contains("//")));
    }

    #[test]
    fn impact_view_shows_peer_hits() {
        let (_w, kx) = world();
        let core = name(&kx, "core", &["app"]);
        let g = graph(&core, "impact serve");
        let call = node(&g, "app//r1:c.tx#fn:call");
        let handler = node(&g, "app//r1:c.tx#fn:handler");
        assert_eq!(handler.group.as_deref(), Some("app//r1"));
        // `call` joins the contract it came through; `handler` joins `call`.
        assert!(
            g.edges
                .iter()
                .any(|e| e.from == call.id && e.kind == "consumes")
        );
        assert!(
            g.edges
                .iter()
                .any(|e| e.from == handler.id && e.to == call.id)
        );
        node(&g, "app//hidden");
    }

    #[test]
    fn snapshot_names_peer_repos() {
        let (w, kx) = world();
        let core = name(&kx, "core", &["app"]);
        let docs = w.root.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("a.md"),
            "# T\n\n```katalogr-query\ncontracts\n```\n",
        )
        .unwrap();
        let out = w.root.join("snap");
        let req = SnapshotRequest {
            docs,
            out: out.clone(),
            options: Options::default(),
        };
        let idx = core.snapshot(&req).unwrap();
        // The repo of the peer that is read is named; the one that is not, is not.
        assert!(
            idx.repos.contains(&"app//r1".to_string()),
            "{:?}",
            idx.repos
        );
        assert!(
            !idx.repos.iter().any(|r| r.contains("r2")),
            "{:?}",
            idx.repos
        );
        let text = std::fs::read_to_string(out.join("index.json")).unwrap();
        assert!(text.contains("app//r1") && !text.contains("r2"), "{text}");
        // The stored view says which peer positions it reflects.
        let body =
            std::fs::read_to_string(out.join(format!("{}.json", idx.blocks[0].view_id))).unwrap();
        assert!(body.contains("\"peers\":[{\"project\":\"app\""), "{body}");
    }
}
