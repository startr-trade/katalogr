//! The page's views, driven under the harness with canned answers written by hand against
//! `schema/v1` (FEAT-033 T12). All names are synthetic.

mod web_app {
    use katalogr_webtest::{Backend, MapBackend, Page};
    use serde_json::{Value, json};

    const HOSTILE: &str = "<img src=x onerror=alert(1)>";
    const SNAPSHOT_ID: &str = "v1-0000000000000000000000000000000a";
    const SYMBOL: &str = "repo%3Aapp%23fn%3Atarget";

    fn fx(name: &str) -> String {
        match name {
            "meta" => include_str!("../fixtures/meta.json"),
            "views-catalogue" => include_str!("../fixtures/views-catalogue.json"),
            "view-structure" => include_str!("../fixtures/view-structure.json"),
            "view-callers" => include_str!("../fixtures/view-callers.json"),
            "view-contracts" => include_str!("../fixtures/view-contracts.json"),
            "timeline" => include_str!("../fixtures/timeline.json"),
            "explain" => include_str!("../fixtures/explain.json"),
            "why" => include_str!("../fixtures/why.json"),
            "tests" => include_str!("../fixtures/tests.json"),
            "show" => include_str!("../fixtures/show.json"),
            "search" => include_str!("../fixtures/search.json"),
            "hubs" => include_str!("../fixtures/hubs.json"),
            "cycles" => include_str!("../fixtures/cycles.json"),
            "communities" => include_str!("../fixtures/communities.json"),
            "dead-code" => include_str!("../fixtures/dead-code.json"),
            "coverage" => include_str!("../fixtures/coverage.json"),
            other => panic!("no fixture {other}"),
        }
        .to_string()
    }

    fn json_fx(name: &str) -> Value {
        serde_json::from_str(&fx(name)).unwrap()
    }

    const STRUCTURE: &str = "/api/v1/views/structure?consistency=wait&timeout=10";
    const CALLERS: &str = "/api/v1/views/callers?consistency=wait&symbol=target&timeout=10";
    const CONTRACTS: &str = "/api/v1/views/contracts?consistency=wait&timeout=10";

    /// Every answer the first-delivery views ask for.
    fn backend() -> MapBackend {
        MapBackend::new()
            .with("/api/v1/meta", 200, &fx("meta"))
            .with("/api/v1/views/", 200, &fx("views-catalogue"))
            .with(STRUCTURE, 200, &fx("view-structure"))
            .with(CALLERS, 200, &fx("view-callers"))
            .with(CONTRACTS, 200, &fx("view-contracts"))
            .with("/api/v1/timeline", 200, &fx("timeline"))
            .with(
                &format!("/api/v1/stored-view?id={SNAPSHOT_ID}"),
                200,
                &fx("view-structure"),
            )
            .with(
                &format!("/api/v1/explain?symbol={SYMBOL}"),
                200,
                &fx("explain"),
            )
            .with("/api/v1/why?location=src%2Flib.rs%3A3", 200, &fx("why"))
            .with(&format!("/api/v1/tests?text={SYMBOL}"), 200, &fx("tests"))
            .with(&format!("/api/v1/show?symbol={SYMBOL}"), 200, &fx("show"))
            .with(
                &format!("/api/v1/views/callers?consistency=wait&symbol={SYMBOL}&timeout=10"),
                200,
                &fx("view-callers"),
            )
            .with("/api/v1/search?terms=target", 200, &fx("search"))
            .with("/api/v1/hubs?limit=20", 200, &fx("hubs"))
            .with("/api/v1/cycles?limit=20", 200, &fx("cycles"))
            .with("/api/v1/communities?limit=20", 200, &fx("communities"))
            .with("/api/v1/dead-code?limit=20", 200, &fx("dead-code"))
            .with("/api/v1/coverage", 200, &fx("coverage"))
    }

    fn with_json(b: MapBackend, path: &str, status: u16, v: &Value) -> MapBackend {
        b.with(path, status, &v.to_string())
    }

    fn mermaid(name: &str) -> String {
        json_fx(name)["mermaid"].as_str().unwrap().to_string()
    }

    fn open(hash: &str, b: &MapBackend) -> Page {
        Page::open(hash, b)
    }

    // ---- R-1, R-3 ----

