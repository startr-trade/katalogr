//! `KW.core`, the page's pure helpers (FEAT-033 T10).

mod web_app {
    use katalogr_webtest::{MapBackend, Page};

    fn page() -> Page {
        Page::open("#/graph", &MapBackend::new())
    }

    #[test]
    fn route_round_trips() {
        let mut p = page();
        assert_eq!(
            p.eval_json("KW.core.parseRoute('#/graph?at=A&symbol=B')"),
            r#"{"view":"graph","params":{"at":"A","symbol":"B"}}"#
        );
        // Values are percent-encoded both ways, and unknown parameters survive.
        assert_eq!(
            p.eval_json("KW.core.buildLink('service-map', {symbol: 'a b&c', at: 'v1', zz: ''})"),
            r##""#/service-map?at=v1&symbol=a%20b%26c""##
        );
        assert_eq!(
            p.eval_json(
                "KW.core.parseRoute(KW.core.buildLink('graph', {symbol: 'a b&c/d', new: 'x'}))"
            ),
            r#"{"view":"graph","params":{"new":"x","symbol":"a b&c/d"}}"#
        );
        assert_eq!(p.eval_json("KW.core.parseRoute('').view"), r#""""#);
        assert_eq!(
            p.eval_json(
                "KW.core.apiPath('views/callers', {symbol: 'x y', depth: 2, 'max-nodes': 60})"
            ),
            r#""/api/v1/views/callers?depth=2&max-nodes=60&symbol=x%20y""#
        );
        assert_eq!(
            p.eval_json("KW.core.apiPath('meta', {})"),
            r#""/api/v1/meta""#
        );
    }

    #[test]
    fn core_helpers_behave() {
        let mut p = page();
        assert_eq!(
            p.eval_json("KW.core.viewState({needs: 'impact'}, ['views'])"),
            r#""absent""#
        );
        assert_eq!(
            p.eval_json("KW.core.viewState({needs: 'impact'}, ['impact'])"),
            r#""ready""#
        );
        assert_eq!(
            p.eval_json("KW.core.viewState({needs: null}, [])"),
            r#""ready""#
        );
        assert_eq!(
            p.eval_json("KW.core.statusMessage(0, '').text"),
            r#""katalogr isn't answering. Is katalogr serve still running?""#
        );
        assert_eq!(
            p.eval_json(
                "KW.core.statusMessage(409, {error: 'position-unavailable', available: [{repo: 'app', commit: 'abcdef0123456789', dirty: null}]})"
            ),
            r#"{"text":"This position isn't held here. Held positions:","detail":"","items":["app@abcdef012345"]}"#
        );
        assert_eq!(
            p.eval_json("KW.core.statusMessage(404, '{\"message\":\"no symbol x\"}')"),
            r#"{"text":"Nothing matches this.","detail":"no symbol x","items":[]}"#
        );
        assert_eq!(
            p.eval_json("KW.core.statusMessage(500, '{\"message\":\"boom\"}').text"),
            r#""katalogr reported a problem:""#
        );
        assert_eq!(
            p.eval_json("KW.core.answerNotes({exact: false, partial: [{shown: 5, total: 9}]})"),
            r#"["The index is still catching up, so this may not match the current code.","Showing 5 of 9."]"#
        );
        assert_eq!(p.eval_json("KW.core.answerNotes({exact: true})"), "[]");
        assert_eq!(
            p.eval_json("KW.core.cell({a: {b: [{name: 'x'}, {name: 'y'}]}, n: 3}, {path: 'a.b', join: 'name'})"),
            r#""x, y""#
        );
        assert_eq!(p.eval_json("KW.core.cell({n: 3}, {path: 'n'})"), r#""3""#);
        assert_eq!(p.eval_json("KW.core.cell({n: 3}, {path: 'a.b'})"), r#""""#);
        assert_eq!(
            p.eval_json(
                "KW.core.pointsFor({result: {points: [{view_id: 'a', query: {op: 'structure', args: {level: 'file'}}}, {view_id: 'b', query: {op: 'structure', args: {level: 'package'}}}]}}, {op: 'structure', args: {level: 'package'}}).map(function (x) { return x.view_id; })"
            ),
            r#"["b"]"#
        );
        assert_eq!(
            p.eval_json(
                "KW.core.requirementsOf({result: {rationale: [{kind: 'refs-item', target: 'REQ-2', label: 'two'}, {kind: 'links-to', target: 'X', label: ''}], commits: [{items: ['REQ-1']}]}}, {result: [{requirements: ['REQ-2', 'REQ-3']}]})"
            ),
            r#"[{"name":"REQ-1","label":""},{"name":"REQ-2","label":"two"},{"name":"REQ-3","label":""}]"#
        );
    }
}
