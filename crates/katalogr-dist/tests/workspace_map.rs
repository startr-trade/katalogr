//! FEAT-020 T6: the project-level map, and that restricted ends change no byte of an aggregate.
//! Needs the test extractor: `cargo test -p katalogr-dist --features test-extractor`.
#![cfg(feature = "test-extractor")]

#[path = "support/ws.rs"]
mod ws;

mod workspace {
    use super::ws::*;
    use katalogr_dist::query::ImpactOptions;
    use katalogr_dist::{ImpactInput, Katalogr, Options, PEERS_SETTING, PeerSpec};
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

    /// `core` provides `svc:load`; `app` has a readable repo `r1` that consumes it and a repo
    /// `r2` whose root is not on this machine.
    fn world() -> (World, PathBuf) {
        let w = World::new();
        let kx = w.kindex("kx");
        w.project(&kx, "core", &[("r", &[("p.tx", &provider("svc:load"))])]);
        w.project(
            &kx,
            "app",
            &[
                ("r1", &[("c.tx", &consumer("svc:load"))]),
                (
                    "r2",
                    &[("s.tx", "sym fn hidden\nbind hidden consumes svc:load\n")],
                ),
            ],
        );
        restrict(&w, &kx);
        (w, kx)
    }

    fn restrict(w: &World, kx: &Path) {
        std::fs::write(
            kx.join("projects/app/.cache/roots.yaml"),
            format!("r1: {}\n", w.repo_dir(kx, "app", "r1").display()),
        )
        .unwrap();
    }

    fn unrestrict(w: &World, kx: &Path) {
        std::fs::write(
            kx.join("projects/app/.cache/roots.yaml"),
            format!(
                "r1: {}\nr2: {}\n",
                w.repo_dir(kx, "app", "r1").display(),
                w.repo_dir(kx, "app", "r2").display()
            ),
        )
        .unwrap();
    }

    /// The answers that aggregate across projects, as bytes.
    fn aggregates(k: &Katalogr) -> String {
        let m = k.workspace_map(&Options::default()).unwrap();
        let i = k
            .impact(
                &ImpactInput::Symbol("serve"),
                &ImpactOptions::default(),
                &Options::default(),
            )
            .unwrap();
        let (katalogr_dist::Outcome::Found(map), katalogr_dist::Outcome::Found(imp)) =
            (&m.outcome, &i.outcome)
        else {
            panic!("answers")
        };
        serde_json::to_string(&(map, &m.workspace, &m.report.view_id, imp, &i.workspace)).unwrap()
    }

    #[test]
    fn aggregates_leak_nothing() {
        let (w, kx) = world();
        let core = name(&kx, "core", &["app"]);
        let before = aggregates(&core);
        // The map counts the readable end only.
        let m = core.workspace_map(&Options::default()).unwrap();
        let katalogr_dist::Outcome::Found(map) = &m.outcome else {
            panic!("map")
        };
        assert_eq!(map.projects, ["app", "core"]);
        assert_eq!(map.edges.len(), 1);
        assert_eq!(
            (
                map.edges[0].consumer.as_str(),
                map.edges[0].provider.as_str()
            ),
            ("app", "core")
        );
        assert_eq!(map.edges[0].contracts, 1);
        // The restricted repo gains, loses and renames ends; the aggregates keep their bytes.
        unrestrict(&w, &kx);
        w.commit(
            &kx,
            "app",
            "r2",
            &[
                ("s.tx", "sym fn renamed\nbind renamed consumes svc:load\nbind renamed provides svc:other\n"),
                ("t.tx", "sym fn more\nbind more consumes svc:load\n"),
            ],
        );
        w.index(&kx, "app");
        restrict(&w, &kx);
        let after = aggregates(&open(&kx, "core"));
        assert_eq!(before, after);
        for hidden in ["hidden", "renamed", "svc:other", "r2"] {
            assert!(!after.contains(hidden), "{hidden} in {after}");
        }
    }
}