    #[test]
    fn link_opens_each_kept_view() {
        let b = with_json(
            with_json(
                backend(),
                "/api/v1/views/callers?at=abc&consistency=wait&symbol=target&timeout=10",
                200,
                &json_fx("view-callers"),
            ),
            "/api/v1/views/contracts?at=abc&consistency=wait&timeout=10",
            200,
            &json_fx("view-contracts"),
        );
        let mut p = open("#/graph?at=abc&symbol=target", &b);
        assert!(p.text().contains("Graph explorer"));
        assert!(p.requests().contains(
            &"/api/v1/views/callers?at=abc&consistency=wait&symbol=target&timeout=10".to_string()
        ));
        p.navigate("#/service-map?at=abc&symbol=target", &b);
        assert!(p.text().contains("Service map"));
        assert!(
            p.requests()
                .iter()
                .any(|r| r.starts_with("/api/v1/views/contracts?at=abc"))
        );
        p.navigate("#/impact?symbol=target", &b);
        assert!(
            p.text()
                .contains("Impact analysis isn't in this version of katalogr."),
            "{}",
            p.text()
        );
        assert!(b.missed().is_empty(), "{:?}", b.missed());
    }

    #[test]
    fn symbol_lists_requirements_and_tests() {
        let b = backend();
        let mut p = open(&format!("#/symbol?symbol={SYMBOL}"), &b);
        let t = p.text();
        assert!(t.contains("repo:app#fn:target"), "{t}");
        assert!(t.contains("pub fn target() -> u32"), "{t}");
        assert!(t.contains("REQ-3"), "{t}");
        assert!(t.contains("REQ-7 - Orders are kept"), "{t}");
        assert!(t.contains("repo:app#fn:test_target (case)"), "{t}");
        assert!(t.contains("G-1 - Traceability"), "{t}");
        assert!(
            t.contains("    1\n}") || t.contains("pub fn target() -> u32 {"),
            "{t}"
        );
        assert!(p.has("section-requirements") && p.has("section-tests") && p.has("section-goals"));
        assert!(b.missed().is_empty(), "{:?}", b.missed());
    }

    #[test]
    fn symbol_without_links_says_so() {
        let mut why = json_fx("why");
        why["result"]["rationale"] = json!([]);
        why["result"]["goals"] = json!([]);
        why["result"]["commits"] = json!([]);
        let mut tests = json_fx("tests");
        tests["result"] = json!([]);
        let b = with_json(
            with_json(
                backend(),
                "/api/v1/why?location=src%2Flib.rs%3A3",
                200,
                &why,
            ),
            &format!("/api/v1/tests?text={SYMBOL}"),
            200,
            &tests,
        );
        let mut p = open(&format!("#/symbol?symbol={SYMBOL}"), &b);
        let t = p.text();
        assert!(t.contains("No requirements are linked to this symbol. They appear when a project's board is indexed."), "{t}");
        assert!(t.contains("No tests are linked to this symbol."), "{t}");
        assert!(!p.has("section-goals"));
    }

    // ---- R-8, R-10 ----

    #[test]
    fn diagram_draws_answer_bytes() {
        let b = backend();
        let mut p = open("#/graph", &b);
        assert_eq!(p.pele_sources(), [mermaid("view-structure")]);
        assert!(p.has("diagram"));
        assert_eq!(
            p.requests(),
            [
                "/api/v1/meta",
                "/api/v1/views/",
                STRUCTURE,
                "/api/v1/timeline"
            ]
        );
    }

    #[test]
    fn missing_capability_says_so() {
        let b = backend();
        let mut p = open("#/dependencies", &b);
        assert!(
            p.text()
                .contains("Dependency data isn't in this version of katalogr.")
        );
        let mut p = open("#/type-lineage", &b);
        assert!(
            p.text()
                .contains("Type lineage isn't in this version of katalogr.")
        );
        // An absent view asks for nothing but the capabilities.
        assert_eq!(p.requests(), ["/api/v1/meta"]);
    }

    #[test]
    fn empty_answer_says_so() {
        let b = with_json(
            backend(),
            "/api/v1/hubs?limit=20",
            200,
            &json!({"schema": 1, "result": {"items": []}}),
        );
        let mut p = open("#/health", &b);
        let t = p.text();
        assert!(t.contains("No hubs in this index."), "{t}");
        assert!(t.contains("Cycles"), "{t}");
    }

