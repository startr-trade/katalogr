//! FEAT-020 T3: peers, their locations and how each opens. Needs the test extractor:
//! `cargo test -p katalogr-dist --features test-extractor`.
#![cfg(feature = "test-extractor")]

#[path = "support/ws.rs"]
mod ws;

mod workspace {
    use super::ws::*;
    use katalogr_dist::{Options, PeerSpec};
    use katalogr_store::{Cache, Reader};
    use std::time::{Duration, Instant};

    fn spec(kindex: Option<&str>, project: &str) -> PeerSpec {
        PeerSpec {
            kindex: kindex.map(str::to_string),
            project: project.to_string(),
        }
    }

    /// Two projects of one kindex: `a` provides, `b` consumes.
    fn pair() -> (World, std::path::PathBuf) {
        let w = World::new();
        let kx = w.kindex("kx");
        w.project(&kx, "a", &[("r", &[("p.tx", &provider("svc:load"))])]);
        w.project(&kx, "b", &[("r", &[("c.tx", &consumer("svc:load"))])]);
        (w, kx)
    }

    #[test]
    fn projects_isolated_without_peer() {
        let (_w, kx) = pair();
        let b = open(&kx, "b");
        assert!(b.peers().unwrap().is_empty());
        assert!(b.workspace_status(&Options::default()).unwrap().is_empty());
        // Without a peer, b's consumer has no provider: the contract is dangling.
        let r = b.dangling(&["contract"], &Options::default()).unwrap();
        let katalogr_dist::Outcome::Found(d) = r.outcome else {
            panic!("dangling answers")
        };
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].kind, "consumes");
        assert!(d[0].from.name.contains(":c.tx#"), "{:?}", d[0].from.name);
    }

    #[test]
    fn repos_of_one_project_link_without_peers() {
        let w = World::new();
        let kx = w.kindex("kx");
        w.project(
            &kx,
            "a",
            &[
                ("r1", &[("p.tx", &provider("svc:load"))]),
                ("r2", &[("c.tx", &consumer("svc:load"))]),
            ],
        );
        let a = open(&kx, "a");
        let r = a.dangling(&["contract"], &Options::default()).unwrap();
        let katalogr_dist::Outcome::Found(d) = r.outcome else {
            // No dangling entry: the repos' ends are joined.
            return;
        };
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn peer_in_other_kindex_via_location() {
        let w = World::new();
        let kx1 = w.kindex("kx1");
        let kx2 = w.kindex("kx2");
        w.project(&kx1, "orders", &[("r", &[("c.tx", &consumer("svc:load"))])]);
        w.project(
            &kx2,
            "billing",
            &[("r", &[("p.tx", &provider("svc:load"))])],
        );
        let orders = open(&kx1, "orders");
        orders.workspace_add("billing", Some("shared")).unwrap();
        let orders = open(&kx1, "orders");
        assert_eq!(
            orders.peers().unwrap(),
            vec![spec(Some("shared"), "billing")]
        );
        let st = orders.workspace_status(&Options::default()).unwrap();
        assert_eq!(st[0].state, "no-location");
        assert_eq!(
            st[0].fix.as_deref(),
            Some("katalogr workspace locate shared <dir>")
        );
        orders.workspace_locate("shared", &kx2).unwrap();
        let st = orders.workspace_status(&Options::default()).unwrap();
        assert_eq!(st[0].state, "ok", "{:?}", st[0]);
        assert_eq!(st[0].kindex.as_deref(), Some("shared"));
        assert_eq!(st[0].positions.len(), 1);
        assert!(!st[0].stale);
        // A directory with no kindex is a usage error.
        let err = orders.workspace_locate("other", &w.root).unwrap_err();
        assert!(err.is_usage(), "{err}");
        // The location is on this machine only: the kindex holds the alias alone.
        let cfg = std::fs::read_to_string(kx1.join("projects/orders/config.yaml")).unwrap();
        assert!(
            cfg.contains("shared") && !cfg.contains(kx2.to_str().unwrap()),
            "{cfg}"
        );
    }

    #[test]
    fn peer_read_writes_nothing() {
        let (_w, kx) = pair();
        let b = open(&kx, "b");
        b.workspace_add("a", None).unwrap();
        let b = open(&kx, "b");
        let before = hash_tree(&kx.join("projects/a"));
        let st = b.workspace_status(&Options::default()).unwrap();
        assert_eq!(st[0].state, "ok");
        // A read that joins the peer writes nothing into it either.
        let r = b.refs("call", &Options::default()).unwrap();
        assert!(r.workspace.is_some());
        assert_eq!(hash_tree(&kx.join("projects/a")), before);
    }

    #[test]
    fn peer_read_beside_its_writer() {
        let (_w, kx) = pair();
        let b = open(&kx, "b");
        b.workspace_add("a", None).unwrap();
        let b = open(&kx, "b");
        // Hold the peer's cache open as a writer would.
        let a = open(&kx, "a");
        let _writer = Cache::open(a.project(), Instant::now() + Duration::from_secs(10)).unwrap();
        let st = b.workspace_status(&Options::default()).unwrap();
        assert_eq!(st[0].state, "ok", "{:?}", st[0]);
    }

    #[test]
    fn peer_read_needs_no_shards() {
        let (_w, kx) = pair();
        assert!(!kx.join("projects/a/shards").exists());
        let b = open(&kx, "b");
        b.workspace_add("a", None).unwrap();
        let st = open(&kx, "b")
            .workspace_status(&Options::default())
            .unwrap();
        assert_eq!(st[0].state, "ok");
    }

    #[test]
    fn peer_read_needs_no_checkout() {
        let (w, kx) = pair();
        open(&kx, "b").workspace_add("a", None).unwrap();
        std::fs::remove_dir_all(w.repo_dir(&kx, "a", "r")).unwrap();
        let st = open(&kx, "b")
            .workspace_status(&Options::default())
            .unwrap();
        assert_eq!(st[0].state, "ok", "{:?}", st[0]);
        // The rows answer; the repo can no longer be read, so its position shows nowhere.
        assert!(st[0].positions.is_empty());
        assert!(st[0].repos_absent.is_empty());
    }

    #[test]
    fn missing_peer_warns_and_answers() {
        let (_w, kx) = pair();
        open(&kx, "b").workspace_add("ghost", None).unwrap();
        let st = open(&kx, "b")
            .workspace_status(&Options::default())
            .unwrap();
        assert_eq!(st[0].state, "not-found");
        assert!(st[0].fix.is_none());
        // The read answers from what it has: local result, warning, partial and not exact.
        let r = open(&kx, "b").refs("call", &Options::default()).unwrap();
        assert!(matches!(r.outcome, katalogr_dist::Outcome::Found(_)));
        assert!(
            r.report.warnings.iter().any(|w| w.contains("ghost")),
            "{:?}",
            r.report.warnings
        );
        assert!(!r.report.exact);
        assert_eq!(r.partial.len(), 1);
        assert_eq!(r.partial[0].cap, "peers");
        assert_eq!(r.workspace.unwrap().peers[0].state, "not-found");
    }

    #[test]
    fn no_index_names_command() {
        let (_w, kx) = pair();
        open(&kx, "b").workspace_add("a", None).unwrap();
        std::fs::remove_file(kx.join("projects/a/.cache/index.redb")).unwrap();
        let st = open(&kx, "b")
            .workspace_status(&Options::default())
            .unwrap();
        assert_eq!(st[0].state, "no-index");
        let fix = st[0].fix.as_deref().unwrap();
        assert!(fix.contains("--project a index --full"), "{fix}");
        assert!(!fix.contains(kx.to_str().unwrap()), "{fix}");
        let r = open(&kx, "b").refs("call", &Options::default()).unwrap();
        assert!(
            r.report.warnings.iter().any(|w| w.contains("index --full")),
            "{:?}",
            r.report.warnings
        );
        assert!(!r.report.exact);
    }

    #[test]
    fn other_schema_peer_not_read() {
        let (_w, kx) = pair();
        open(&kx, "b").workspace_add("a", None).unwrap();
        {
            let a = open(&kx, "a");
            let cache = Cache::open(a.project(), Instant::now() + Duration::from_secs(10)).unwrap();
            let mut tx = cache.write().unwrap();
            tx.set_meta("cache_schema", &0u32.to_le_bytes()).unwrap();
            tx.commit().unwrap();
            let _ = cache.read().unwrap().meta("cache_schema");
        }
        let st = open(&kx, "b")
            .workspace_status(&Options::default())
            .unwrap();
        assert_eq!(st[0].state, "unavailable", "{:?}", st[0]);
        assert!(st[0].fix.is_some());
    }

    #[test]
    fn rootless_repo_listed_absent() {
        let w = World::new();
        let kx = w.kindex("kx");
        w.project(
            &kx,
            "a",
            &[
                ("r1", &[("p.tx", &provider("svc:load"))]),
                ("r2", &[("q.tx", "sym fn other\n")]),
            ],
        );
        w.project(&kx, "b", &[("r", &[("c.tx", &consumer("svc:load"))])]);
        // A second machine that never cloned r2: forget its root and index again from scratch.
        let _ = std::fs::remove_file(kx.join("projects/a/.cache/index.redb"));
        std::fs::write(
            kx.join("projects/a/.cache/roots.yaml"),
            format!("r1: {}\n", w.repo_dir(&kx, "a", "r1").display()),
        )
        .unwrap();
        w.index(&kx, "a");
        open(&kx, "b").workspace_add("a", None).unwrap();
        let st = open(&kx, "b")
            .workspace_status(&Options::default())
            .unwrap();
        assert_eq!(st[0].state, "ok", "{:?}", st[0]);
        assert_eq!(st[0].repos_absent, vec!["r2".to_string()]);
    }
}
