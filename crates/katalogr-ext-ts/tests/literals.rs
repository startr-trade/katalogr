//! FEAT-017: string and template literals offered to guest extensions, with their call context.

mod ts {
    use katalogr_ext_ts::TsExtractor;
    use katalogr_model::{ExtractInput, Extraction, Extractor, Name, RelPath};

    fn run(src: &str) -> Extraction {
        let path = RelPath::new("src/a.ts").unwrap();
        TsExtractor
            .extract_full(&ExtractInput {
                repo: "r",
                path: &path,
                revision: "rev",
                content: src.as_bytes(),
                context: &[],
            })
            .unwrap()
    }

    type Offer = (String, Option<(String, u32)>);

    fn offers(src: &str) -> Vec<Offer> {
        run(src)
            .embedded
            .into_iter()
            .map(|e| (e.text, e.call.map(|c| (c.callee, c.arg))))
            .collect()
    }

    fn call(callee: &str, arg: u32) -> Option<(String, u32)> {
        Some((callee.to_string(), arg))
    }

    #[test]
    fn string_literals_offered() {
        let src = r#"export function load(id: number) {
  return `SELECT * FROM orders WHERE a = ${id} AND b = ${id + 1}`;
}
const tagged = sql`SELECT x FROM items`;
function outer() {
  const inner = () => "UPDATE items SET a = 1";
  return inner;
}
"#;
        let x = run(src);
        assert!(x.errors.is_empty(), "{:?}", x.errors);
        assert_eq!(x.embedded.len(), 3, "{:?}", x.embedded);
        let t = &x.embedded[0];
        assert_eq!(t.text, "SELECT * FROM orders WHERE a =  AND b = ");
        assert_eq!(t.holes, vec![31, 40]);
        assert!(t.call.is_none());
        let owner_of = |i: usize| {
            let id = x.embedded[i].owner;
            x.fragment
                .symbols
                .iter()
                .find(|s| s.name.id() == id)
                .map(|s| s.name.canonical().split_once('#').unwrap().1.to_string())
                .unwrap_or_else(|| "<artifact>".to_string())
        };
        assert_eq!(owner_of(0), "fn:load");
        // the tagged template is the argument of its tag
        let tagged = &x.embedded[1];
        assert_eq!(tagged.text, "SELECT x FROM items");
        assert_eq!(tagged.call.as_ref().map(|c| c.callee.as_str()), Some("sql"));
        // an arrow function inside a named function is owned by that function
        assert_eq!(x.embedded[2].text, "UPDATE items SET a = 1");
        assert_eq!(owner_of(2), "fn:outer");
        assert_eq!(
            (t.span.start_line, t.span.start_col),
            (2, 10),
            "{:?}",
            t.span
        );
        // plain files offer nothing
        assert!(offers("export const n = 1;").is_empty());
    }

    #[test]
    fn literal_call_context() {
        let o = |body: &str| offers(body);
        assert_eq!(
            o(r#"fetch("/orders");"#),
            vec![("/orders".into(), call("fetch", 0))]
        );
        assert_eq!(
            o("axios.get(`/o/${id}`);"),
            vec![("/o/".into(), call("axios.get", 0))]
        );
        assert_eq!(
            o(r#"class A { m() { this.http.post(u, "/x"); } }"#),
            vec![("/x".into(), call("this.http.post", 1))]
        );
        assert_eq!(
            o(r#"api().get("/y");"#),
            vec![("/y".into(), call(".get", 0))]
        );
        assert_eq!(
            o(r#"new URL("/z", base);"#),
            vec![("/z".into(), call("new URL", 0))]
        );
        assert_eq!(
            o("sql`SELECT * FROM t`;"),
            vec![("SELECT * FROM t".into(), call("sql", 0))]
        );
        assert_eq!(o(r#"f("/a" as const);"#), vec![("/a".into(), call("f", 0))]);
        assert_eq!(o(r#"f(...xs, "/b");"#), vec![("/b".into(), call("f", 1))]);
        assert_eq!(
            o(r#"f({url: "a b c d e f"});"#),
            vec![("a b c d e f".into(), None)]
        );
        // a short literal inside an object is not offered at all
        assert!(o(r#"f({url: "/c"});"#).is_empty());
        assert_eq!(
            o(r#"const s = "x y z w v u";"#),
            vec![("x y z w v u".into(), None)]
        );
        // parentheses and non-null wrappers are transparent; nesting is not
        assert_eq!(o(r#"f(("/p"));"#), vec![("/p".into(), call("f", 0))]);
        assert_eq!(o(r#"f(g("/q"));"#), vec![("/q".into(), call("g", 0))]);
        assert_eq!(
            o(r#"f(["a b c d e f"]);"#),
            vec![("a b c d e f".into(), None)]
        );
    }
}
