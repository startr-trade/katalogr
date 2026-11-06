//! The drawing helpers: elements, diagrams and the explorer (FEAT-033 T11).

mod web_app {
    use katalogr_webtest::{MapBackend, Page};

    const HOSTILE: &str = "<img src=x onerror=alert(1)>";

    fn page() -> Page {
        Page::open("#/graph", &MapBackend::new())
    }

    /// R-9. Pele failing on a block shows that block's source and the error, not a blank.
    #[test]
    fn diagram_failure_shows_source() {
        let mut p = page();
        p.eval_json(
            "(function () {
               var host = KW.dom.h('div', {}, []);
               document.getElementById('kw-main').appendChild(host);
               window.okDrawn = KW.diagram.draw(host, 'flowchart LR\\n A-->B');
               window.failDrawn = KW.diagram.draw(host, 'flowchart LR\\n FAILS-->B');
             })()",
        );
        assert_eq!(p.eval_json("window.okDrawn"), "true");
        assert_eq!(p.eval_json("window.failDrawn"), "false");
        // The failing draw replaced the good one with the source and the message.
        assert!(p.has("diagram-error"));
        assert!(!p.has("diagram"));
        let text = p.text();
        assert!(
            text.contains("Diagram could not be drawn: pele: cannot draw FAILS"),
            "{text}"
        );
        assert!(text.contains("flowchart LR\n FAILS-->B"), "{text}");
        assert_eq!(p.pele_sources().len(), 2);
        // Pele's SVG is the one markup write, from the good draw.
        assert_eq!(p.markup_writes(), ["<svg></svg>"]);
    }

    /// R-16. Hostile text is shown as text: no markup write, no handler attribute, no script URL.
    #[test]
    fn dom_sets_text_only() {
        let mut p = page();
        let js = format!(
            "(function () {{
               var bad = {hostile:?};
               var m = document.getElementById('kw-main');
               var a = KW.dom.h('a', {{href: 'javascript:alert(1)', onclick: 'alert(1)', 'data-kw': 'bad-link'}}, [bad]);
               m.appendChild(a);
               m.appendChild(KW.dom.table([{{title: bad, path: 'name', symbol_link: true}}], [{{name: bad}}]));
               m.appendChild(KW.dom.notice(bad, 'warn', [bad]));
               var host = KW.dom.h('div', {{}}, []);
               m.appendChild(host);
               var graph = {{title: bad, shown: 1, total: 1, edges: [],
                 nodes: [{{id: 'n1', name: bad, label: '', kind: 'fn', path: bad, group: bad}}]}};
               window.explorer = KW.explore.open(host, graph, {{highlight: bad}});
               __cyFire('tap', {{id: 'n1', name: bad, label: bad, kind: 'fn', path: bad}});
               return a.getAttribute('href') === null && a.getAttribute('onclick') === null;
             }})()",
            hostile = HOSTILE
        );
        assert_eq!(p.eval_json(&js), "true");
        assert!(p.markup_writes().is_empty(), "{:?}", p.markup_writes());
        let text = p.text();
        assert!(text.contains(HOSTILE), "{text}");
        // The explorer got the hostile name as data only.
        assert_eq!(
            p.eval_json(
                "__cy[0].filter(function (e) { return e.classes === 'highlight'; }).length"
            ),
            "1"
        );
        assert_eq!(p.eval_json("__cy[0][0].data.parent === undefined"), "true");
        assert!(p.has("explore-symbol"));
    }

    /// The explorer maps a ViewGraph to elements and merges a double tap's answer.
    #[test]
    fn explore_maps_and_expands() {
        let answer = r#"{"graph":{"title":"t","shown":2,"total":2,"nodes":[{"id":"n1","name":"a/b/c","label":"","kind":"fn","path":null,"group":null},{"id":"n2","name":"d","label":"D","kind":"fn","path":null,"group":null}],"edges":[{"from":"n2","to":"n1","kind":"calls","inferred":true}]}}"#;
        let b = MapBackend::new().with(
            "/api/v1/views/callers?depth=1&symbol=a%2Fb%2Fc",
            200,
            answer,
        );
        let mut p = Page::open("#/graph", &b);
        p.eval_json(
            "(function () {
               var host = KW.dom.h('div', {}, []);
               document.getElementById('kw-main').appendChild(host);
               var g = {title: 't', shown: 1, total: 1, edges: [], nodes: [{id: 'n1', name: 'a/b/c', label: '', kind: 'fn', path: null, group: null}]};
               KW.explore.open(host, g, {get: window.KW_TRANSPORT});
             })()",
        );
        // The label is the last two segments of the name.
        assert_eq!(p.eval_json("__cy[0][0].data.label"), r#""b/c""#);
        p.eval_json("(function () { __cyFire('tap', {id: 'n1', name: 'a/b/c'}); __cyFire('tap', {id: 'n1', name: 'a/b/c'}); })()");
        // Drain the request the double tap queued.
        p.settle(&b);
        assert!(
            p.requests()
                .contains(&"/api/v1/views/callers?depth=1&symbol=a%2Fb%2Fc".to_string())
        );
        // n1 was already drawn, so only n2 and the dashed edge are added.
        assert_eq!(
            p.eval_json("__cyAdded[0].map(function (e) { return e.data.id.slice(0, 4) + (e.classes || ''); })"),
            r#"["n2","edgeinferred"]"#
        );
        assert!(!b.missed().iter().any(|m| m.contains("callers")));
    }
}
