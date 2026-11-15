//! FEAT-141: a source file checked out with CRLF line endings (Windows, `core.autocrlf=true`)
//! gives the same symbols, with the same line and column spans, as its LF form.

mod rust {
    use katalogr_ext_rust::RustExtractor;
    use katalogr_model::{ExtractInput, Extractor, RelPath};

    const SRC: &str = "//! Crate docs.\n\n/// A thing.\n/// Second line.\npub struct Thing {\n    pub a: u32,\n}\n\nimpl Thing {\n    /// Makes one.\n    pub fn new() -> Self {\n        Thing { a: helper(1) }\n    }\n}\n\nfn helper(x: u32) -> u32 {\n    let s = \"multi\nline\";\n    x + s.len() as u32\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn works() {\n        super::helper(2);\n    }\n}\n";

    fn run(src: &str) -> serde_json::Value {
        let path = RelPath::new("src/lib.rs").unwrap();
        let x = RustExtractor
            .extract_full(&ExtractInput {
                repo: "r",
                path: &path,
                revision: "rev",
                content: src.as_bytes(),
                context: &[],
            })
            .unwrap();
        serde_json::to_value(&x.fragment).unwrap()
    }

    /// FEAT-141. Symbols, links and refs of the CRLF file equal those of the LF file.
    #[test]
    fn crlf_source_gives_lf_symbols_and_spans() {
        let lf = run(SRC);
        let crlf = run(&SRC.replace('\n', "\r\n"));
        assert!(lf["symbols"].as_array().is_some_and(|s| s.len() > 3));
        for key in ["symbols", "links", "refs", "exports"] {
            assert_eq!(
                serde_json::to_string_pretty(&crlf[key]).unwrap(),
                serde_json::to_string_pretty(&lf[key]).unwrap(),
                "{key}"
            );
        }
    }
}
