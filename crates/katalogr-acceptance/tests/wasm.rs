//! FEAT-016: the WASM bridge, indexed end to end over the `fixtures/wasm` cases with every
//! shipped extension registered.

mod wasm {
    use camino::{Utf8Path, Utf8PathBuf};
    use katalogr_acceptance::{ParsedGraph, REPO, full_registry, parse_graph};
    use katalogr_engine::{Consistency, Engine, NewEvent, QueryOptions, Scope};
    use katalogr_query::DanglingRef;
    use katalogr_store::{Kindex, ProjectConfig, ProjectName, RepoConfig};
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    struct Indexed {
        text: String,
        graph: ParsedGraph,
        dangling: Vec<DanglingRef>,
    }

    fn case_dir(case: &str) -> Utf8PathBuf {
        Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/wasm")
            .join(case)
            .canonicalize_utf8()
            .unwrap()
    }

    fn index(case: &str) -> Indexed {
        let tmp = tempfile::tempdir().unwrap();
        let kx = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let dir = Kindex::init(&kx)
            .unwrap()
            .create_project(
                &ProjectName::new("fx").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new(REPO)],
                    ..Default::default()
                },
            )
            .unwrap();
        let roots = BTreeMap::from([(REPO.to_string(), case_dir(case))]);
        let engine = Engine::open(
            dir,
            full_registry(),
            roots,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap();
        engine.inbox().append(&NewEvent::rescan(REPO)).unwrap();
        let answer = engine
            .with_consistency(
                &QueryOptions {
                    consistency: Consistency::WaitAll,
                    timeout: Duration::from_secs(30),
                    scope: Scope::Project,
                },
                |tx| {
                    (
                        katalogr_engine::canonical_graph(tx),
                        katalogr_query::dangling(tx, &["contract"]).unwrap(),
                    )
                },
            )
            .unwrap();
        assert!(!answer.stale, "queue drained");
        let (text, dangling) = answer.value;
        let graph = parse_graph(&text).unwrap();
        Indexed {
            text,
            graph,
            dangling,
        }
    }

    /// `contract:wasm_export:<key>` as the canonical graph spells it.
    fn contract(key: &str) -> String {
        format!("contract:wasm_export:{}", key.replace('/', "%2F"))
    }

    impl Indexed {
        /// The link from `from` (a name below `fx:`) of `kind` to the contract `key`, with its
        /// confidence in lower case.
        fn link(&self, from: &str, kind: &str, key: &str) -> Option<String> {
            let from = format!("{REPO}:{from}");
            let to = contract(key);
            self.graph
                .links
                .iter()
                .find(|l| l.from == from && l.kind == kind && l.to == to)
                .map(|l| l.confidence.clone().unwrap_or_default().to_lowercase())
        }

        fn has(&self, from: &str, kind: &str, key: &str) -> bool {
            self.link(from, kind, key).is_some()
        }

        fn plain_link(&self, from: &str, kind: &str, to: &str) -> bool {
            let (from, to) = (format!("{REPO}:{from}"), format!("{REPO}:{to}"));
            self.graph
                .links
                .iter()
                .any(|l| l.from == from && l.kind == kind && l.to == to)
        }

        fn fact(&self, symbol: &str, key: &str) -> Option<String> {
            let name = format!("{REPO}:{symbol}");
            self.text.lines().find_map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).ok()?;
                if v["node"] == "symbol" && v["name"] == name.as_str() {
                    v["facts"][key]["Str"].as_str().map(str::to_string)
                } else {
                    None
                }
            })
        }
    }

    #[test]
    fn rust_exports_provide() {
        let g = index("exports-uncommitted");
        let lib = "src/lib.rs#";
        for (sym, key) in [
            ("fn:greet", "demo_wasm/greet"),
            ("fn:say_hi", "demo_wasm/sayHi"),
            ("fn:tick", "demo_wasm/tick"),
            ("fn:unused_export", "demo_wasm/unused_export"),
            ("struct:Counter", "demo_wasm/Counter"),
            ("impl:Counter/fn:new", "demo_wasm/Counter"),
            ("impl:Counter/fn:increment", "demo_wasm/Counter.increment"),
            ("impl:Counter/fn:value", "demo_wasm/Counter.value"),
            ("impl:Counter/fn:set_value", "demo_wasm/Counter.value"),
            ("enum:Color", "demo_wasm/Color"),
            ("enum:Color/variant:Red", "demo_wasm/Color.Red"),
        ] {
            assert!(
                g.has(&format!("{lib}{sym}"), "provides", key),
                "{sym} provides {key}"
            );
        }
        // Not exported: `start`, a private method, a trait impl.
        for sym in ["fn:boot", "impl:Counter/fn:helper"] {
            assert!(
                !g.graph
                    .links
                    .iter()
                    .any(|l| l.from == format!("{REPO}:{lib}{sym}") && l.kind == "provides"),
                "{sym}"
            );
        }
        assert_eq!(
            g.fact("src/lib.rs#fn:boot", "wasm_js").as_deref(),
            Some("(start)")
        );
        assert_eq!(
            g.fact("src/lib.rs#fn:say_hi", "wasm_js").as_deref(),
            Some("sayHi")
        );
    }

    #[test]
    fn package_key_matches_both_sides() {
        let g = index("scripts-out-dir");
        let rs = "crates/engine-core/src/lib.rs#";
        // `--out-dir`, `--out-name` and `--scope` name the package; the crate name keys it.
        assert!(g.has(
            &format!("{rs}fn:run_engine"),
            "provides",
            "engine_core/run_engine"
        ));
        assert!(g.has("web/main.ts#fn:start", "consumes", "engine_core/run_engine"));
        // Through the output directory, the scoped npm name is not needed.
        assert!(g.has(
            "web/main.ts#fn:start",
            "consumes",
            "engine_core/stop_engine"
        ));
        assert!(g.has(
            &format!("{rs}fn:stop_engine"),
            "provides",
            "engine_core/stop_engine"
        ));
    }

    #[test]
    fn ts_imports_consume() {
        let g = index("exports-uncommitted");
        for key in ["greet", "sayHi", "Counter", "Color"] {
            assert_eq!(
                g.link("web/src/app.ts", "consumes", &format!("demo_wasm/{key}"))
                    .as_deref(),
                Some("extracted"),
                "import {key}"
            );
        }
        let main = "web/src/app.ts#fn:main";
        for key in ["greet", "sayHi", "Counter", "Color.Red"] {
            assert_eq!(
                g.link(main, "consumes", &format!("demo_wasm/{key}"))
                    .as_deref(),
                Some("extracted"),
                "use {key}"
            );
        }
        // The default import is the glue's init function, which no Rust item provides.
        assert!(!g.graph.contracts.iter().any(|c| c.contains("default")));
    }

    #[test]
    fn ts_relative_glue_import_consumes() {
        let g = index("exports-uncommitted");
        assert_eq!(
            g.link("web/src/rel.ts#fn:run", "consumes", "demo_wasm/tick")
                .as_deref(),
            Some("extracted")
        );
    }

    #[test]
    fn ts_instance_member_inferred() {
        let g = index("exports-uncommitted");
        assert_eq!(
            g.link(
                "web/src/app.ts#fn:main",
                "consumes",
                "demo_wasm/Counter.increment"
            )
            .as_deref(),
            Some("inferred")
        );
    }

    #[test]
    fn glue_committed_declares() {
        let g = index("glue-committed");
        for (sym, key) in [
            ("pkg/demo_wasm.js#fn:greet", "demo_wasm/greet"),
            ("pkg/demo_wasm.js#fn:sayHi", "demo_wasm/sayHi"),
            ("pkg/demo_wasm.js#class:Counter", "demo_wasm/Counter"),
            (
                "pkg/demo_wasm.js#class:Counter/method:constructor",
                "demo_wasm/Counter",
            ),
            (
                "pkg/demo_wasm.js#class:Counter/method:increment",
                "demo_wasm/Counter.increment",
            ),
            (
                "pkg/demo_wasm.js#class:Counter/method:value",
                "demo_wasm/Counter.value",
            ),
            ("pkg/demo_wasm_bg.js#fn:tick", "demo_wasm/tick"),
            ("pkg/demo_wasm.d.ts#fn:greet", "demo_wasm/greet"),
            ("pkg/demo_wasm.d.ts#class:Counter", "demo_wasm/Counter"),
            ("pkg/demo_wasm.d.ts#enum:Color", "demo_wasm/Color"),
        ] {
            assert_eq!(
                g.link(sym, "declares", key).as_deref(),
                Some("extracted"),
                "{sym}"
            );
        }
        // The consumer reaches the provider through the same contract.
        assert!(g.has("app.ts#fn:main", "consumes", "demo_wasm/greet"));
        assert!(g.has("src/lib.rs#fn:greet", "provides", "demo_wasm/greet"));
    }

    #[test]
    fn glue_absent_still_linked() {
        let g = index("exports-uncommitted");
        assert!(
            !g.text.contains("\"declares\""),
            "no glue file in the repository, so nothing declares"
        );
        assert!(g.has("src/lib.rs#fn:greet", "provides", "demo_wasm/greet"));
        assert!(g.has("web/src/app.ts#fn:main", "consumes", "demo_wasm/greet"));
        assert!(g.graph.contracts.contains(&contract("demo_wasm/greet")));
        assert!(
            g.dangling
                .iter()
                .all(|d| !d.wanted.contains(&contract("demo_wasm/greet"))),
            "provider and consumer meet"
        );
    }

    #[test]
    fn extern_module_import_linked() {
        let g = index("extern-imports");
        assert!(g.plain_link("src/lib.rs#fn:now", "calls", "js/host.js#fn:now"));
        assert!(g.plain_link(
            "src/lib.rs#type:Clock",
            "references",
            "js/host.js#class:Clock"
        ));
        assert_eq!(
            g.fact("src/lib.rs#fn:now", "wasm_import").as_deref(),
            Some("/js/host.js:now")
        );
        assert_eq!(
            g.fact("src/lib.rs#fn:tick", "wasm_import").as_deref(),
            Some("/js/host.js:Clock.tick")
        );
    }

    #[test]
    fn global_import_fact_only() {
        let g = index("extern-imports");
        assert_eq!(
            g.fact("src/lib.rs#fn:log", "wasm_import").as_deref(),
            Some("global:console.log")
        );
        assert_eq!(
            g.fact("src/lib.rs#fn:alert", "wasm_import").as_deref(),
            Some("global:alert")
        );
        for sym in ["fn:log", "fn:alert"] {
            let from = format!("{REPO}:src/lib.rs#{sym}");
            assert!(
                !g.graph.links.iter().any(|l| l.from == from),
                "{sym} has no outgoing link"
            );
        }
    }

    #[test]
    fn dangling_exports_reported() {
        let g = index("exports-uncommitted");
        let unused = contract("demo_wasm/unused_export");
        let hit = g
            .dangling
            .iter()
            .find(|d| d.wanted == [unused.clone()])
            .expect("unused_export is reported");
        assert_eq!(hit.kind, "provides");
        assert!(hit.from.name.ends_with("src/lib.rs#fn:unused_export"));
        let reported: Vec<&str> = g.dangling.iter().map(|d| d.wanted[0].as_str()).collect();
        assert!(!reported.contains(&contract("demo_wasm/greet").as_str()));
    }

    #[test]
    fn glue_only_names_skipped() {
        let g = index("glue-committed");
        for c in &g.graph.contracts {
            for bad in ["default", "initSync", "free", "__wbg_", "__wbindgen_"] {
                assert!(!c.contains(bad), "{c} names a glue-only name");
            }
        }
        for l in g
            .graph
            .links
            .iter()
            .filter(|l| l.to.starts_with("contract:"))
        {
            assert!(
                !l.to.contains("__wb") && !l.to.contains("initSync"),
                "{l:?}"
            );
        }
        assert!(g.graph.contracts.contains(&contract("demo_wasm/greet")));
    }

    #[test]
    fn deterministic_output() {
        let a = index("glue-committed");
        let b = index("glue-committed");
        assert_eq!(a.text, b.text);
        let c = index("exports-uncommitted");
        let d = index("exports-uncommitted");
        assert_eq!(c.text, d.text);
    }
}