    #[test]
    fn absent_panel_leaves_others() {
        let b = backend();
        let mut p = open("#/health", &b);
        let t = p.text();
        assert!(
            t.contains("Hotspots and change coupling aren't in this version of katalogr."),
            "{t}"
        );
        assert!(
            t.contains("Drift checks aren't in this version of katalogr."),
            "{t}"
        );
        assert!(t.contains("repo:app#fn:target"), "{t}");
        assert!(
            !p.requests()
                .iter()
                .any(|r| r.contains("hotspots") || r.contains("drift"))
        );
    }

    #[test]
    fn health_draws_first_delivery_panels() {
        let b = backend();
        let mut p = open("#/health", &b);
        for id in [
            "hubs",
            "cycles",
            "communities",
            "dead-code",
            "coverage",
            "hotspots",
            "drift",
            "rules",
            "trends",
        ] {
            assert!(p.has(&format!("panel-{id}")), "{id}");
        }
        let t = p.text();
        for want in [
            "Hubs",
            "Dead code candidates",
            "Coverage",
            "rs",
            "p, q",
            "unused",
        ] {
            assert!(t.contains(want), "{want}: {t}");
        }
        assert!(b.missed().is_empty(), "{:?}", b.missed());
    }

    // ---- R-11 ----

    #[test]
    fn graph_controls_follow_catalogue() {
        let b = with_json(
            backend(),
            "/api/v1/views/structure?consistency=wait&level=file&timeout=10",
            200,
            &json_fx("view-structure"),
        );
        let mut p = open("#/graph", &b);
        for k in ["param-level", "param-max-nodes", "param-inferred"] {
            assert!(p.has(k), "{k}");
        }
        assert!(!p.has("param-symbol"));
        p.fire("param-level", "change", Some("file"), &b);
        assert_eq!(
            p.eval_json("location.hash"),
            r##""#/graph?level=file&op=structure""##
        );
        p.navigate("#/graph?level=file&op=structure", &b);
        assert!(p.requests().contains(
            &"/api/v1/views/structure?consistency=wait&level=file&timeout=10".to_string()
        ));
        let mut c = open("#/graph?symbol=target", &backend());
        assert!(c.has("param-symbol") && c.has("param-depth"));
    }

    #[test]
    fn symbol_link_selects_callers() {
        let b = backend();
        let mut p = open("#/graph?symbol=target", &b);
        let r = p.requests();
        assert!(r.contains(&CALLERS.to_string()), "{r:?}");
        assert!(!r.contains(&STRUCTURE.to_string()));
        assert_eq!(p.pele_sources(), [mermaid("view-callers")]);
    }

    #[test]
    fn explore_uses_same_answer() {
        let b = backend();
        let mut p = open("#/graph?tab=explore", &b);
        assert!(p.pele_sources().is_empty());
        assert_eq!(p.eval_json("__cy.length"), "1");
        assert_eq!(p.eval_json("__cy[0].length"), "3");
        let before = p.requests();
        p.fire("tab-diagram", "click", None, &b);
        assert_eq!(p.pele_sources(), [mermaid("view-structure")]);
        assert_eq!(p.requests(), before);
        p.fire("tab-explore", "click", None, &b);
        assert_eq!(p.requests(), before);
        assert_eq!(p.eval_json("__cy.length"), "2");
    }

    #[test]
    fn service_map_uses_contracts() {
        let b = backend();
        let mut p = open("#/service-map", &b);
        assert!(p.requests().contains(&CONTRACTS.to_string()));
        assert_eq!(p.pele_sources(), [mermaid("view-contracts")]);
        assert!(p.pele_sources()[0].contains("endpoint:orders"));
    }

    #[test]
    fn service_map_symbol_keeps_diagram() {
        let b = backend();
        let mut p = open("#/service-map?symbol=serve", &b);
        assert!(!p.requests().iter().any(|r| r.contains("views/callers")));
        assert_eq!(p.pele_sources(), [mermaid("view-contracts")]);
        p.fire("tab-explore", "click", None, &b);
        assert_eq!(
            p.eval_json("__cy[0].filter(function (e) { return e.classes === 'highlight'; }).map(function (e) { return e.data.label; })"),
            r#"["serve"]"#
        );
    }

