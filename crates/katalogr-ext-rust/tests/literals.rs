//! FEAT-017: string literals offered to guest extensions, with their call context.

mod rust {
    use katalogr_ext_rust::RustExtractor;
    use katalogr_model::{ExtractInput, Extraction, Extractor, Name, RelPath};

    fn run(src: &str) -> Extraction {
        let path = RelPath::new("src/lib.rs").unwrap();
        RustExtractor
            .extract_full(&ExtractInput {
                repo: "r",
                path: &path,
                revision: "rev",
                content: src.as_bytes(),
                context: &[],
            })
            .unwrap()
    }

    fn offers(src: &str) -> Vec<(String, Option<(String, u32)>)> {
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
        let src = r##"const Q: &str = "SELECT id FROM items WHERE a = 1";
fn plain() {
    let _ = r#"SELECT raw FROM raw_t"#;
    let _ = b"bytes are not offered";
}
struct S;
impl S {
    fn method(&self) -> &str {
        "UPDATE items SET a = 1"
    }
}
fn mac() {
    let _ = format!("select {} from t where x = 1", 3);
}
fn query_it() {
    sqlx::query!("SELECT 1 FROM t", id);
}
"##;
        let x = run(src);
        assert!(x.embeds_nothing_extra());
        let texts: Vec<&str> = x.embedded.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "SELECT id FROM items WHERE a = 1",
                "SELECT raw FROM raw_t",
                "UPDATE items SET a = 1",
                "select {} from t where x = 1",
                "SELECT 1 FROM t",
            ]
        );
        let owner_of = |text: &str| {
            let id = x.embedded.iter().find(|e| e.text == text).unwrap().owner;
            x.fragment
                .symbols
                .iter()
                .find(|s| s.name.id() == id)
                .map(|s| s.name.canonical().split_once('#').unwrap().1.to_string())
                .unwrap_or_else(|| "<artifact>".to_string())
        };
        assert_eq!(owner_of("SELECT id FROM items WHERE a = 1"), "const:Q");
        assert_eq!(owner_of("UPDATE items SET a = 1"), "impl:S/fn:method");
        assert_eq!(owner_of("SELECT raw FROM raw_t"), "fn:plain");
        assert_eq!(owner_of("SELECT 1 FROM t"), "fn:query_it");
        // the span covers the literal with its delimiters
        let e = &x.embedded[0];
        assert_eq!(
            (e.span.start_line, e.span.start_col, e.span.end_line),
            (1, 17, 1)
        );
        assert!(e.holes.is_empty());
        // the manifest and non-literal files offer nothing
        assert!(offers("fn f() { let x = 1; }").is_empty());
    }

    trait Quiet {
        fn embeds_nothing_extra(&self) -> bool;
    }
    impl Quiet for Extraction {
        fn embeds_nothing_extra(&self) -> bool {
            self.errors.is_empty()
        }
    }

    #[test]
    fn literal_call_context() {
        let one = |body: &str| offers(&format!("fn f() {{ {body} }}"));
        assert_eq!(
            one(r#"sqlx::query("SELECT 1 FROM t");"#),
            vec![("SELECT 1 FROM t".into(), call("sqlx::query", 0))]
        );
        // offered although it has no whitespace
        assert_eq!(
            one(r#"client.get("/orders");"#),
            vec![("/orders".into(), call("client.get", 0))]
        );
        assert_eq!(
            one(r#"self.http.post(a, "/x");"#),
            vec![("/x".into(), call("self.http.post", 1))]
        );
        assert_eq!(
            one(r#"Client::new().get("/y");"#),
            vec![("/y".into(), call(".get", 0))]
        );
        assert_eq!(
            one(r#"Vec::<u8>::with("a b c d e");"#),
            vec![("a b c d e".into(), call("Vec::with", 0))]
        );
        assert_eq!(
            one(r#"sqlx::query!("SELECT 1 FROM t", id);"#),
            vec![("SELECT 1 FROM t".into(), call("sqlx::query!", 0))]
        );
        assert_eq!(
            one(r#"format!("{}", "a b c d e f");"#),
            vec![
                ("{}".into(), call("format!", 0)),
                ("a b c d e f".into(), call("format!", 1)),
            ]
        );
        assert_eq!(
            one(r#"let s = "x y z w v u";"#),
            vec![("x y z w v u".into(), None)]
        );
        assert_eq!(
            one(r#"f(&("a b c d e f"));"#),
            vec![("a b c d e f".into(), call("f", 0))]
        );
        // a literal nested in a call is the argument of the inner call only
        assert_eq!(
            one(r#"f(g("a b c d e f"));"#),
            vec![("a b c d e f".into(), call("g", 0))]
        );
        // a literal nested in a binary expression or a receiver has no call context
        assert_eq!(
            one(r#"f("a b c d e".to_string() + "f g h i j");"#),
            vec![("a b c d e".into(), None), ("f g h i j".into(), None)]
        );
        assert_eq!(
            one(r#"f(["a b c d e f"]);"#),
            vec![("a b c d e f".into(), None)]
        );
    }
}
