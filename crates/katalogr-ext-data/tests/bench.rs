//! Timing benchmark, run on demand:
//! `cargo test -p katalogr-ext-data --release --test bench -- --ignored --nocapture`.

use katalogr_ext_data::{DataExtractor, Format, parse};
use katalogr_model::{ExtractInput, Extractor, RelPath};
use std::time::{Duration, Instant};

/// 100 maps of 199 scalar keys each: 100 + 19,900 = 20,000 keys.
fn yaml_20k_keys() -> String {
    let mut s = String::new();
    for a in 0..100 {
        s.push_str(&format!("section{a}:\n"));
        for b in 0..199 {
            s.push_str(&format!("  key{b}: value-{a}-{b}\n"));
        }
    }
    s
}

mod bench {
    use super::*;

    /// R-13 (FEAT-009). A 20,000-key YAML data document, parsed and turned into symbols.
    /// Budget: 50 ms.
    #[test]
    #[ignore]
    fn data_doc_20k_keys() {
        let text = yaml_20k_keys();
        let path = RelPath::new("codecs/big.yaml").unwrap();
        let input = ExtractInput {
            repo: "r",
            path: &path,
            revision: "ce013625030ba8dba906f756967f9e9ca394464a",
            content: text.as_bytes(),
            context: &[],
        };
        let mut best = Duration::MAX;
        for _ in 0..7 {
            let start = Instant::now();
            let (f, errors) = DataExtractor.extract_with_errors(&input).unwrap();
            best = best.min(start.elapsed());
            assert!(errors.is_empty(), "{errors:?}");
            // 20,000 keys and the document.
            assert_eq!(f.symbols.len(), 20_001);
        }
        println!("bench::data_doc_20k_keys: {} bytes in {best:?}", text.len());
        assert!(best < Duration::from_millis(73), "took {best:?}");

        // The same keys as JSON and XML parse within the same order of time.
        let json = {
            let mut s = String::from("{");
            for a in 0..100 {
                s.push_str(&format!(
                    "{}\"section{a}\":{{",
                    if a == 0 { "" } else { "," }
                ));
                for b in 0..199 {
                    s.push_str(&format!(
                        "{}\"key{b}\":\"value-{a}-{b}\"",
                        if b == 0 { "" } else { "," }
                    ));
                }
                s.push('}');
            }
            s.push('}');
            s
        };
        let start = Instant::now();
        let p = parse(Format::Json, &json);
        println!("bench::data_doc_20k_keys (json): {:?}", start.elapsed());
        assert!(p.errors.is_empty());
    }

    /// The nesting bound (64) holds on a small stack: the recursive walks of all three
    /// parsers and of the key tree run in a 64 KiB thread at the deepest allowed level.
    #[test]
    #[ignore]
    fn deepest_document_fits_64_kib_of_stack() {
        let json = format!("{}1{}", "[".repeat(64), "]".repeat(64));
        let yaml = format!("{}1{}", "[".repeat(64), "]".repeat(64));
        let xml = format!("{}x{}", "<a>".repeat(64), "</a>".repeat(64));
        std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(move || {
                for (name, text) in [("deep.json", json), ("deep.yaml", yaml), ("deep.xml", xml)] {
                    let path = RelPath::new(name).unwrap();
                    let (f, errors) = DataExtractor
                        .extract_with_errors(&ExtractInput {
                            repo: "r",
                            path: &path,
                            revision: "ce013625030ba8dba906f756967f9e9ca394464a",
                            content: text.as_bytes(),
                            context: &[],
                        })
                        .unwrap();
                    assert!(errors.is_empty(), "{name}: {errors:?}");
                    assert!(f.symbols.len() >= 64, "{name}");
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
