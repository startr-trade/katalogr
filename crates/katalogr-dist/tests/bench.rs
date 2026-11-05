//! Evaluation benchmarks (FEAT-061 R-11). Ignored; run in release:
//! `cargo test -p katalogr-dist --release --test bench -- --ignored --nocapture`.

#[path = "support/synth.rs"]
mod synth;

mod bench {
    use katalogr_dist::eval::{EvalRequest, parse_sweep, run_fixtures, sweep};
    use std::path::Path;
    use std::time::Instant;

    fn request() -> EvalRequest {
        EvalRequest {
            sets: vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/eval")],
            ..EvalRequest::default()
        }
    }

    #[test]
    #[ignore]
    fn eval_fixture_suite() {
        let t = Instant::now();
        let r = run_fixtures(&request()).unwrap();
        let ms = t.elapsed().as_millis();
        println!(
            "eval_fixture_suite: {ms} ms for {} queries",
            r.queries.len()
        );
        assert!(ms < 5000, "{ms} ms");
    }

    #[test]
    #[ignore]
    fn eval_sweep_16() {
        let grid = [
            parse_sweep("search.k1=1.0,1.2,1.5,2.0").unwrap(),
            parse_sweep("search.b=0.5,0.6,0.75,0.9").unwrap(),
        ];
        let t = Instant::now();
        let r = sweep(&request(), &grid, "retrieval.ndcg@10").unwrap();
        let ms = t.elapsed().as_millis();
        println!("eval_sweep_16: {ms} ms for {} combinations", r.rows.len());
        assert_eq!(r.rows.len(), 16);
        assert!(ms < 10000, "{ms} ms");
    }

    // ------------------------------------------------ cache size and cost (FEAT-075 T9)

    use katalogr_dist::{Katalogr, Options, Overrides, kindex_init, project_init};
    use katalogr_engine::Stitcher;
    use katalogr_store::{Cache, CompactMode, Kindex, ProjectDir, ProjectName, Reader};
    use std::path::PathBuf;

    fn load_average() -> String {
        std::fs::read_to_string("/proc/loadavg")
            .map(|s| s.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
    }

    /// A kindex in `tmp` with one project holding `repo`, fully indexed (`index --full`, with
    /// the compaction it runs). No marker is written into `repo`.
    fn index_full(tmp: &Path, repo: &Path) -> (ProjectDir, std::time::Duration) {
        let kx = tmp.join("kx");
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), repo.to_path_buf())], false).unwrap();
        let k = Katalogr::resolve(
            repo,
            &Overrides {
                kindex: Some(kx.display().to_string()),
                project: Some("demo".into()),
            },
        )
        .unwrap();
        let opts = Options {
            timeout: Some(3600.0),
            ..Options::default()
        };
        let t = Instant::now();
        let report = k.index(None, true, None, &opts).unwrap();
        let took = t.elapsed();
        assert_eq!(report.pending, 0, "drained: {:?}", report.warnings);
        let dir = Kindex::open(camino::Utf8Path::from_path(&kx).unwrap())
            .unwrap()
            .project(&ProjectName::new("demo").unwrap())
            .unwrap();
        (dir, took)
    }

    /// Bytes of `index.redb` per byte of the sources that have a shard.
    fn bytes_per_source_byte(dir: &ProjectDir, repo: &Path) -> (f64, u64, u64, usize) {
        let cache = std::fs::metadata(dir.cache_dir().join("index.redb"))
            .unwrap()
            .len();
        let mut source = 0u64;
        let shards = dir.list_shards().unwrap();
        for (_, path) in &shards {
            if let Ok(m) = std::fs::metadata(repo.join(path.as_str())) {
                source += m.len();
            }
        }
        (cache as f64 / source as f64, cache, source, shards.len())
    }

    /// R-11a. Katalogr's own tree, indexed with the default excludes, keeps its cache at or below
    /// 5.0 bytes per source byte (schema 8: 25.5).
    #[test]
    #[ignore = "benchmark: run in release with --ignored"]
    fn cache_bytes_per_source_byte_self() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let (dir, took) = index_full(tmp.path(), &repo);
        let (ratio, cache, source, files) = bytes_per_source_byte(&dir, &repo);
        println!(
            "cache_bytes_per_source_byte_self: {files} files, {source} source bytes, cache {cache} bytes = {ratio:.2} B/B, index {took:?}, load {}",
            load_average()
        );
        assert!(ratio <= 5.0, "{ratio:.2} bytes per source byte");
    }

    /// R-11b. A large Rust corpus (the path in `KATALOGR_BENCH_RUST_CORPUS`, a read-only clone
    /// outside this repository) keeps its cache at or below 10.0 bytes per source byte (schema
    /// 8: 81.9). Prints sizes and timings only. Skipped when the variable is unset.
    #[test]
    #[ignore = "benchmark: run in release with --ignored"]
    fn cache_budget_rust_corpus() {
        let Some(path) = std::env::var_os("KATALOGR_BENCH_RUST_CORPUS") else {
            println!("cache_budget_rust_corpus: skipped, KATALOGR_BENCH_RUST_CORPUS is not set");
            return;
        };
        let repo = PathBuf::from(path).canonicalize().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let (dir, took) = index_full(tmp.path(), &repo);
        let (ratio, cache, source, files) = bytes_per_source_byte(&dir, &repo);
        println!(
            "cache_budget_rust_corpus: {files} files, {source} source bytes, cache {cache} bytes = {ratio:.2} B/B, index {took:?}, load {}",
            load_average()
        );
        assert!(ratio <= 10.0, "{ratio:.2} bytes per source byte");
    }

    /// The `index --full` time of this corpus at schema 9, in milliseconds, from the T1-era run on
    /// the benchmark machine. `None` until that run is recorded: the bench then prints the
    /// numbers and checks only the compaction share. Rerun on the machine before asserting.
    const SCHEMA9_FULL_INDEX_MS: Option<u128> = None;

    /// R-13b. A full index of the synthetic 860-file corpus, compaction included, takes at most
    /// 1.05 times the schema-9 time of the same corpus, and the compaction itself at most 5% of
    /// the full index.
    #[test]
    #[ignore = "benchmark: run in release with --ignored"]
    fn full_index_with_compaction() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("app");
        crate::synth::rust_corpus(&repo, 20, 40, 12);
        let (dir, took) = index_full(tmp.path(), &repo);
        // The compaction alone, on the cache the run just left: a rewrite of the same rows.
        let cache = Cache::open_or_rebuild(
            &dir,
            &Stitcher::new(),
            Instant::now() + std::time::Duration::from_secs(60),
        )
        .unwrap();
        let before = cache.file_bytes().unwrap();
        let (cache, report) = cache
            .compact(
                &dir,
                CompactMode::Rewrite,
                Instant::now() + std::time::Duration::from_secs(60),
            )
            .unwrap();
        let files = cache.read().unwrap().artifacts().unwrap().len();
        println!(
            "full_index_with_compaction: {files} artifacts, full index {took:?}, rewrite {} ms ({before} -> {} bytes), load {}",
            report.millis,
            report.bytes_after,
            load_average()
        );
        assert!(
            u128::from(report.millis) * 20 <= took.as_millis(),
            "the rewrite is more than 5% of the full index"
        );
        match SCHEMA9_FULL_INDEX_MS {
            Some(base) => assert!(
                took.as_millis() * 100 <= base * 105,
                "{} ms against {base} ms at schema 9",
                took.as_millis()
            ),
            None => println!("full_index_with_compaction: no schema-9 baseline recorded yet"),
        }
    }
}
