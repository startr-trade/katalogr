//! The single-file export's file (FEAT-034 T2): what `single_file` writes, and the static
//! transport running under the harness.

mod support;

mod web_export {
    use super::support::*;
    use katalogr_web::export::{
        CSP, PlannedCall, PositionRef, answer_key, positions_agree, single_file, strip_volatile,
    };
    use katalogr_webtest::Page;
    use serde_json::json;

    /// The file's text with every script body and style body cut out.
    fn markup_only(file: &str) -> String {
        let mut out = String::new();
        let mut rest = file;
        while let Some(i) = rest.find("<script") {
            out.push_str(&rest[..i]);
            let tail = &rest[i..];
            let open_end = tail.find('>').unwrap() + 1;
            out.push_str(&tail[..open_end]);
            let close = tail.find("</script>").expect("script closes");
            rest = &tail[close..];
        }
        out.push_str(rest);
        let mut plain = String::new();
        let mut rest = out.as_str();
        while let Some(i) = rest.find("<style>") {
            plain.push_str(&rest[..i + 7]);
            let close = i + rest[i..].find("</style>").expect("style closes");
            rest = &rest[close..];
        }
        plain.push_str(rest);
        plain
    }

    fn style_bodies(file: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut rest = file;
        while let Some(i) = rest.find("<style>") {
            let body = &rest[i + 7..];
            let end = body.find("</style>").expect("style closes");
            out.push(&body[..end]);
            rest = &body[end..];
        }
        out
    }

    #[test]
    fn file_is_self_contained() {
        let file = single_file(&canned(false));
        let markup = markup_only(&file);
        assert!(!markup.contains("src="), "a src attribute");
        for (i, _) in markup.match_indices("href=") {
            assert!(
                markup[i..].starts_with("href=\"data:,\""),
                "a link: {}",
                &markup[i..i + 40]
            );
        }
        assert!(!markup.contains("url(") && !markup.contains("@import"));
        for css in style_bodies(&file) {
            assert!(
                !css.contains("url(") && !css.contains("@import"),
                "a stylesheet loads"
            );
        }
        // No script is linked, and the four markers are all filled.
        assert!(!file.contains("<!--KW:"));
        assert!(!file.contains("<script src"));
        assert!(!file.contains("<link rel=\"stylesheet\""));
        // The only URLs are the ones the asset tests allow: licences, the Pele banner, the SVG
        // namespace and the GraphML-free Cytoscape credits.
        let own = [
            "https://github.com/obsidianmd/pele",
            "http://www.w3.org/2000/svg",
            "http://engelschall.com",
            "http://en.wikipedia.org/wiki/MIT_License",
            "http://opensource.org/licenses/MIT",
        ];
        for (i, _) in file.match_indices("http") {
            let rest = &file[i..];
            if rest.starts_with("http://") || rest.starts_with("https://") {
                assert!(
                    own.iter().any(|u| rest.starts_with(u)),
                    "a URL: {}",
                    &rest[..rest.len().min(60)]
                );
            }
        }
        // The licences travel with the inlined libraries.
        assert!(file.contains("Copyright (c) 2016-2026, The Cytoscape Consortium."));
        assert!(file.contains("Permission is hereby granted"));
    }

    #[test]
    fn csp_blocks_requests() {
        let file = single_file(&canned(false));
        let meta = format!("<meta http-equiv=\"Content-Security-Policy\" content=\"{CSP}\">");
        assert!(file.contains(&meta));
        assert!(CSP.starts_with("default-src 'none'"));
        assert!(!CSP.contains("connect-src") && !CSP.contains("http"));
        assert!(CSP.contains("base-uri 'none'") && CSP.contains("form-action 'none'"));
        // The policy comes before any script.
        assert!(file.find(&meta).unwrap() < file.find("<script").unwrap());
    }

    #[test]
    fn opens_with_no_server() {
        let file = single_file(&canned(false));
        let mut p = Page::open_file(&file, "#/health");
        let t = p.text();
        assert!(
            t.contains("Hubs") && t.contains("repo:app#fn:target"),
            "{t}"
        );
        assert!(t.contains("Impact") || t.contains("Cycles"), "{t}");
        // Only the static transport was asked, and nothing missed.
        let reqs = p.requests();
        assert!(reqs.iter().any(|r| r.starts_with("/api/v1/meta")));
        assert!(!t.contains("This isn't in this export"), "{t}");
        p.navigate("#/graph", &katalogr_webtest::NoBackend);
        assert!(p.has("view-body"));
        assert_eq!(p.pele_sources().len(), 1);
    }

    #[test]
    fn inlines_pele_and_theme() {
        let file = single_file(&canned(false));
        let tags = katalogr_render::assets::head_tags(None);
        assert!(
            file.contains(&tags),
            "the same inlined tags as every other page"
        );
        assert!(file.contains("--pele-bg") && file.contains("--pele-accent"));
        assert!(!file.contains("href=\"pele") && !file.contains("src=\"pele"));
        assert!(file.contains("data-kw=\"theme-button\""));
    }

