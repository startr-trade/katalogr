//! The type lineage page, driven under the harness with canned answers written by hand against
//! `schema/v1/lineage.schema.json` (FEAT-101 T13). All names are synthetic.

mod web_app {
    use katalogr_webtest::{Backend, MapBackend, Page};
    use serde_json::{Value, json};

    const SUBJECT: &str = "repo:app#type:Order";
    const ENC: &str = "repo%3Aapp%23type%3AOrder";
    const HOSTILE: &str = "<img src=x onerror=alert(1)>";
    const SYMBOL: &str = "repo%3Aapp%23fn%3Atarget";

    const LINEAGE: &str =
        "/api/v1/lineage?consistency=wait&text=repo%3Aapp%23type%3AOrder&timeout=10";
    const VIEW: &str =
        "/api/v1/views/lineage?consistency=wait&symbol=repo%3Aapp%23type%3AOrder&timeout=10";

    fn fx(name: &str) -> String {
        match name {
            "meta" => include_str!("../fixtures/meta.json"),
            "lineage" => include_str!("../fixtures/lineage.json"),
            "view-lineage" => include_str!("../fixtures/view-lineage.json"),
            "timeline" => include_str!("../fixtures/timeline.json"),
            "explain" => include_str!("../fixtures/explain.json"),
            "why" => include_str!("../fixtures/why.json"),
            "tests" => include_str!("../fixtures/tests.json"),
            "show" => include_str!("../fixtures/show.json"),
            "view-callers" => include_str!("../fixtures/view-callers.json"),
            other => panic!("no fixture {other}"),
        }
        .to_string()
    }

    fn json_fx(name: &str) -> Value {
        serde_json::from_str(&fx(name)).unwrap()
    }

    /// The meta answer, with or without the capability.
    fn meta(with: bool) -> String {
        let mut m = json_fx("meta");
        if with {
            let caps = m["capabilities"].as_array_mut().unwrap();
            caps.push(json!("type-lineage"));
        }
        m.to_string()
    }

    fn backend() -> MapBackend {
        MapBackend::new()
            .with("/api/v1/meta", 200, &meta(true))
            .with(LINEAGE, 200, &fx("lineage"))
            .with(VIEW, 200, &fx("view-lineage"))
            .with("/api/v1/timeline", 200, &fx("timeline"))
    }

