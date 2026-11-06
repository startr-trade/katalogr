//! The page's side of the single file (FEAT-034 T3): the real scripts under the harness, opened
//! on the text of an exported file with canned data. A request that is not answered by the
//! file's own transport fails the test.

mod support;

mod web_export {
    use super::support::*;
    use katalogr_web::export::{plan_views, single_file};
    use katalogr_webtest::{NoBackend, Page};
    use serde_json::json;

    const SYMBOL_HASH: &str = "#/symbol?symbol=repo%3Aapp%23fn%3Atarget";

    /// A request path as the file's transport keys it: no prefix, no volatile names.
    fn key_of(path: &str) -> String {
        let p = path.strip_prefix("/api/v1/").unwrap_or(path);
        let (op, qs) = p.split_once('?').unwrap_or((p, ""));
        let kept: Vec<&str> = qs
            .split('&')
            .filter(|x| !x.is_empty())
            .filter(|x| {
                !["consistency=", "timeout=", "at="]
                    .iter()
                    .any(|v| x.starts_with(v))
            })
            .collect();
        if kept.is_empty() {
            op.to_string()
        } else {
            format!("{op}?{}", kept.join("&"))
        }
    }

    #[test]
    fn symbol_lists_requirements_and_tests() {
        for with_source in [false, true] {
            let d = canned(with_source);
            let planned = planned_keys(&d);
            let mut p = Page::open_file(&single_file(&d), SYMBOL_HASH);
            let t = p.text();
            assert!(t.contains("repo:app#fn:target"), "{t}");
            assert!(t.contains("pub fn target() -> u32"), "{t}");
            assert!(t.contains("REQ-3"), "{t}");
            assert!(t.contains("REQ-7 - Orders are kept"), "{t}");
            assert!(t.contains("repo:app#fn:test_target (case)"), "{t}");
            assert!(p.has("section-requirements") && p.has("section-tests"));
            assert!(!t.contains("This isn't in this export"), "{t}");
            // Every request the page made for the symbol is one the plan holds.
            for r in p.requests() {
                let k = key_of(&r);
                assert!(k == "meta" || planned.contains(&k), "{k} is not planned");
            }
            // `show` asks for the signature unless the file was made with the source.
            let asked_signature = p
                .requests()
                .iter()
                .any(|r| r.starts_with("/api/v1/show?") && r.contains("signature=true"));
            assert_eq!(asked_signature, !with_source);
        }
    }

    #[test]
    fn default_requests_are_planned() {
        let d = empty();
        let planned: Vec<String> = plan_views(&d.capabilities)
            .iter()
            .map(katalogr_web::export::answer_key)
            .collect();
        let file = single_file(&d);
        let manifest = katalogr_web::manifest::manifest();
        let mut asked = 0;
        for view in manifest.views.iter().filter(|v| v.nav) {
            let mut p = Page::open_file(&file, &format!("#/{}", view.id));
            for r in p.requests() {
                let k = key_of(&r);
                if k == "meta" || k == "timeline" {
                    continue;
                }
                asked += 1;
                assert!(
                    planned.contains(&k),
                    "{}: {k} is not planned ({planned:?})",
                    view.id
                );
            }
        }
        assert!(asked >= 6, "only {asked} requests were checked");
        // The graph page's controls may ask for either level, and both are held.
        for level in [
            "views/structure?level=package",
            "views/structure?level=file",
        ] {
            assert!(planned.contains(&level.to_string()), "{level}");
        }
    }

    #[test]
    fn missing_answer_says_not_in_export() {
        let mut p = Page::open_file(&single_file(&empty()), "#/graph");
        let t = p.text();
        assert!(
            t.contains("This isn't in this export. Run katalogr serve on the project to see it, or export again with a larger --max-symbols."),
            "{t}"
        );
        p.navigate(SYMBOL_HASH, &NoBackend);
        assert!(p.text().contains("This isn't in this export."));
        assert_eq!(
            p.eval_json("KW.core.statusMessage(0, 'not-in-export').text"),
            "\"This isn't in this export. Run katalogr serve on the project to see it, or export again with a larger --max-symbols.\""
        );
        // A real transport failure still says what it said.
        assert!(
            p.eval_json("KW.core.statusMessage(0, '').text")
                .contains("Is katalogr serve still running?")
        );
    }

    #[test]
    fn find_filters_embedded_symbols() {
        let mut d = canned(false);
        let sym = |n: &str| {
            json!({"name": n, "kind": "fn", "path": "src/lib.rs",
            "span": {"start_line": 1, "start_col": 0, "end_line": 1, "end_col": 1}})
        };
        d.symbols = vec![
            sym("repo:app#fn:Alpha"),
            sym("repo:app#fn:beta"),
            sym("repo:app#fn:alphabet"),
        ];
        let mut p = Page::open_file(&single_file(&d), "#/health");
        let r = p.eval_json(
            "(function () { var r; window.KW_TRANSPORT('/api/v1/find?consistency=wait&text=ALPH&timeout=10', \
             function (s, b) { r = [s, JSON.parse(b).result.map(function (x) { return x.name; })]; }); return r; })()",
        );
        assert_eq!(r, r#"[200,["repo:app#fn:Alpha","repo:app#fn:alphabet"]]"#);
        let none = p.eval_json(
            "(function () { var r; window.KW_TRANSPORT('/api/v1/find?text=zzz', function (s, b) { r = JSON.parse(b).result.length; }); return r; })()",
        );
        assert_eq!(none, "0");
        // The search box reaches the same list.
        p.navigate("#/search?q=alph", &NoBackend);
        let t = p.text();
        assert!(
            t.contains("repo:app#fn:Alpha") && t.contains("repo:app#fn:alphabet"),
            "{t}"
        );
        assert!(!t.contains("repo:app#fn:beta"), "{t}");
    }

    #[test]
    fn slider_says_fixed() {
        let mut p = Page::open_file(&single_file(&canned(false)), "#/graph");
        let t = p.text();
        assert!(
            t.contains("This file is fixed at one position, so there is no history to show."),
            "{t}"
        );
        assert!(!p.has("slider"));
        assert!(!p.requests().iter().any(|r| r.contains("timeline")));
    }

    #[test]
    fn positions_and_cut_are_noted() {
        let mut d = canned(false);
        d.partial = vec![json!({"at": "export.symbols", "cap": "limit", "shown": 2, "total": 9})];
        let mut p = Page::open_file(&single_file(&d), "#/health");
        let t = p.text();
        assert!(t.contains("Positions: app@0123456789ab"), "{t}");
        assert!(
            t.contains("This export holds the 2 most connected of 9 symbols."),
            "{t}"
        );
        assert!(p.has("positions"));
    }
}
