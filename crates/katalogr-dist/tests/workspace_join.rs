//! FEAT-020 T4: peers joined into `refs`, `callers` and `dangling`. Needs the test extractor:
//! `cargo test -p katalogr-dist --features test-extractor`.
#![cfg(feature = "test-extractor")]

#[path = "support/ws.rs"]
mod ws;

mod workspace {
    use super::ws::*;
    use katalogr_dist::query::{PeerEnd, WorkspaceBlock};
    use katalogr_dist::{Katalogr, Options, Outcome, PEERS_SETTING, PeerSpec, Reply, ViewQuery};
    use katalogr_store::{Cache, PendingKey};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    const CONTRACT: &str = "svc:load";

    /// `orders` consumes `svc:load` (symbol `call`); `billing` provides it (symbol `serve`).
    fn pair() -> (World, PathBuf) {
        let w = World::new();
        let kx = w.kindex("kx");
        w.project(&kx, "orders", &[("r", &[("c.tx", &consumer(CONTRACT))])]);
        w.project(&kx, "billing", &[("r", &[("p.tx", &provider(CONTRACT))])]);
        (w, kx)
    }

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

    fn found<T>(r: &Reply<T>) -> &T {
        match &r.outcome {
            Outcome::Found(v) => v,
            _ => panic!("expected an answer"),
        }
    }

    fn block<T>(r: &Reply<T>) -> &WorkspaceBlock {
        r.workspace.as_ref().expect("a workspace block")
    }

    fn names(ends: &[PeerEnd]) -> Vec<(String, String, bool)> {
        ends.iter()
            .map(|e| {
                (
                    e.role.clone(),
                    e.hit.as_ref().map(|h| h.name.clone()).unwrap_or_default(),
                    e.opaque,
                )
            })
            .collect()
    }

    fn view(target: &str) -> Options {
        Options {
            view: Some(ViewQuery {
                op: "refs".into(),
                args: BTreeMap::from([("target".to_string(), target.to_string())]),
            }),
            ..Options::default()
        }
    }

    #[test]
    fn peer_contract_links_both_ways() {
        let (_w, kx) = pair();
        let orders = name(&kx, "orders", &["billing"]);
        let r = orders.refs("call", &Options::default()).unwrap();
        let b = block(&r);
        assert_eq!(b.peers.len(), 1);
        assert_eq!(b.peers[0].state, "ok");
        assert_eq!(
            names(&b.ends),
            vec![("provides".into(), "r:p.tx#fn:serve".into(), false)]
        );
        assert_eq!(b.ends[0].link, "contract:svc:load");
        assert_eq!(b.ends[0].project, "billing");
        // The reverse: billing names orders and reads from its own provider.
        let billing = name(&kx, "billing", &["orders"]);
        let r = billing.refs("serve", &Options::default()).unwrap();
        assert_eq!(
            names(&block(&r).ends),
            vec![("consumes".into(), "r:c.tx#fn:call".into(), false)]
        );
        assert!(r.report.warnings.is_empty(), "{:?}", r.report.warnings);
        assert!(r.partial.is_empty());
    }

    #[test]
    fn callers_cross_into_peer() {
        let (_w, kx) = pair();
        let billing = name(&kx, "billing", &["orders"]);
        let r = billing.callers("serve", 2, &Options::default()).unwrap();
        assert_eq!(
            names(&block(&r).ends),
            vec![("consumes".into(), "r:c.tx#fn:call".into(), false)]
        );
    }

    #[test]
    fn naming_is_one_way() {
        let (_w, kx) = pair();
        let _orders = name(&kx, "orders", &["billing"]);
        let billing = open(&kx, "billing");
        let r = billing.refs("serve", &Options::default()).unwrap();
        assert!(r.workspace.is_none());
        let r = billing.callers("serve", 2, &Options::default()).unwrap();
        assert!(r.workspace.is_none());
    }

    #[test]
    fn no_peers_flag_reads_local_only() {
        let (_w, kx) = pair();
        let orders = name(&kx, "orders", &["billing"]);
        let o = Options {
            no_peers: true,
            ..Options::default()
        };
        let r = orders.refs("call", &o).unwrap();
        assert!(r.workspace.is_none());
        assert!(r.partial.is_empty() && r.report.warnings.is_empty());
        let r = orders.dangling(&["contract"], &o).unwrap();
        assert!(r.workspace.is_none());
        assert_eq!(found(&r).len(), 1);
    }

