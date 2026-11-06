//! FEAT-020 T5: `impact` continues into peers from the contracts it reached. Needs the test
//! extractor: `cargo test -p katalogr-dist --features test-extractor`.
#![cfg(feature = "test-extractor")]

#[path = "support/ws.rs"]
mod ws;

mod workspace {
    use super::ws::*;
    use katalogr_dist::query::{ImpactOptions, ImpactReport};
    use katalogr_dist::{ImpactInput, Katalogr, Options, Outcome, PEERS_SETTING, PeerSpec, Reply};
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

    fn run(k: &Katalogr, text: &str, opts: &Options) -> Reply<ImpactReport> {
        k.impact(&ImpactInput::Symbol(text), &ImpactOptions::default(), opts)
            .unwrap()
    }

    fn report(r: &Reply<ImpactReport>) -> &ImpactReport {
        match &r.outcome {
            Outcome::Found(x) => x,
            _ => panic!("expected a report"),
        }
    }

    fn hit_names(p: &katalogr_dist::query::PeerImpact) -> Vec<String> {
        p.groups
            .iter()
            .flat_map(|g| g.hits.iter().map(|h| h.node.name.clone()))
            .collect()
    }

    /// `core` provides `svc:load` (`serve`); `app` consumes it in `call`, which `handler` calls.
    fn two() -> (World, PathBuf) {
        let w = World::new();
        let kx = w.kindex("kx");
        w.project(&kx, "core", &[("r", &[("p.tx", &provider("svc:load"))])]);
        w.project(
            &kx,
            "app",
            &[(
                "r",
                &[(
                    "c.tx",
                    "sym fn call\nbind call consumes svc:load\nsym fn handler\nlink handler calls call\n",
                )],
            )],
        );
        (w, kx)
    }

    #[test]
    fn impact_crosses_peer() {
        let (_w, kx) = two();
        let core = name(&kx, "core", &["app"]);
        let r = run(&core, "serve", &Options::default());
        let rep = report(&r);
        // The local groups hold nothing of the peer.
        assert!(
            rep.groups.iter().all(|g| g.hits.is_empty()),
            "{:?}",
            rep.groups
        );
        assert_eq!(rep.projects, ["app", "core"]);
        assert_eq!(rep.peers.len(), 1);
        let p = &rep.peers[0];
        assert_eq!(p.project, "app");
        let mut names = hit_names(p);
        names.sort();
        assert_eq!(names, ["r:c.tx#fn:call", "r:c.tx#fn:handler"]);
        let depth = |n: &str| {
            p.groups
                .iter()
                .flat_map(|g| &g.hits)
                .find(|h| h.node.name.ends_with(n))
                .unwrap()
                .depth
        };
        assert_eq!((depth("call"), depth("handler")), (1, 2));
        assert!(
            p.contracts
                .iter()
                .any(|c| c.contract == "contract:svc:load" && c.consumers == 1)
        );
        assert!(r.workspace.is_some());
    }

    /// `n` projects `p0..`: `p0` provides `c1`; `p<i>` consumes `c<i>` in `q<i>` and, but for the
    /// last, provides `c<i+1>` there.
    fn chain(n: usize) -> (World, PathBuf) {
        let w = World::new();
        let kx = w.kindex("kx");
        w.project(
            &kx,
            "p0",
            &[("r", &[("a.tx", "sym fn q0\nbind q0 provides svc:c1\n")])],
        );
        for i in 1..n {
            let mut text = format!("sym fn q{i}\nbind q{i} consumes svc:c{i}\n");
            if i + 1 < n {
                text.push_str(&format!("bind q{i} provides svc:c{}\n", i + 1));
            }
            w.project(&kx, &format!("p{i}"), &[("r", &[("a.tx", &text)])]);
        }
        (w, kx)
    }

    #[test]
    fn impact_rounds_bounded() {
        // Five projects: the last is four contracts away, one round too far.
        let (_w, kx) = chain(5);
        let p0 = name(&kx, "p0", &["p1", "p2", "p3", "p4"]);
        let r = run(&p0, "q0", &Options::default());
        let rep = report(&r);
        // `p4` was read in the last round with nothing to find: the contract it consumes was
        // reached in that round, which is one past the bound.
        let found: Vec<&str> = rep
            .peers
            .iter()
            .filter(|p| !p.groups.is_empty())
            .map(|p| p.project.as_str())
            .collect();
        assert_eq!(found, ["p1", "p2", "p3"]);
        assert!(
            r.partial
                .iter()
                .any(|p| p.cap == "peers" && p.at == 3 && p.shown == 3),
            "{:?}",
            r.partial
        );
        // Four projects fit in the three rounds: nothing is left over.
        let (_w2, kx2) = chain(4);
        let p0 = name(&kx2, "p0", &["p1", "p2", "p3"]);
        let r = run(&p0, "q0", &Options::default());
        assert_eq!(report(&r).peers.len(), 3);
        assert!(
            r.partial.iter().all(|p| p.cap != "peers"),
            "{:?}",
            r.partial
        );
    }

    #[test]
    fn impact_unchanged_without_peers() {
        let (_w, kx) = two();
        let lone = serde_json::to_string(report(&run(
            &open(&kx, "core"),
            "serve",
            &Options::default(),
        )))
        .unwrap();
        let core = name(&kx, "core", &["app"]);
        let off = Options {
            no_peers: true,
            ..Options::default()
        };
        let r = run(&core, "serve", &off);
        assert!(r.workspace.is_none());
        assert_eq!(serde_json::to_string(report(&r)).unwrap(), lone);
        // An empty list is no setting.
        let empty = name(&kx, "core", &[]);
        let r = run(&empty, "serve", &Options::default());
        assert_eq!(serde_json::to_string(report(&r)).unwrap(), lone);
        assert!(!lone.contains("\"peers\""), "{lone}");
    }

    #[test]
    fn chain_drops_restricted_names() {
        let w = World::new();
        let kx = w.kindex("kx");
        w.project(&kx, "core", &[("r", &[("p.tx", &provider("svc:load"))])]);
        w.project(
            &kx,
            "app",
            &[
                (
                    "r1",
                    &[("v.tx", "sym fn viewer\nref viewer calls code:xcall\n")],
                ),
                (
                    "r2",
                    &[(
                        "x.tx",
                        "sym fn xcall\nbind xcall consumes svc:load\nexport code:xcall xcall\n",
                    )],
                ),
            ],
        );
        std::fs::write(
            kx.join("projects/app/.cache/roots.yaml"),
            format!("r1: {}\n", w.repo_dir(&kx, "app", "r1").display()),
        )
        .unwrap();
        let core = name(&kx, "core", &["app"]);
        let r = run(&core, "serve", &Options::default());
        let rep = report(&r);
        let p = &rep.peers[0];
        assert_eq!(hit_names(p), ["r1:v.tx#fn:viewer"]);
        let viewer = &p.groups[0].hits[0];
        assert!(viewer.chain.is_empty(), "{:?}", viewer.chain);
        assert_eq!(p.opaque.len(), 1);
        assert_eq!(p.opaque[0].link, "contract:svc:load");
        let json = serde_json::to_string(rep).unwrap();
        for hidden in ["xcall", "x.tx", "r2"] {
            assert!(!json.contains(hidden), "{hidden} in {json}");
        }
    }
}
