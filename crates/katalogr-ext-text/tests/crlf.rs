//! FEAT-141: a text file checked out with CRLF line endings (Windows, `core.autocrlf=true`)
//! gives the same chunks, with the same line spans and text, as its LF form.

mod text {
    use katalogr_ext_text::TextExtractor;
    use katalogr_model::{ExtractInput, Extractor, RelPath};

    /// The fragment's symbols, links, refs and exports, as debug text.
    fn run(bytes: &[u8]) -> Vec<String> {
        let path = RelPath::new("notes/a.txt").unwrap();
        let (f, errors) = TextExtractor
            .extract_with_errors(&ExtractInput {
                repo: "r",
                path: &path,
                revision: "rev",
                content: bytes,
                context: &[],
            })
            .expect("never Err");
        assert!(errors.is_empty(), "{errors:?}");
        vec![
            format!("{:#?}", f.symbols),
            format!("{:#?}", f.links),
            format!("{:#?}", f.refs),
            format!("{:#?}", f.exports),
        ]
    }

    /// FEAT-141. Headings mode and paragraph mode: CRLF symbols equal the LF ones.
    #[test]
    fn crlf_text_gives_lf_chunks_and_spans() {
        let headed = "intro line\n\n# One\nalpha\nbeta\n## Two ##\ngamma\n\ndelta\n";
        let plain = "first paragraph\nstill first\n\nsecond paragraph\n\n\nthird\n";
        for src in [headed, plain] {
            let lf = run(src.as_bytes());
            let crlf = run(src.replace('\n', "\r\n").as_bytes());
            assert_ne!(lf[0], "[]", "the text yields symbols");
            for (c, l) in crlf.iter().zip(&lf) {
                assert_eq!(c, l, "{src:?}");
            }
        }
    }
}
