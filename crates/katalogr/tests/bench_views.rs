//! FEAT-088 R-12 benchmark: on a 10,000-file repository, a view served from the store adds at
//! most 20 ms over reading the live position. It is an `#[ignore]` test: run it in release,
//! under the shared heavy-build lock, with the corpus on disk outside `/tmp`:
//! `flock ~/.cache/heavy-build.lock cargo test --release -p katalogr --test bench_views -- --ignored --nocapture --test-threads=1`
//! It prints its numbers with the load average; record them in the component doc.

mod bench {
    use katalogr_dist::render::ViewSpec;
    use katalogr_dist::{Katalogr, Options, Outcome, Overrides, kindex_init, project_init};
    use std::time::{Duration, Instant};

    /// The one-minute load average, for the line every timing prints.
    fn load() -> String {
        std::fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|s| s.split_whitespace().next().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Where the corpus goes: on disk under the scratch folder, never `/tmp`.
    fn scratch() -> tempfile::TempDir {
        let home = std::env::var("HOME").expect("HOME");
        let dir = std::path::Path::new(&home).join(".cache/katalogr-scratch/FEAT-088");
        std::fs::create_dir_all(&dir).unwrap();
        tempfile::Builder::new()
            .prefix("bench-")
            .tempdir_in(dir)
            .unwrap()
    }

    /// 10,000 files of 5 symbols; each file calls the next one through an exported name.
    fn corpus(dir: &std::path::Path) -> Katalogr {
        let kx = dir.join("kx");
        let repo = dir.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        for i in 0..10_000 {
            let mut text = String::new();
            for j in 0..5 {
                text.push_str(&format!("sym fn s{j}\n"));
            }
            text.push_str(&format!("export x:m{i} s0\n"));
            text.push_str(&format!("ref s1 calls x:m{}\n", (i + 1) % 10_000));
            std::fs::write(repo.join(format!("m{i}.tx")), text).unwrap();
        }
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), repo.clone())], true).unwrap();
        let k = Katalogr::resolve(
            &repo,
            &Overrides {
                kindex: Some(kx.display().to_string()),
                project: None,
                context: None,
            },
        )
        .unwrap();
        k.index(None, true, None, &Options::default()).unwrap();
        k
    }

    fn best_of<T>(runs: usize, mut f: impl FnMut() -> T) -> (Duration, T) {
        let mut best = Duration::MAX;
        let mut last = None;
        for _ in 0..runs {
            let t = Instant::now();
            let v = f();
            best = best.min(t.elapsed());
            last = Some(v);
        }
        (best, last.unwrap())
    }

    /// R-12: a stored view costs at most 20 ms more than `position`.
    #[test]
    #[ignore = "benchmark: run in release with --ignored"]
    fn view_store_hit() {
        let tmp = scratch();
        let k = corpus(tmp.path());
        let opts = Options::default();
        let spec = ViewSpec::parse("structure --level file --max-nodes 100").unwrap();
        // The first read computes and stores.
        let first = k.view(&spec, &opts).unwrap();
        let Outcome::Found(stored) = first.outcome else {
            panic!("no view")
        };
        assert!(!stored.from_store && first.report.exact);
        let (position, _) = best_of(10, || k.position(None, &opts).unwrap());
        let (hit, reply) = best_of(10, || k.view(&spec, &opts).unwrap());
        let Outcome::Found(b) = reply.outcome else {
            panic!("no view")
        };
        assert!(b.from_store, "the second read must be a hit");
        let extra = hit.saturating_sub(position);
        println!(
            "view_store_hit: position {position:?}, hit {hit:?}, extra {extra:?}, body {} bytes (load {})",
            b.body.len(),
            load()
        );
        assert!(
            extra <= Duration::from_millis(20),
            "a hit adds {extra:?} over the position read"
        );
    }
}
