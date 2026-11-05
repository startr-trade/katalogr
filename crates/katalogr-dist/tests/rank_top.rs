//! FEAT-076 R-6: `session-start` built from the stored head of the rank prints what the full map
//! prints. Needs the `.tx` test extractor: `cargo test -p katalogr-dist --features test-extractor`.
#![cfg(feature = "test-extractor")]

mod rank_top {
    use katalogr_dist::{Overrides, kindex_init, project_init, session_start};
    use katalogr_engine::Stitcher;
    use katalogr_store::{Cache, Kindex, ProjectName};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    struct Setup {
        _tmp: tempfile::TempDir,
        kx: PathBuf,
        repo: PathBuf,
    }

    fn setup(text: &str) -> Setup {
        let tmp = tempfile::tempdir().unwrap();
        let kx = tmp.path().join("kx");
        let repo = tmp.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("s.tx"), text).unwrap();
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), repo.clone())], true).unwrap();
        let s = Setup {
            _tmp: tmp,
            kx,
            repo,
        };
        let k = katalogr_dist::Katalogr::resolve(&s.repo, &overrides(&s.kx)).unwrap();
        k.index(None, true, None, &katalogr_dist::Options::default())
            .unwrap();
        s
    }

    fn overrides(kx: &Path) -> Overrides {
        Overrides {
            kindex: Some(kx.display().to_string()),
            project: None,
        }
    }

    fn session(s: &Setup, budget: u64) -> String {
        session_start(&s.repo, &overrides(&s.kx), Some(budget), Some(30.0))
    }

    /// Leave the stored rank behind the watermark, so `session-start` takes the full map.
    fn stale_rank(s: &Setup) {
        let kindex = Kindex::open(camino::Utf8Path::from_path(&s.kx).unwrap()).unwrap();
        let dir = kindex.project(&ProjectName::new("demo").unwrap()).unwrap();
        let cache = Cache::open_or_rebuild(
            &dir,
            &Stitcher::new(),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        let mut tx = cache.write().unwrap();
        tx.set_rank_watermark(1_000_000).unwrap();
        tx.commit().unwrap();
    }

    fn corpus(n: usize) -> String {
        let mut text = String::new();
        for i in 0..n {
            text.push_str(&format!("sym fn symbol_number_{i}\n"));
        }
        for i in 1..n.min(40) {
            text.push_str(&format!("link symbol_number_{i} calls symbol_number_0\n"));
        }
        text
    }

    #[test]
    fn session_start_matches_full_map() {
        let s = setup(&corpus(60));
        let with_top: Vec<String> = [40, 120, 600, 4000].iter().map(|b| session(&s, *b)).collect();
        assert!(with_top[0].contains("partial:"), "{}", with_top[0]);
        assert!(!with_top[3].contains("partial:"), "{}", with_top[3]);
        stale_rank(&s);
        for (b, want) in [40u64, 120, 600, 4000].iter().zip(&with_top) {
            assert_eq!(&session(&s, *b), want, "budget {b}");
        }
    }

    #[test]
    fn budget_beyond_top_uses_full_map() {
        // More symbols than the stored head holds.
        let n = katalogr_store::RANK_TOP_ENTRIES + 100;
        let s = setup(&corpus(n));
        let small = session(&s, 1500);
        let huge = session(&s, 10_000_000);
        stale_rank(&s);
        assert_eq!(session(&s, 1500), small);
        assert_eq!(session(&s, 10_000_000), huge);
        assert!(small.contains(&format!("budget; {n} symbols)")), "{small}");
        let lines = huge.lines().filter(|l| l.starts_with("fn ")).count();
        assert_eq!(lines, n, "a budget past the head still lists every symbol");
        assert!(!huge.contains("partial:"));
    }
}