    // ---- R-13 ----

    #[test]
    fn slider_shows_only_matching_points() {
        let b = backend();
        let mut p = open("#/graph", &b);
        assert!(p.has("slider"));
        // Two of the three timeline points ask the structure question.
        assert_eq!(
            p.eval_json("__byKw('slider').getAttribute('max')"),
            r#""1""#
        );
        let t = p.text();
        assert!(
            t.contains("2023-11-14 22:30 UTC") || t.contains("UTC"),
            "{t}"
        );
        assert!(t.contains("stored"), "{t}");
        // The timeline is read once per page load, even across views.
        p.navigate("#/graph?level=file", &b);
        assert_eq!(
            p.requests()
                .iter()
                .filter(|r| r.as_str() == "/api/v1/timeline")
                .count(),
            1
        );
    }

    #[test]
    fn slider_hidden_when_nothing_stored() {
        let b = with_json(
            backend(),
            "/api/v1/timeline",
            200,
            &json!({"schema": 1, "result": {"points": []}}),
        );
        let mut p = open("#/graph", &b);
        assert!(!p.has("slider"));
        assert!(
            p.text()
                .contains("Nothing is stored for this view yet, so there is no history to show.")
        );
    }

    #[test]
    fn slider_point_reads_stored_bytes() {
        let b = backend();
        let mut p = open("#/graph", &b);
        p.fire("slider", "change", Some("0"), &b);
        let hash = format!("#/graph?id={SNAPSHOT_ID}");
        assert_eq!(p.eval_json("location.hash"), format!("\"{hash}\""));
        p.navigate(&hash, &b);
        let r = p.requests();
        assert_eq!(
            r.last().unwrap(),
            &format!("/api/v1/stored-view?id={SNAPSHOT_ID}")
        );
        assert_eq!(
            r.iter()
                .filter(|x| x.starts_with("/api/v1/views/structure"))
                .count(),
            1
        );
        assert!(p.has("slider-back"));
        assert_eq!(p.pele_sources().len(), 2);
    }

    // ---- R-15 ----

    #[test]
    fn stale_answer_marked() {
        let mut v = json_fx("view-structure");
        v["exact"] = json!(false);
        let b = with_json(backend(), STRUCTURE, 200, &v);
        let mut p = open("#/graph", &b);
        assert!(
            p.text().contains(
                "The index is still catching up, so this may not match the current code."
            )
        );
    }

    #[test]
    fn partial_answer_marked() {
        let mut v = json_fx("view-structure");
        v["partial"] = json!([{"cap": "limit", "at": 5, "shown": 5, "total": 9, "note": "absence is not proof"}]);
        let b = with_json(backend(), STRUCTURE, 200, &v);
        let mut p = open("#/graph", &b);
        assert!(p.text().contains("Showing 5 of 9."));
    }

    #[test]
    fn position_unavailable_lists_held() {
        let body = json!({"schema": 1, "error": "position-unavailable", "wanted": "x",
            "available": [{"repo": "app", "commit": "0123456789abcdef0123456789abcdef01234567", "dirty": null}]});
        let b = with_json(
            backend(),
            "/api/v1/views/structure?at=zz&consistency=wait&timeout=10",
            409,
            &body,
        );
        let mut p = open("#/graph?at=zz", &b);
        let t = p.text();
        assert!(
            t.contains("This position isn't held here. Held positions:"),
            "{t}"
        );
        assert!(t.contains("app@0123456789ab"), "{t}");
    }

    #[test]
    fn ambiguous_symbol_lists_choices() {
        let body = json!({"schema": 1, "result": [
            {"name": "repo:a#fn:dup", "kind": "fn", "path": "a.rs", "span": {"start_line": 1, "start_col": 0, "end_line": 1, "end_col": 1}},
            {"name": "repo:b#fn:dup", "kind": "fn", "path": "b.rs", "span": {"start_line": 1, "start_col": 0, "end_line": 1, "end_col": 1}}]});
        let b = with_json(backend(), "/api/v1/explain?symbol=dup", 409, &body);
        let mut p = open("#/symbol?symbol=dup", &b);
        let t = p.text();
        assert!(t.contains("Several symbols match. Choose one:"), "{t}");
        assert!(
            t.contains("repo:a#fn:dup") && t.contains("repo:b#fn:dup"),
            "{t}"
        );
    }

