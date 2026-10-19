//! FEAT-017 R-16. Run in release: `cargo test --release -p katalogr-ext-sql --test bench -- --ignored`.

mod bench {
    use katalogr_ext_sql::SqlExtractor;
    use katalogr_model::{ExtractInput, Extractor, RelPath};
    use std::time::Instant;

    /// 10,000 statements (about 1 MB): create, alter, view and insert-select, in turn.
    fn generate() -> String {
        let mut s = String::new();
        for i in 0..10_000u32 {
            match i % 4 {
                0 => s.push_str(&format!(
                    "CREATE TABLE shop_{i} (id bigint PRIMARY KEY, name text NOT NULL, created_at timestamptz NOT NULL DEFAULT now(), parent_id bigint REFERENCES shop_{p} (id), note varchar(200));\n",
                    p = i.saturating_sub(4)
                )),
                1 => s.push_str(&format!(
                    "ALTER TABLE shop_{p} ADD COLUMN extra_{i} integer NOT NULL DEFAULT {i}, ADD COLUMN flag_{i} boolean;\n",
                    p = i - 1
                )),
                2 => s.push_str(&format!(
                    "CREATE VIEW shop_view_{i} AS SELECT a.id, a.name, b.created_at FROM shop_{p} a JOIN shop_{q} b ON b.id = a.parent_id WHERE a.name <> 'x{i}';\n",
                    p = i - 2,
                    q = i - 6.min(i)
                )),
                _ => s.push_str(&format!(
                    "INSERT INTO shop_{p} (id, name) SELECT id, name FROM shop_{q} WHERE created_at > now() - interval '{i} days';\n",
                    p = i - 3,
                    q = i - 7.min(i)
                )),
            }
        }
        s
    }

    #[test]
    #[ignore]
    fn sql_extract_10k_statements() {
        let text = generate();
        eprintln!("input: {} bytes", text.len());
        let path = RelPath::new("db/migration/V1__big.sql").unwrap();
        let input = ExtractInput {
            repo: "r",
            path: &path,
            revision: "rev",
            content: text.as_bytes(),
            context: &[],
        };
        let mut best = u128::MAX;
        for _ in 0..3 {
            let t = Instant::now();
            let (f, errs) = SqlExtractor.extract_with_errors(&input).unwrap();
            let ms = t.elapsed().as_millis();
            best = best.min(ms);
            eprintln!(
                "run: {ms} ms, {} symbols, {} bindings, {} errors",
                f.symbols.len(),
                f.bindings.len(),
                errs.len()
            );
            assert!(errs.is_empty(), "{:?}", &errs[..errs.len().min(3)]);
            assert!(f.symbols.len() > 10_000);
        }
        assert!(best <= 500, "best of 3 runs took {best} ms");
    }
}