    fn mermaid() -> String {
        json_fx("view-lineage")["mermaid"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn lineage_asks_for_a_subject() {
        let found = json!({"schema": 1, "result": [
            {"name": SUBJECT, "kind": "type", "path": "schemas/orders.tx",
             "span": {"start_line": 1, "start_col": 1, "end_line": 1, "end_col": 1}}]});
        let b = backend().with("/api/v1/find?text=Order", 200, &found.to_string());
        let mut p = Page::open("#/type-lineage", &b);
        let t = p.text();
        assert!(
            t.contains("Choose a type, schema element or contract to see where it goes."),
            "{t}"
        );
        assert!(p.has("lineage-search"));
        // Nothing is asked of the lineage ops until a subject is named.
        assert_eq!(p.requests(), ["/api/v1/meta"]);
        p.fire("lineage-search", "change", Some("Order"), &b);
        assert!(p.text().contains(SUBJECT), "{}", p.text());
        assert!(
            p.requests()
                .contains(&"/api/v1/find?text=Order".to_string())
        );
        assert!(b.missed().is_empty(), "{:?}", b.missed());
    }

    #[test]
    fn lineage_draws_sections() {
        let b = backend();
        let mut p = Page::open(&format!("#/type-lineage?symbol={ENC}"), &b);
        let t = p.text();
        for kw in [
            "lineage-subject",
            "lineage-declared",
            "lineage-fields",
            "lineage-related",
            "lineage-bindings",
            "lineage-code",
            "lineage-contracts",
            "lineage-stops",
        ] {
            assert!(p.has(kw), "{kw}: {t}");
        }
        // The sections come in the order of the design.
        let at = |s: &str| t.find(s).unwrap_or_else(|| panic!("{s} in {t}"));
        let order = [
            SUBJECT,
            "Declared in",
            "Fields",
            "Related types",
            "Bindings",
            "Code",
            "Contracts",
            "Where this stops",
        ];
        for w in order.windows(2) {
            assert!(at(w[0]) < at(w[1]), "{} before {}", w[0], w[1]);
        }
        for want in [
            "uses-x, key, extracted: 3",
            "Showing 2 of 3.",
            "through lines",
            "contract:shape:Order: 0 providers, 0 consumers - nothing else in this project uses it",
            "(test)",
            "repo:app#key:wire (maps-x, declared)",
        ] {
            assert!(t.contains(want), "{want} in {t}");
        }
        // The group cut links to the same page asking for all of it.
        let href = p.eval_json("__byKw('lineage-expand').getAttribute('href')");
        assert!(
            href.contains("expand=true") && href.contains("symbol="),
            "{href}"
        );
        assert!(b.missed().is_empty(), "{:?}", b.missed());
    }

    #[test]
    fn lineage_diagram_draws_answer_bytes() {
        let b = backend();
        let mut p = Page::open(&format!("#/type-lineage?symbol={ENC}"), &b);
        assert_eq!(p.pele_sources(), [mermaid()]);
        assert!(p.has("diagram"));
        let r = p.requests();
        assert!(
            r.contains(&LINEAGE.to_string()) && r.contains(&VIEW.to_string()),
            "{r:?}"
        );
    }

    #[test]
    fn lineage_boundaries_in_words() {
        let b = backend();
        let mut p = Page::open(&format!("#/type-lineage?symbol={ENC}"), &b);
        assert!(
            p.text().contains(
                "A contract here has nothing on one side in this project, so it may continue elsewhere."
            ),
            "{}",
            p.text()
        );
        let words = [
            (
                "no-contract",
                "This binds no contract, so only direct references to it can show.",
            ),
            (
                "no-fields",
                "No fields are recorded for this type. If it is a code type, the project may have turned fields off with extract.code_fields.",
            ),
            (
                "owner-level",
                "This is a field. Bindings below are on the type that holds it, unless they name the field.",
            ),
            (
                "no-code",
                "No code is linked. Code appears when it reaches this through a contract, for example by a project's profile.",
            ),
            (
                "open-contract",
                "A contract here has nothing on one side in this project, so it may continue elsewhere.",
            ),
        ];
        for (code, text) in words {
            assert_eq!(
                p.eval_json(&format!("KW.lineage.boundaryText('{code}')")),
                serde_json::to_string(text).unwrap(),
                "{code}"
            );
        }
    }

    #[test]
    fn lineage_ambiguous_lists_choices() {
        let amb = json!({"schema": 1, "result": [
            {"name": "repo:app#type:Order", "kind": "type", "path": "a.tx",
             "span": {"start_line": 1, "start_col": 1, "end_line": 1, "end_col": 1}},
            {"name": "repo:app#struct:Order", "kind": "struct", "path": "b.rs",
             "span": {"start_line": 1, "start_col": 1, "end_line": 1, "end_col": 1}}]});
        let b = backend()
            .with(
                "/api/v1/lineage?consistency=wait&text=Order&timeout=10",
                409,
                &amb.to_string(),
            )
            .with(
                "/api/v1/views/lineage?consistency=wait&symbol=Order&timeout=10",
                409,
                &amb.to_string(),
            );
        let mut p = Page::open("#/type-lineage?symbol=Order", &b);
        let t = p.text();
        assert!(t.contains("Several symbols match. Choose one:"), "{t}");
        assert!(
            t.contains("repo:app#type:Order") && t.contains("repo:app#struct:Order"),
            "{t}"
        );
        // A choice links back to this page.
        p.navigate(&format!("#/type-lineage?symbol={ENC}"), &b);
        assert!(p.text().contains("Declared in"));
    }

    #[test]
    fn lineage_names_are_text() {
        let mut l = json_fx("lineage");
        l["result"]["subject"]["name"] = json!(HOSTILE);
        l["result"]["declared_in"][0]["name"] = json!(HOSTILE);
        l["result"]["bindings"][0]["hits"][0]["node"]["name"] = json!(HOSTILE);
        l["result"]["related"][0]["through_field"] = json!(HOSTILE);
        let b = backend().with(LINEAGE, 200, &l.to_string());
        let mut p = Page::open(&format!("#/type-lineage?symbol={ENC}"), &b);
        assert!(p.text().contains(HOSTILE), "{}", p.text());
        // Markup is set only for the diagram the library drew.
        assert_eq!(p.markup_writes(), ["<svg></svg>"]);
    }

    #[test]
    fn symbol_page_links_lineage() {
        let b = |with: bool| {
            MapBackend::new()
                .with("/api/v1/meta", 200, &meta(with))
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
        };
        let mut p = Page::open(&format!("#/symbol?symbol={SYMBOL}"), &b(true));
        assert!(p.has("symbol-lineage"), "{}", p.text());
        assert!(p.text().contains("Type lineage"));
        let mut q = Page::open(&format!("#/symbol?symbol={SYMBOL}"), &b(false));
        assert!(!q.has("symbol-lineage"));
        let _: &dyn Backend = &b(true);
    }
}
