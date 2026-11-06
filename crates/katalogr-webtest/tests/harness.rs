//! The harness itself (FEAT-033 T5).

mod harness {
    use katalogr_webtest::{Backend, MapBackend, Page};

    #[test]
    fn map_backend_records_misses() {
        let b = MapBackend::new().with("/api/v1/meta", 200, "{\"ok\":true}");
        assert_eq!(b.get("/api/v1/meta"), (200, "{\"ok\":true}".to_string()));
        assert!(b.missed().is_empty());
        let (status, body) = b.get("/api/v1/nope?a=1");
        assert_eq!(status, 404);
        assert!(body.contains("/api/v1/nope?a=1"));
        assert_eq!(b.missed(), ["/api/v1/nope?a=1"]);
    }

    #[test]
    fn fake_dom_collects_text() {
        let b = MapBackend::new().with("/ping", 200, "pong");
        let mut p = Page::open("#/graph", &b);
        // The page drew its failure notice for the missing capabilities; clear it.
        p.eval_json("document.getElementById('kw-main').replaceChildren()");
        assert_eq!(p.text(), "");
        // Build a small tree in <main>, make a request, and click a button.
        p.eval_json(
            "(function () {
               var m = document.getElementById('kw-main');
               var h = document.createElement('h2'); h.textContent = 'Title';
               var b = document.createElement('button'); b.setAttribute('data-kw', 'go'); b.textContent = 'Go';
               b.addEventListener('click', function () {
                 window.KW_TRANSPORT('/ping', function (s, body) {
                   var d = document.createElement('p'); d.textContent = s + ':' + body; m.appendChild(d);
                 });
               });
               var s = document.createElement('div'); s.innerHTML = '<b>x</b>';
               m.append(h, b, s);
               return 1;
             })()",
        );
        assert_eq!(p.text(), "Title\nGo");
        assert!(p.has("go"));
        assert!(!p.has("missing"));
        p.fire("go", "click", None, &b);
        assert_eq!(p.text(), "Title\nGo\n200:pong");
        assert_eq!(p.requests(), ["/api/v1/meta", "/ping"]);
        assert_eq!(p.markup_writes(), ["<b>x</b>"]);
        assert!(p.pele_sources().is_empty());
        assert_eq!(
            p.eval_json("pele.render('flowchart LR').svg"),
            "\"<svg></svg>\""
        );
        assert_eq!(p.pele_sources(), ["flowchart LR"]);
        assert!(b.missed().is_empty());
        // The manifest is in place for the page to read.
        assert_eq!(
            p.eval_json("JSON.parse(document.getElementById('kw-views').textContent).format"),
            "1"
        );
        p.navigate("#/service-map", &b);
        assert_eq!(p.eval_json("location.hash"), "\"#/service-map\"");
    }
}