    #[test]
    fn dangling_resolved_by_peer() {
        let (_w, kx) = pair();
        let orders = name(&kx, "orders", &["billing"]);
        let r = orders.dangling(&["contract"], &Options::default()).unwrap();
        assert!(found(&r).is_empty(), "{:?}", found(&r));
        let b = block(&r);
        assert_eq!(b.resolved.len(), 1);
        assert_eq!(b.resolved[0].reference.kind, "consumes");
        assert_eq!(
            names(&b.resolved[0].ends),
            vec![("provides".into(), "r:p.tx#fn:serve".into(), false)]
        );
    }

    #[test]
    fn unresolved_export_key_resolved_by_peer() {
        let w = World::new();
        let kx = w.kindex("kx");
        w.project(
            &kx,
            "orders",
            &[("r", &[("a.tx", "sym fn user\nref user links-to pkg:lib\n")])],
        );
        w.project(
            &kx,
            "billing",
            &[("r", &[("b.tx", "sym fn lib\nexport pkg:lib lib\n")])],
        );
        let alone = open(&kx, "orders")
            .dangling(&["links-to"], &Options::default())
            .unwrap();
        assert_eq!(found(&alone).len(), 1);
        let orders = name(&kx, "orders", &["billing"]);
        let r = orders.dangling(&["links-to"], &Options::default()).unwrap();
        assert!(found(&r).is_empty());
        let b = block(&r);
        assert_eq!(b.resolved.len(), 1);
        assert_eq!(b.resolved[0].ends[0].link, "pkg:lib");
        assert_eq!(b.resolved[0].ends[0].role, "exports");
    }

    #[test]
    fn external_node_matches_peer_export() {
        let w = World::new();
        let kx = w.kindex("kx");
        w.project(
            &kx,
            "orders",
            &[(
                "r",
                &[("a.tx", "sym package_external lib\nexport pkg:lib lib\n")],
            )],
        );
        w.project(
            &kx,
            "billing",
            &[(
                "r",
                &[(
                    "b.tx",
                    "sym fn real\nexport pkg:lib real\nsym package_external other\nexport pkg:lib other\n",
                )],
            )],
        );
        let orders = name(&kx, "orders", &["billing"]);
        let r = orders.refs("lib", &Options::default()).unwrap();
        // The peer's external node is no match; the node that really exports the key is.
        assert_eq!(
            names(&block(&r).ends),
            vec![("exports".into(), "r:b.tx#fn:real".into(), false)]
        );
        assert_eq!(block(&r).ends[0].link, "pkg:lib");
    }

    /// Billing with a readable repo `r1` and a repo `r2` whose root is gone from this machine.
    fn restricted() -> (World, PathBuf) {
        let w = World::new();
        let kx = w.kindex("kx");
        w.project(&kx, "orders", &[("r", &[("c.tx", &consumer(CONTRACT))])]);
        w.project(
            &kx,
            "billing",
            &[
                ("r1", &[("q.tx", "sym fn other\n")]),
                (
                    "r2",
                    &[(
                        "secret.tx",
                        "sym fn hidden_serve\nbind hidden_serve provides svc:load\n",
                    )],
                ),
            ],
        );
        std::fs::write(
            kx.join("projects/billing/.cache/roots.yaml"),
            format!("r1: {}\n", w.repo_dir(&kx, "billing", "r1").display()),
        )
        .unwrap();
        (w, kx)
    }

    #[test]
    fn unreadable_repo_is_opaque() {
        let (_w, kx) = restricted();
        let orders = name(&kx, "orders", &["billing"]);
        let r = orders.refs("call", &Options::default()).unwrap();
        let ends = &block(&r).ends;
        assert_eq!(ends.len(), 1);
        assert!(ends[0].opaque && ends[0].hit.is_none());
        assert_eq!(ends[0].role, "provides");
        assert_eq!(ends[0].link, "contract:svc:load");
        assert_eq!(ends[0].project, "billing");
    }

