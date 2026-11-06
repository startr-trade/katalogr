//! FEAT-017 R-17. Run in release:
//! `cargo test --release -p katalogr-acceptance --test bench -- --ignored --nocapture`.

mod bench {
    use camino::Utf8PathBuf;
    use katalogr_acceptance::{full_registry, testfmt};
    use katalogr_engine::registry::ExtensionRegistry;
    use katalogr_engine::{Consistency, Engine, NewEvent, QueryOptions, Scope};
    use katalogr_store::{Kindex, ProjectConfig, ProjectName, RepoConfig};
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    /// The full registry without the SQL extension.
    fn registry_without_sql() -> ExtensionRegistry {
        let mut reg = testfmt::registry();
        reg.register(Box::new(katalogr_ext_rust::RustExtractor));
        reg.register(Box::new(katalogr_ext_ts::TsExtractor));
        reg.register(Box::new(katalogr_ext_kanbanr::BoardExtractor));
        reg.register(Box::new(katalogr_ext_md::MarkdownExtractor));
        reg
    }

    /// 500 `.rs` and 500 `.ts` files with 20 literals each: 2 SQL, 6 short call arguments
    /// without whitespace, the rest plain strings.
    fn generate(work: &camino::Utf8Path) {
        for i in 0..500 {
            let mut rs = String::new();
            let mut ts = String::new();
            for k in 0..20 {
                match k {
                    0 | 1 => {
                        rs.push_str(&format!(
                            "pub fn q{k}() -> String {{ sqlx::query(\"SELECT id FROM orders_{i} WHERE n = {k}\") }}\n"
                        ));
                        ts.push_str(&format!(
                            "export function q{k}() {{ return db.query(\"SELECT id FROM orders_{i} WHERE n = {k}\"); }}\n"
                        ));
                    }
                    2..=7 => {
                        rs.push_str(&format!(
                            "pub fn u{k}() -> String {{ http::get(\"/a/b/{k}\") }}\n"
                        ));
                        ts.push_str(&format!(
                            "export function u{k}() {{ return get(\"/a/b/{k}\"); }}\n"
                        ));
                    }
                    _ => {
                        rs.push_str(&format!(
                            "pub fn s{k}() -> &'static str {{ \"this is plain text number {k} for file {i}\" }}\n"
                        ));
                        ts.push_str(&format!(
                            "export const s{k} = \"this is plain text number {k} for file {i}\";\n"
                        ));
                    }
                }
            }
            std::fs::create_dir_all(work.join("src")).unwrap();
            std::fs::create_dir_all(work.join("web")).unwrap();
            std::fs::write(work.join(format!("src/m{i}.rs")), rs).unwrap();
            std::fs::write(work.join(format!("web/m{i}.ts")), ts).unwrap();
        }
    }

