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
}