    #[test]
    fn opaque_hides_details() {
        let (_w, kx) = restricted();
        let orders = name(&kx, "orders", &["billing"]);
        let r = orders.refs("call", &Options::default()).unwrap();
        let json = serde_json::to_string(block(&r)).unwrap();
        for hidden in ["r2", "secret", "hidden_serve", ".tx#"] {
            assert!(!json.contains(hidden), "{hidden} in {json}");
        }
        assert!(json.contains("\"opaque\":true"), "{json}");
    }

    #[test]
    fn positions_stay_local() {
        let (_w, kx) = pair();
        let orders = name(&kx, "orders", &["billing"]);
        let r = orders.refs("call", &Options::default()).unwrap();
        // Both projects have a repo named `r`, at different commits.
        assert_eq!(r.report.positions.len(), 1);
        let local = &r.report.positions[0];
        let peer = &block(&r).peers[0].positions;
        assert_eq!(peer.len(), 1);
        assert_eq!(peer[0].repo, "r");
        assert_ne!(peer[0].commit, local.commit);
    }

    #[test]
    fn view_id_changes_with_peer_position() {
        let (w, kx) = pair();
        let id = |k: &Katalogr| {
            k.refs("call", &view("call"))
                .unwrap()
                .report
                .view_id
                .expect("an exact answer has an id")
        };
        let orders = name(&kx, "orders", &["billing"]);
        let first = id(&orders);
        assert_eq!(first, id(&orders), "the same peers give the same id");
        w.commit(
            &kx,
            "billing",
            "r",
            &[(
                "p.tx",
                "sym fn serve\nsym fn more\nbind serve provides svc:load\n",
            )],
        );
        w.index(&kx, "billing");
        assert_ne!(first, id(&open(&kx, "orders")));
    }

    #[test]
    fn view_id_unchanged_without_peers() {
        let (_w, kx) = pair();
        let lone = open(&kx, "orders");
        let alone = lone.refs("call", &view("call")).unwrap().report.view_id;
        assert!(alone.is_some());
        let orders = name(&kx, "orders", &["billing"]);
        let with = orders.refs("call", &view("call")).unwrap().report.view_id;
        assert_ne!(with, alone, "reading a peer is part of the question");
        let off = Options {
            no_peers: true,
            ..view("call")
        };
        assert_eq!(orders.refs("call", &off).unwrap().report.view_id, alone);
    }

    #[test]
    fn stale_peer_not_exact() {
        let (_w, kx) = pair();
        let orders = name(&kx, "orders", &["billing"]);
        let clean = orders.refs("call", &view("call")).unwrap();
        assert!(clean.report.exact && clean.report.view_id.is_some());
        {
            let billing = open(&kx, "billing");
            let w =
                Cache::open(billing.project(), Instant::now() + Duration::from_secs(5)).unwrap();
            let mut tx = w.write().unwrap();
            tx.put_pending(
                &PendingKey {
                    context: "worktree".into(),
                    repo: "r".into(),
                    path: katalogr_model::RelPath::new("p.tx").unwrap(),
                },
                1_000_000,
            )
            .unwrap();
            tx.commit().unwrap();
        }
        let r = orders.refs("call", &view("call")).unwrap();
        assert!(block(&r).peers[0].stale);
        assert!(!r.report.exact);
        assert!(r.report.view_id.is_none());
    }

    #[test]
    fn answer_independent_of_peer_order() {
        let w = World::new();
        let kx = w.kindex("kx");
        w.project(&kx, "orders", &[("r", &[("c.tx", &consumer(CONTRACT))])]);
        w.project(&kx, "billing", &[("r", &[("p.tx", &provider(CONTRACT))])]);
        w.project(&kx, "stock", &[("r", &[("p.tx", &provider(CONTRACT))])]);
        let one = name(&kx, "orders", &["billing", "stock"])
            .refs("call", &view("call"))
            .unwrap();
        let two = name(&kx, "orders", &["stock", "billing"])
            .refs("call", &view("call"))
            .unwrap();
        assert_eq!(block(&one).ends.len(), 2);
        assert_eq!(
            serde_json::to_string(block(&one)).unwrap(),
            serde_json::to_string(block(&two)).unwrap()
        );
        assert_eq!(one.report.view_id, two.report.view_id);
        assert!(one.report.view_id.is_some());
    }
}