    fn run(make: &dyn Fn() -> ExtensionRegistry, work: &camino::Utf8Path) -> Duration {
        let tmp = tempfile::tempdir().unwrap();
        let kx = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let dir = Kindex::init(&kx)
            .unwrap()
            .create_project(
                &ProjectName::new("p").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let roots = BTreeMap::from([("r".to_string(), work.to_path_buf())]);
        let engine =
            Engine::open(dir, make(), roots, Instant::now() + Duration::from_secs(30)).unwrap();
        let t = Instant::now();
        engine.inbox().append(&NewEvent::rescan("r")).unwrap();
        let a = engine
            .with_consistency(
                &QueryOptions {
                    consistency: Consistency::WaitAll,
                    timeout: Duration::from_secs(600),
                    scope: Scope::Project,
                },
                |_| (),
            )
            .unwrap();
        assert!(!a.stale);
        t.elapsed()
    }

    fn median(v: &mut [Duration]) -> Duration {
        v.sort();
        v[v.len() / 2]
    }

    #[test]
    #[ignore]
    fn embedded_overhead_1k_files() {
        let tmp = tempfile::tempdir().unwrap();
        let work = Utf8PathBuf::from_path_buf(tmp.path().join("work")).unwrap();
        generate(&work);
        let (mut with, mut without) = (Vec::new(), Vec::new());
        // Interleave the runs so that machine noise hits both.
        for _ in 0..3 {
            without.push(run(&registry_without_sql, &work));
            with.push(run(&full_registry, &work));
        }
        let (w, wo) = (median(&mut with), median(&mut without));
        let ratio = w.as_secs_f64() / wo.as_secs_f64();
        eprintln!(
            "with SqlExtractor: {w:?} (runs {with:?}); without: {wo:?} (runs {without:?}); ratio {ratio:.3}"
        );
        assert!(ratio <= 1.10, "ratio {ratio:.3}");
    }

    /// FEAT-014 R-15. A synthetic corpus of 500 Rust and 500 TypeScript files with 40
    /// functions each and 10 calls per function: two pass a string, one of those also a handler
    /// name (20% of the calls pass a string, half of those a handler). `strings = false` gives
    /// the same corpus with every string argument replaced by an integer.
    fn string_call_corpus(strings: bool) -> Vec<(katalogr_model::RelPath, Vec<u8>)> {
        let arg = |s: &str, n: usize| {
            if strings {
                format!("\"{s}\"")
            } else {
                n.to_string()
            }
        };
        let mut files = Vec::new();
        for i in 0..500 {
            let mut rs = String::from("use crate::h::{");
            let mut ts = String::from("import { ");
            for f in 0..40 {
                rs.push_str(&format!("h{f}, "));
                ts.push_str(&format!("h{f}, "));
            }
            rs.push_str("};\n\n");
            ts.push_str("} from \"./h\";\n\n");
            for f in 0..40 {
                let (a, b) = (
                    arg(&format!("/a/{i}/{f}"), f),
                    arg(&format!("/b/{i}/{f}"), f),
                );
                rs.push_str(&format!("pub fn f{f}(a: u32, c: &Client, r: &Router) {{\n"));
                ts.push_str(&format!(
                    "export function f{f}(a: number, c: Client, r: Router) {{\n"
                ));
                for k in 0..8 {
                    // Calls to the other functions of the file, which resolve.
                    let g = (f + k + 1) % 40;
                    rs.push_str(&format!("    f{g}({k}, c, r);\n"));
                    ts.push_str(&format!("  f{g}({k}, c, r);\n"));
                }
                rs.push_str(&format!("    c.get({a});\n    r.route({b}, h{f});\n}}\n\n"));
                ts.push_str(&format!("  c.get({a});\n  r.get({b}, h{f});\n}}\n\n"));
            }
            files.push((
                katalogr_model::RelPath::new(&format!("src/m{i}.rs")).unwrap(),
                rs.into_bytes(),
            ));
            files.push((
                katalogr_model::RelPath::new(&format!("web/m{i}.ts")).unwrap(),
                ts.into_bytes(),
            ));
        }
        files
    }

    /// Extract every file; the time spent and the JSON bytes of the fragments.
    fn extract_all(
        files: &[(katalogr_model::RelPath, Vec<u8>)],
        rust_ctx: &[u8],
    ) -> (Duration, usize, usize, [Duration; 2]) {
        use katalogr_model::{ExtractInput, Extractor};
        let (mut bytes, mut calls) = (0usize, 0usize);
        let t = Instant::now();
        let mut split = [Duration::ZERO; 2];
        let mut fragments = Vec::with_capacity(files.len());
        for (path, content) in files {
            let rust = path.as_str().ends_with(".rs");
            let input = ExtractInput {
                repo: "r",
                path,
                revision: "ce013625030ba8dba906f756967f9e9ca394464a",
                content,
                context: if rust { rust_ctx } else { &[] },
            };
            let t1 = Instant::now();
            let x = if rust {
                katalogr_ext_rust::RustExtractor.extract_full(&input)
            } else {
                katalogr_ext_ts::TsExtractor.extract_full(&input)
            }
            .unwrap();
            split[usize::from(!rust)] += t1.elapsed();
            fragments.push(x.fragment);
        }
        let elapsed = t.elapsed();
        for f in &fragments {
            let n = serde_json::to_vec(f).unwrap().len();
            bytes += n;
            calls += f
                .symbols
                .iter()
                .filter(|s| s.facts.contains_key("str_calls"))
                .count();
        }
        (elapsed, bytes, calls, split)
    }

    #[test]
    #[ignore]
    fn string_calls_overhead() {
        let strings = string_call_corpus(true);
        let ints = string_call_corpus(false);
        let ctx = katalogr_ext_rust::RustExtractor
            .build_context(&[
                (
                    katalogr_model::RelPath::new("Cargo.toml").unwrap(),
                    b"[package]\nname = \"bench\"\nversion = \"0.1.0\"\n".to_vec(),
                ),
                (
                    katalogr_model::RelPath::new("src/lib.rs").unwrap(),
                    b"pub mod h;\n".to_vec(),
                ),
            ])
            .unwrap();
        use katalogr_model::Extractor;
        // One warm-up of each, then five interleaved runs.
        let _ = extract_all(&ints, &ctx);
        let (mut ts_, mut ti_) = (Vec::new(), Vec::new());
        let (mut bs, mut bi, mut cs, mut ci) = (0, 0, 0, 0);
        let (mut split_s, mut split_i) = ([Duration::ZERO; 2], [Duration::ZERO; 2]);
        for _ in 0..5 {
            let (t, b, c, sp) = extract_all(&ints, &ctx);
            ti_.push(t);
            (bi, ci, split_i) = (b, c, sp);
            let (t, b, c, sp) = extract_all(&strings, &ctx);
            ts_.push(t);
            (bs, cs, split_s) = (b, c, sp);
        }
        let (ts, ti) = (median(&mut ts_), median(&mut ti_));
        let time_ratio = ts.as_secs_f64() / ti.as_secs_f64();
        let byte_ratio = bs as f64 / bi as f64;
        eprintln!(
            "str_calls: strings {ts:?} ({bs} bytes, {cs} symbols with the fact); integers {ti:?} \
             ({bi} bytes, {ci}); time ratio {time_ratio:.3}, bytes ratio {byte_ratio:.3}"
        );
        eprintln!(
            "rust {:?} vs {:?}; ts {:?} vs {:?} (last runs)",
            split_s[0], split_i[0], split_s[1], split_i[1]
        );
        assert!(
            cs > 0 && ci == 0,
            "the fact is recorded for the strings only"
        );
        println!(
            "katalogr-bench: string_calls_time_ratio {:.4} ratio",
            time_ratio
        );
        println!(
            "katalogr-bench: string_calls_bytes_ratio {:.4} ratio",
            byte_ratio
        );
        assert!(time_ratio <= 1.41, "time ratio {time_ratio:.3}");
        assert!(byte_ratio <= 1.15, "bytes ratio {byte_ratio:.3}");
    }
}