    #[test]
    fn answers_are_sorted() {
        let a = canned(false);
        // The same answers inserted in the other order give the same bytes.
        let mut b = a.clone();
        b.answers = a.answers.clone().into_iter().rev().collect();
        assert_eq!(single_file(&a), single_file(&b));
        let file = single_file(&a);
        let start = file.find("id=\"kw-data\">").unwrap();
        let data = &file[start..file[start..].find("</script>").unwrap() + start];
        let keys: Vec<&String> = a.answers.keys().collect();
        let mut last = 0;
        for k in keys {
            let enc = serde_json::to_string(k).unwrap().replace('<', "\\u003c");
            let at = data
                .find(&format!("{enc}:{{"))
                .unwrap_or_else(|| panic!("{k} missing"));
            assert!(at >= last, "{k} out of order");
            last = at;
        }
        for forbidden in ["generated", "timestamp", "date", "hostname"] {
            assert!(!data.contains(&format!("\"{forbidden}\"")), "{forbidden}");
        }
        let mut body =
            json!({"watermark": 3, "context": "x", "pending": 0, "result": 1, "exact": true});
        strip_volatile(&mut body);
        assert_eq!(body, json!({"result": 1, "exact": true}));
        let mut other = json!([1]);
        strip_volatile(&mut other);
        assert_eq!(other, json!([1]));
    }

    #[test]
    fn mixed_positions_refused() {
        let p = |c: &str| PositionRef {
            repo: "app".into(),
            commit: Some(c.into()),
            dirty: None,
        };
        let q = |r: &str| PositionRef {
            repo: r.into(),
            commit: Some("a".into()),
            dirty: None,
        };
        assert!(positions_agree(&[]));
        assert!(positions_agree(&[vec![p("a")], vec![p("a")]]));
        assert!(!positions_agree(&[vec![p("a")], vec![p("b")]]));
        assert!(positions_agree(&[
            vec![q("x"), q("y")],
            vec![q("y"), q("x")]
        ]));
        assert!(!positions_agree(&[vec![q("x"), q("y")], vec![q("x")]]));
    }

    #[test]
    fn answer_key_matches_js() {
        let table: Vec<(&str, Vec<(&str, &str)>)> = vec![
            ("explain", vec![("symbol", "repo:app#fn:target")]),
            (
                "views/callers",
                vec![("symbol", "a b&c=d"), ("depth", "2"), ("max-nodes", "60")],
            ),
            (
                "tests",
                vec![(
                    "text",
                    "100% \"quoted\" 'single' (paren) *star* ~tilde! +plus",
                )],
            ),
            ("why", vec![("location", "src/lib.rs:3")]),
            (
                "find",
                vec![("text", "caf\u{e9} \u{4e2d}\u{6587} \u{1f600}"), ("z", "")],
            ),
            ("views/", vec![]),
            (
                "odd",
                vec![
                    ("b", "1"),
                    ("B", "2"),
                    ("a_b", "3"),
                    ("a-b", "4"),
                    ("a.b", "5"),
                ],
            ),
            (
                "hostile",
                vec![
                    ("x", "</script><script>alert(1)</script>"),
                    ("y", "<!-- -->"),
                ],
            ),
            ("tabs", vec![("t", "a\tb\nc\r\n"), ("slash", "//\\\\")]),
        ];
        let mut p = Page::open_file(&single_file(&canned(false)), "#/health");
        for (op, params) in table {
            let call = PlannedCall {
                op: op.into(),
                params: params
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            };
            let js_params: serde_json::Map<String, serde_json::Value> = params
                .iter()
                .map(|(k, v)| (k.to_string(), json!(v)))
                .collect();
            let js = p.eval_json(&format!(
                "KW.core.apiPath({}, {})",
                serde_json::to_string(op).unwrap(),
                serde_json::Value::Object(js_params)
            ));
            let js: String = serde_json::from_str(&js).unwrap();
            assert_eq!(
                js.strip_prefix("/api/v1/").unwrap(),
                answer_key(&call),
                "{op}"
            );
        }
    }

    #[test]
    fn script_text_cannot_escape() {
        let hostile = "</script><script>alert(1)</script><!-- <img src=x onerror=alert(2)>";
        let mut d = canned(false);
        d.symbols = vec![
            json!({"name": hostile, "kind": "fn", "path": "src/</script>.rs",
                                "span": {"start_line": 1, "start_col": 0, "end_line": 1, "end_col": 1}}),
        ];
        d.repos = vec![hostile.to_string()];
        put(
            &mut d,
            "hubs",
            &[("limit", "20")],
            json!({"result": {"items": [
            {"node": {"name": hostile, "kind": "fn", "path": "p"}, "degree": 1, "in_degree": 0, "out_degree": 1, "rank": 1}]}}),
        );
        let file = single_file(&d);
        // Every `<script` has exactly one closing tag, and the hostile text never closes one.
        assert_eq!(
            file.matches("<script").count(),
            file.matches("</script>").count()
        );
        assert!(!file.contains(hostile));
        assert!(!file.contains("</script><script>alert"));
        // The data block reads back to what went in.
        let start = file.find("id=\"kw-data\">").unwrap() + "id=\"kw-data\">".len();
        let end = file[start..].find("</script>").unwrap() + start;
        let back: serde_json::Value = serde_json::from_str(&file[start..end]).unwrap();
        assert_eq!(back["symbols"][0]["name"], hostile);
        assert_eq!(back["repos"][0], hostile);
        // On the page the name is text, never markup.
        let mut p = Page::open_file(&file, "#/health");
        assert!(p.text().contains(hostile));
        assert!(
            p.markup_writes().iter().all(|m| !m.contains("alert")),
            "{:?}",
            p.markup_writes()
        );
    }
}
