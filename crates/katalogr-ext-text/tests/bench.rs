//! Timing benchmarks, run on demand:
//! `cargo test -p katalogr-ext-text --release --test bench -- --ignored --nocapture`.

use katalogr_ext_text::TextExtractor;
use katalogr_model::{ExtractInput, Extractor, RelPath};
use std::time::{Duration, Instant};

/// About 1 MiB: 20,000 lines; every ten lines a blank line (blocks) or, with `headings`, a
/// heading every seven lines (3,000 headings).
fn generate(headings: bool) -> String {
    let mut s = String::new();
    let mut h = 0;
    for i in 0..20_000 {
        if headings && i % 6 == 0 && h < 3_000 {
            s.push_str(&format!("## Section {h} heading text here\n"));
            h += 1;
            continue;
        }
        if !headings && i % 10 == 9 {
            s.push('\n');
            continue;
        }
        s.push_str(&format!(
            "line {i} of deterministic prose for the benchmark\n"
        ));
    }
    while s.len() < 1024 * 1024 - 100 {
        s.push_str("padding words to reach about one mebibyte of synthetic text\n\n");
    }
    s.truncate(1024 * 1024);
    s
}

fn measure(name: &str, headings: bool, expect_chunks: std::ops::RangeInclusive<usize>) {
    let text = generate(headings);
    let path = RelPath::new("notes/big.txt").unwrap();
    let input = ExtractInput {
        repo: "r",
        path: &path,
        revision: "rev",
        content: text.as_bytes(),
        context: &[],
    };
    let mut best = Duration::MAX;
    for _ in 0..7 {
        let start = Instant::now();
        let (f, _) = TextExtractor.extract_with_errors(&input).unwrap();
        best = best.min(start.elapsed());
        assert!(
            expect_chunks.contains(&f.symbols.len()),
            "{}",
            f.symbols.len()
        );
    }
    println!("bench::{name}: {} bytes in {best:?}", text.len());
    assert!(best < Duration::from_millis(40), "took {best:?}");
}

mod bench {
    use super::*;

    /// R-11 (FEAT-066). A 1 MiB file in blank-line blocks. Budget: 40 ms.
    #[test]
    #[ignore]
    fn text_extract_1mib() {
        measure("text_extract_1mib", false, 1_000..=5_000);
    }

    /// R-11 (FEAT-066). A 1 MiB file with 3,000 headings. Budget: 40 ms.
    #[test]
    #[ignore]
    fn text_extract_1mib_headings() {
        measure("text_extract_1mib_headings", true, 3_000..=5_000);
    }
}