    struct Gone;
    impl Backend for Gone {
        fn get(&self, _: &str) -> (u16, String) {
            (0, String::new())
        }
    }

    #[test]
    fn server_gone_says_so() {
        let mut p = Page::open("#/graph", &Gone);
        assert!(
            p.text()
                .contains("katalogr isn't answering. Is katalogr serve still running?")
        );
    }

    #[test]
    fn symbol_uses_wait_for_views() {
        let b = backend();
        let mut p = open(&format!("#/symbol?symbol={SYMBOL}"), &b);
        for r in p.requests() {
            let is_view = r.contains("/views/callers");
            assert_eq!(r.contains("consistency=wait"), is_view, "{r}");
        }
    }

    // ---- R-16, R-17 ----

    #[test]
    fn text_is_never_markup() {
        let mut view = json_fx("view-structure");
        view["graph"]["nodes"][0]["name"] = json!(HOSTILE);
        view["graph"]["nodes"][0]["label"] = json!(HOSTILE);
        let mut search = json_fx("search");
        search["result"][0]["symbol"]["name"] = json!(HOSTILE);
        let mut explain = json_fx("explain");
        explain["result"]["symbol"]["name"] = json!(HOSTILE);
        explain["result"]["doc"] = json!(HOSTILE);
        let b = with_json(
            with_json(
                with_json(backend(), STRUCTURE, 200, &view),
                "/api/v1/search?terms=target",
                200,
                &search,
            ),
            &format!("/api/v1/explain?symbol={SYMBOL}"),
            200,
            &explain,
        );
        let mut p = open("#/graph?tab=explore", &b);
        p.navigate("#/graph", &b);
        p.navigate("#/search?q=target", &b);
        assert!(p.text().contains(HOSTILE));
        p.navigate(&format!("#/symbol?symbol={SYMBOL}"), &b);
        assert!(p.text().contains(HOSTILE));
        // Pele's SVG is the only markup ever written.
        for w in p.markup_writes() {
            assert_eq!(w, "<svg></svg>");
        }
    }

    #[test]
    fn theme_switch_does_not_redraw() {
        let b = backend();
        let mut p = open("#/graph", &b);
        assert_eq!(p.eval_json("KW.theme.current()"), r#""light""#);
        let (sources, requests) = (p.pele_sources(), p.requests());
        p.fire("theme-button", "click", None, &b);
        assert_eq!(p.eval_json("KW.theme.current()"), r#""coal""#);
        assert_eq!(
            p.eval_json("document.documentElement.className"),
            r#""coal""#
        );
        assert_eq!((p.pele_sources(), p.requests()), (sources, requests));
        p.fire("theme-button", "click", None, &b);
        assert_eq!(p.eval_json("KW.theme.current()"), r#""light""#);
        // With nothing saved, the system's choice decides.
        let mut q = open("#/graph", &b);
        assert_eq!(
            q.eval_json("(function () { localStorage.removeItem('katalogr-theme'); window.matchMedia = function () { return { matches: true }; }; return KW.theme.choose(); })()"),
            r#""coal""#
        );
        assert_eq!(
            q.eval_json("(function () { localStorage.setItem('katalogr-theme', 'light'); return KW.theme.choose(); })()"),
            r#""light""#
        );
    }

    #[test]
    fn navigation_is_real_links() {
        let b = backend();
        let mut p = open("#/graph", &b);
        assert_eq!(p.eval_json("__byKw('nav-graph').tagName"), r#""A""#);
        for (id, href) in [
            ("graph", "#/graph"),
            ("service-map", "#/service-map"),
            ("impact", "#/impact"),
            ("health", "#/health"),
        ] {
            assert_eq!(
                p.eval_json(&format!("__byKw('nav-{id}').getAttribute('href')")),
                format!("\"{href}\"")
            );
        }
        assert!(!p.has("nav-symbol"));
        assert_eq!(
            p.eval_json("__byKw('nav-graph').getAttribute('aria-current')"),
            r#""page""#
        );
    }
}
