//! Markdown and HTML of a diff report (FEAT-095 R-8, R-12 to R-14), from a synthetic report.

mod diff_render {
    use katalogr_dist::diff::DiffReport;
    use katalogr_dist::diff_render::{DIAGRAM_EDGES, diff_graph, to_html, to_markdown};
    use katalogr_render::{PELE_JS, head_tags, node_id, to_mermaid};
    use std::path::PathBuf;

    fn fixtures() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/diff")
    }

    fn report() -> DiffReport {
        let text = std::fs::read_to_string(fixtures().join("report.json")).unwrap();
        serde_json::from_str(&text).expect("the fixture is a report")
    }

    /// The Pele block has its own checksums; the golden holds a marker in its place.
    const PELE_MARKER: &str = "<!-- pele assets inlined here -->\n";

    fn golden_html(r: &DiffReport) -> String {
        to_html(r).replace(&head_tags(None), PELE_MARKER)
    }

    fn check_golden(name: &str, got: &str) {
        let path = fixtures().join(name);
        if std::env::var_os("KATALOGR_UPDATE_DIFF_GOLDEN").is_some() {
            std::fs::write(&path, got).unwrap();
            return;
        }
        let want = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            want == got,
            "{name} differs from the golden; rerun with KATALOGR_UPDATE_DIFF_GOLDEN=1 and review the diff"
        );
    }

    /// A report whose fixtures names are replaced by `name`, everywhere they occur.
    fn renamed(from: &str, name: &str) -> DiffReport {
        let text = std::fs::read_to_string(fixtures().join("report.json"))
            .unwrap()
            .replace(
                from,
                &name
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n"),
            );
        serde_json::from_str(&text).expect("still a report")
    }

    // ---------------------------------------------------------------- R-8

    #[test]
    fn no_time_in_any_format() {
        let r = report();
        let all = [
            serde_json::to_string(&r).unwrap(),
            to_markdown(&r),
            to_html(&r),
        ];
        let year = regex_lite_year();
        for text in &all {
            let body = text.replace(&head_tags(None), "");
            assert!(!year(&body), "a date or time appears");
        }
        // The same report gives the same bytes.
        assert_eq!(to_markdown(&r), to_markdown(&report()));
        assert_eq!(to_html(&r), to_html(&report()));
    }

    /// Whether `20nn-nn-nn` or `nn:nn:nn` appears.
    fn regex_lite_year() -> impl Fn(&str) -> bool {
        |s: &str| {
            let b = s.as_bytes();
            let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
            (0..b.len()).any(|i| {
                (digit(i)
                    && digit(i + 1)
                    && b.get(i + 2) == Some(&b':')
                    && digit(i + 3)
                    && digit(i + 4)
                    && b.get(i + 5) == Some(&b':')
                    && digit(i + 6))
                    || (b.get(i) == Some(&b'2')
                        && b.get(i + 1) == Some(&b'0')
                        && digit(i + 2)
                        && digit(i + 3)
                        && b.get(i + 4) == Some(&b'-')
                        && digit(i + 5)
                        && digit(i + 6)
                        && b.get(i + 7) == Some(&b'-')
                        && digit(i + 8))
            })
        }
    }

    // ---------------------------------------------------------------- R-12

    #[test]
    fn markdown_matches_golden() {
        check_golden("report.md", &to_markdown(&report()));
    }

    #[test]
    fn markdown_escapes_table_cells() {
        let r = renamed("fn parse(x: u8)", "a | b\nc");
        let md = to_markdown(&r);
        assert!(md.contains("a \\| b<br>c"), "{md}");
        // Every table row has the same number of unescaped bars as its header.
        for table in md.split("\n\n") {
            let rows: Vec<&str> = table.lines().filter(|l| l.starts_with('|')).collect();
            if rows.is_empty() {
                continue;
            }
            let bars = |l: &str| l.replace("\\|", "").matches('|').count();
            assert!(rows.iter().all(|l| bars(l) == bars(rows[0])), "{table}");
        }
    }

    // ---------------------------------------------------------------- R-13

    #[test]
    fn html_matches_golden() {
        check_golden("report.html", &golden_html(&report()));
    }

    #[test]
    fn html_has_no_external_reference() {
        let r = report();
        let page = golden_html(&r);
        assert!(!page.contains(" src="), "{page}");
        assert!(!page.contains("@import"));
        assert!(!page.contains("url("));
        assert!(!page.contains("http://") && !page.contains("https://"));
        // Every link goes to an anchor in the page.
        for part in page.split("href=\"").skip(1) {
            assert!(part.starts_with('#'), "{}", &part[..part.len().min(40)]);
        }
        // The inlined assets are tags with their text, never a reference.
        let tags = head_tags(None);
        assert!(!tags.contains("<link") && !tags.contains("<script src"));
    }

    #[test]
    fn html_policy_forbids_requests() {
        for r in [report(), no_links()] {
            let page = to_html(&r);
            let policy = "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:\">";
            assert!(page.contains(policy));
            assert!(page.find(policy).unwrap() < page.find("<style>").unwrap());
        }
    }

    #[test]
    fn pele_asset_has_no_fetch() {
        let js = String::from_utf8_lossy(PELE_JS);
        for banned in [
            "fetch(",
            "XMLHttpRequest",
            "importScripts",
            "eval(",
            "new Function",
        ] {
            assert!(!js.contains(banned), "pele.global.js holds {banned}");
        }
    }

    #[test]
    fn html_links_resolve_to_anchors() {
        let page = to_html(&report()).replace(&head_tags(None), "");
        let ids: Vec<&str> = page
            .split("id=\"")
            .skip(1)
            .filter_map(|p| p.split('"').next())
            .collect();
        let hrefs: Vec<&str> = page
            .split("href=\"#")
            .skip(1)
            .filter_map(|p| p.split('"').next())
            .collect();
        assert!(!hrefs.is_empty(), "the fixture links symbols");
        for h in &hrefs {
            assert!(ids.contains(h), "{h} has no anchor");
        }
        // The anchor of a symbol is built from its canonical name.
        let id = format!("s-{}", node_id("app:src/lib.tx#fn:parse"));
        assert!(ids.contains(&id.as_str()));
        // Ids are unique.
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
    }

    fn no_links() -> DiffReport {
        let mut r = report();
        r.diff.links.links_added.clear();
        r.diff.links.links_removed.clear();
        r
    }

    #[test]
    fn diagram_only_when_links_changed() {
        let with = to_html(&report());
        let without = to_html(&no_links());
        assert!(with.contains("language-mermaid"));
        assert!(with.len() > 100_000, "Pele is inlined with a diagram");
        assert!(!without.contains("language-mermaid"));
        assert!(!without.contains("<script"), "no script without a diagram");
        assert!(without.len() < 100_000, "{} bytes", without.len());
        assert!(diff_graph(&no_links()).is_none());
        // Edges are capped; the rest is counted.
        let mut many = report();
        many.diff.links.links_added = (0..60)
            .map(|i| katalogr_analytics::context_diff::LinkDelta {
                from: format!("app:f.tx#fn:a{i}"),
                kind: "calls".into(),
                to: format!("app:f.tx#fn:b{i}"),
            })
            .collect();
        let g = diff_graph(&many).unwrap();
        assert_eq!(g.edges.len(), DIAGRAM_EDGES);
        assert_eq!(g.shown, DIAGRAM_EDGES as u64);
        assert_eq!(g.total, 61);
        // Added links come first, so the one removed link is the one cut.
        assert!(g.edges.iter().all(|e| e.kind == "added calls"));
    }

    // ---------------------------------------------------------------- R-14

    const HOSTILE: &str = "<script>alert(1)</script>\"&'";

    #[test]
    fn names_are_escaped() {
        let r = renamed("app:src/new.tx#fn:fresh", HOSTILE);
        let page = to_html(&r).replace(&head_tags(None), "");
        assert!(!page.contains("<script>alert"), "raw script in the page");
        assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;&quot;&amp;&#39;"));
        // Metadata too: a hostile repo name.
        let mut r = report();
        r.repo = HOSTILE.to_string();
        let page = to_html(&r).replace(&head_tags(None), "");
        assert!(!page.contains("<script>alert"));
    }

    #[test]
    fn diagram_labels_are_escaped() {
        let r = renamed("app:src/new.tx#fn:fresh", "x\"]; end; A[\"<b>y");
        let g = diff_graph(&r).unwrap();
        let mermaid = to_mermaid(&g);
        // The label's quote cannot close the node's own label.
        assert!(!mermaid.contains("x\"];"), "{mermaid}");
        let page = to_html(&r).replace(&head_tags(None), "");
        assert!(!page.contains("<b>y"), "an unescaped tag reached the page");
    }
}
