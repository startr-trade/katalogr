//! FEAT-066 text-fallback unit tests.

mod text {
    use katalogr_ext_text::chunk::{Mode, split};
    use katalogr_ext_text::decode::{Encoding, decode, is_binary};
    use katalogr_ext_text::{
        BINARY_SNIFF_BYTES, MAX_CHUNK_LINES, MAX_CHUNKS, MAX_INPUT_BYTES, MAX_TEXT_BYTES,
        TextExtractor,
    };
    use katalogr_model::{ExtractError, ExtractInput, Extractor, Fact, Fragment, RelPath};
    use proptest::prelude::*;

    fn run(bytes: &[u8]) -> (Fragment, Vec<ExtractError>) {
        let path = RelPath::new("notes/a.txt").unwrap();
        TextExtractor
            .extract_with_errors(&ExtractInput {
                repo: "r",
                path: &path,
                revision: "rev",
                content: bytes,
                context: &[],
            })
            .expect("never Err")
    }

    fn str_fact<'a>(f: &'a std::collections::BTreeMap<String, Fact>, k: &str) -> Option<&'a str> {
        match f.get(k) {
            Some(Fact::Str(s)) => Some(s),
            _ => None,
        }
    }

    #[test]
    fn handles_nothing() {
        for p in ["a.txt", "README.md", "x.adoc", "notes/a.log", "Makefile"] {
            assert!(!TextExtractor.handles(&RelPath::new(p).unwrap()), "{p}");
        }
    }

    #[test]
    fn headings_split_sections() {
        let s = split("intro\n\n# One\nalpha\n## Two ##\nbeta\n\ngamma\n###### Six\n");
        assert_eq!(s.mode, Mode::Headings);
        let v: Vec<_> = s
            .chunks
            .iter()
            .map(|c| (c.start_line, c.end_line, c.heading.clone()))
            .collect();
        assert_eq!(
            v,
            vec![
                (1, 1, None),
                (3, 4, Some((1, "One".to_string()))),
                (5, 8, Some((2, "Two".to_string()))),
                (9, 9, Some((6, "Six".to_string()))),
            ]
        );
        assert_eq!(s.chunks[2].text, "## Two ##\nbeta\n\ngamma");
        // Not headings: no space, seven hashes, four leading spaces, empty title.
        let s = split("#nope\n####### seven\n    # indented\n#\n");
        assert_eq!(s.mode, Mode::Blocks);
    }

    #[test]
    fn fenced_hash_lines_are_not_headings() {
        let s = split("# Real\n```sh\n# comment\n```\n~~~\n## also not\n~~~\n## After\n");
        assert_eq!(s.mode, Mode::Headings);
        let titles: Vec<_> = s
            .chunks
            .iter()
            .filter_map(|c| c.heading.as_ref().map(|h| h.1.as_str()))
            .collect();
        assert_eq!(titles, vec!["Real", "After"]);
    }

    #[test]
    fn blank_line_blocks() {
        let s = split("one\ntwo\n\n\n  \nthree\n\nfour\nfive\n");
        assert_eq!(s.mode, Mode::Blocks);
        let v: Vec<_> = s
            .chunks
            .iter()
            .map(|c| (c.start_line, c.end_line, c.text.as_str()))
            .collect();
        assert_eq!(
            v,
            vec![(1, 2, "one\ntwo"), (6, 6, "three"), (8, 9, "four\nfive")]
        );
        assert!(split("").chunks.is_empty());
        assert!(split("\n\n  \n").chunks.is_empty());
    }

    #[test]
    fn line_cap_splits_long_chunks() {
        let text: String = (1..=130).map(|i| format!("l{i}\n")).collect();
        let s = split(&text);
        let v: Vec<_> = s.chunks.iter().map(|c| (c.start_line, c.end_line)).collect();
        assert_eq!(v, vec![(1, 60), (61, 120), (121, 130)]);
        // Only the first piece of a heading chunk keeps the heading.
        let text = format!("# H\n{}", "x\n".repeat(100));
        let s = split(&text);
        assert_eq!(s.chunks.len(), 2);
        assert!(s.chunks[0].heading.is_some());
        assert!(s.chunks[1].heading.is_none());
        assert!(
            s.chunks
                .iter()
                .all(|c| ((c.end_line - c.start_line) as usize) < MAX_CHUNK_LINES)
        );
    }

    #[test]
    fn chunk_limit_reports_truncation() {
        let exact: String = (0..MAX_CHUNKS).map(|i| format!("b{i}\n\n")).collect();
        let s = split(&exact);
        assert_eq!(s.chunks.len(), MAX_CHUNKS);
        assert!(!s.truncated);
        let over: String = (0..MAX_CHUNKS + 10).map(|i| format!("b{i}\n\n")).collect();
        let s = split(&over);
        assert_eq!(s.chunks.len(), MAX_CHUNKS);
        assert!(s.truncated);
        let (f, errors) = run(over.as_bytes());
        assert_eq!(f.symbols.len(), MAX_CHUNKS);
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ExtractError::Unsupported(m) if m.contains("chunk limit")))
        );
    }

    #[test]
    fn text_fact_cut_on_char_boundary() {
        // Three-byte chars: 2,048 is not a multiple of 3, so the cut must step back.
        let line = "€".repeat(500);
        let s = split(&format!("{line}\n{line}\n"));
        let t = &s.chunks[0].text;
        assert!(t.len() <= MAX_TEXT_BYTES);
        assert_eq!(t.len(), 2047);
        assert!(t.chars().all(|c| c == '€' || c == '\n'));
    }

    #[test]
    fn utf8_bom_dropped() {
        let (t, enc, msg) = decode(b"\xEF\xBB\xBFhello\r\nworld\rend");
        assert_eq!(t, "hello\nworld\nend");
        assert_eq!(enc, Encoding::Utf8Bom);
        assert!(msg.is_none());
        let (f, errors) = run(b"\xEF\xBB\xBFhello\n");
        assert!(errors.is_empty());
        assert_eq!(str_fact(&f.artifact.facts, "encoding"), Some("utf-8-bom"));
        assert_eq!(str_fact(&f.artifact.facts, "title"), Some("hello"));
    }

    #[test]
    fn utf16_with_bom_decoded() {
        let le: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("# Tïtle\nbody".encode_utf16().flat_map(|u| u.to_le_bytes()))
            .collect();
        let be: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain("# Tïtle\nbody".encode_utf16().flat_map(|u| u.to_be_bytes()))
            .collect();
        let (t, enc, msg) = decode(&le);
        assert_eq!((t.as_str(), enc, msg), ("# Tïtle\nbody", Encoding::Utf16Le, None));
        let (t, enc, msg) = decode(&be);
        assert_eq!((t.as_str(), enc, msg), ("# Tïtle\nbody", Encoding::Utf16Be, None));
        // UTF-16 text is not binary although it holds NUL bytes.
        assert!(!is_binary(&le));
        let (f, errors) = run(&le);
        assert!(errors.is_empty());
        assert_eq!(f.symbols.len(), 1);
        // A lone surrogate is replaced and reported.
        let bad = [0xFF, 0xFE, 0x00, 0xD8, b'a', 0x00];
        let (t, _, msg) = decode(&bad);
        assert_eq!(t, "\u{FFFD}a");
        assert!(msg.is_some());
    }

    #[test]
    fn invalid_utf8_falls_back_to_latin1() {
        let (t, enc, msg) = decode(b"caf\xE9 au lait\n");
        assert_eq!(t, "café au lait\n");
        assert_eq!(enc, Encoding::Latin1);
        assert!(msg.is_some());
        let (f, errors) = run(b"caf\xE9 au lait\n");
        assert_eq!(errors.len(), 1);
        assert!(matches!(&errors[0], ExtractError::Parse { message, .. } if message.contains("Latin-1")));
        assert_eq!(str_fact(&f.artifact.facts, "encoding"), Some("latin-1"));
        assert_eq!(f.symbols.len(), 1);
    }

    #[test]
    fn binary_detection_ratio() {
        assert!(is_binary(b"abc\0def"));
        assert!(!is_binary(b"tab\there\r\nform\x0Cfeed"));
        // Exactly 10% control bytes is not binary; above is.
        let mut ten = vec![b'a'; 90];
        ten.extend([1u8; 10]);
        assert!(!is_binary(&ten));
        let mut eleven = vec![b'a'; 89];
        eleven.extend([1u8; 11]);
        assert!(is_binary(&eleven));
        // Only the first 8,192 bytes count.
        let mut late = vec![b'a'; BINARY_SNIFF_BYTES];
        late.push(0);
        assert!(!is_binary(&late));
        assert!(!is_binary(b""));
    }

    #[test]
    fn binary_file_is_artifact_only() {
        let (f, errors) = run(b"PK\x03\x04\0\0 data");
        assert!(f.symbols.is_empty());
        assert_eq!(str_fact(&f.artifact.facts, "skipped"), Some("binary"));
        assert!(matches!(&errors[..], [ExtractError::Unsupported(m)] if m.contains("binary")));
        assert_eq!(f.extension, "text");
    }

    #[test]
    fn oversize_file_is_artifact_only() {
        let big = vec![b'a'; MAX_INPUT_BYTES + 1];
        let (f, errors) = run(&big);
        assert!(f.symbols.is_empty());
        assert_eq!(str_fact(&f.artifact.facts, "skipped"), Some("too-large"));
        assert!(matches!(&errors[..], [ExtractError::Unsupported(m)] if m.contains("1 MiB")));
        let ok = vec![b'a'; MAX_INPUT_BYTES];
        let (f, _) = run(&ok);
        assert!(str_fact(&f.artifact.facts, "skipped").is_none());
    }

    #[test]
    fn deterministic_fragment() {
        let text = "# A\nx\n\n## B\ny\n".repeat(20);
        assert_eq!(run(text.as_bytes()).0, run(text.as_bytes()).0);
        assert_eq!(run(b"\xFFlatin").0, run(b"\xFFlatin").0);
    }

    #[test]
    fn chunk_symbols_carry_spans_and_facts() {
        let (f, errors) = run(b"# Title\nfirst line\n\nsecond block ccc\n");
        assert!(errors.is_empty());
        assert_eq!(f.symbols.len(), 1, "one heading chunk");
        let s = &f.symbols[0];
        assert_eq!(s.kind, "chunk");
        assert_eq!((s.span.start_line, s.span.start_col), (1, 1));
        assert_eq!((s.span.end_line, s.span.end_col), (4, 16));
        assert_eq!(str_fact(&s.facts, "title"), Some("Title"));
        assert_eq!(s.facts.get("level"), Some(&Fact::Int(1)));
        assert!(str_fact(&s.facts, "text").unwrap().contains("second block ccc"));
        assert_eq!(str_fact(&f.artifact.facts, "chunk_mode"), Some("headings"));
    }

    #[test]
    fn shard_size_within_budget() {
        // Prose in blocks of ten lines of about 75 characters.
        let mut text = String::new();
        let words = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf"];
        let mut n = 0usize;
        while text.len() < 256 * 1024 {
            for _ in 0..10 {
                let line: Vec<&str> = (0..12).map(|i| words[(n + i * 3) % 7]).collect();
                text.push_str(&line.join(" "));
                text.push('\n');
                n += 1;
            }
            text.push('\n');
        }
        let (f, _) = run(text.as_bytes());
        let shard = katalogr_store::shard::encode(&f).unwrap();
        let ratio = shard.len() as f64 / text.len() as f64;
        println!("shard {} source {} ratio {ratio:.2}", shard.len(), text.len());
        assert!(ratio <= 1.5, "ratio {ratio:.2}");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        #[test]
        fn never_panics_on_arbitrary_bytes(
            bytes in prop_oneof![
                proptest::collection::vec(any::<u8>(), 0..400),
                // Mostly-text inputs: multibyte chars, hashes, fences, BOMs, newlines.
                proptest::collection::vec(
                    prop_oneof![
                        Just("# ".to_string()), Just("\n".to_string()), Just("\r".to_string()),
                        Just("```".to_string()), Just("€".to_string()), Just("é".to_string()),
                        "[a-z ]{0,20}", Just("\u{FEFF}".to_string()),
                    ],
                    0..200,
                ).prop_map(|v| v.concat().into_bytes()),
                // UTF-16 with BOM, arbitrary units (lone surrogates, odd lengths).
                (any::<bool>(), proptest::collection::vec(any::<u8>(), 0..200)).prop_map(
                    |(le, mut v)| {
                        let mut out = if le { vec![0xFF, 0xFE] } else { vec![0xFE, 0xFF] };
                        out.append(&mut v);
                        out
                    }
                ),
            ]
        ) {
            let (f, _) = run(&bytes);
            prop_assert_eq!(f.extension.as_str(), "text");
        }
    }
}
