//! FEAT-080 R-5 and R-7 through the facade: `session-start` and `find` beside a writer. Needs the
//! `.tx` test extractor: `cargo test -p katalogr-dist --features test-extractor`.
#![cfg(feature = "test-extractor")]

mod readers {
    use katalogr_dist::{Consistency, Options, Outcome, Overrides, kindex_init, project_init};
    use katalogr_dist::{Katalogr, session_start};
    use katalogr_engine::Stitcher;
    use katalogr_model::RelPath;
    use katalogr_store::{Cache, Kindex, PendingKey, ProjectName};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    struct Setup {
        _tmp: tempfile::TempDir,
        kx: PathBuf,
        repo: PathBuf,
    }

    fn setup() -> Setup {
        let tmp = tempfile::tempdir().unwrap();
        let kx = tmp.path().join("kx");
        let repo = tmp.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        let mut text = String::new();
        for i in 0..30 {
            text.push_str(&format!("sym fn symbol_number_{i}\n"));
        }
        for i in 1..20 {
            text.push_str(&format!("link symbol_number_{i} calls symbol_number_0\n"));
        }
        std::fs::write(repo.join("s.tx"), text).unwrap();
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), repo.clone())], true).unwrap();
        let s = Setup {
            _tmp: tmp,
            kx,
            repo,
        };
        let k = Katalogr::resolve(&s.repo, &overrides(&s.kx)).unwrap();
        k.index(None, true, None, &Options::default()).unwrap();
        s
    }

    fn overrides(kx: &Path) -> Overrides {
        Overrides {
            kindex: Some(kx.display().to_string()),
            project: None,
        }
    }

    fn session(s: &Setup) -> String {
        session_start(&s.repo, &overrides(&s.kx), Some(4000), Some(30.0))
    }

    /// The writer, held open by the test as `index --full` holds it.
    fn writer(s: &Setup) -> Cache {
        let kindex = Kindex::open(camino::Utf8Path::from_path(&s.kx).unwrap()).unwrap();
        let dir = kindex.project(&ProjectName::new("demo").unwrap()).unwrap();
        Cache::open_or_rebuild(
            &dir,
            &Stitcher::new(),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap()
    }

    /// A drain in flight: a queued path, and the stored rank behind the watermark.
    fn start_drain(w: &Cache) {
        let mut tx = w.write().unwrap();
        tx.put_pending(
            &PendingKey {
                context: "worktree".into(),
                repo: "app".into(),
                path: RelPath::new("s.tx").unwrap(),
            },
            1_000_000,
        )
        .unwrap();
        tx.set_rank_watermark(1).unwrap();
        tx.commit().unwrap();
    }

    fn symbol_lines(text: &str) -> Vec<&str> {
        text.lines().filter(|l| l.starts_with("fn ")).collect()
    }

    /// R-7: during a drain `session-start` answers from the last stored head, marked stale.
    #[test]
    fn session_start_uses_stored_top_during_drain() {
        let s = setup();
        let before = session(&s);
        assert!(!before.contains(", stale"), "{before}");
        let w = writer(&s);
        start_drain(&w);
        let t = Instant::now();
        let during = session(&s);
        assert!(t.elapsed() < Duration::from_secs(10));
        assert!(during.contains("1 pending, stale"), "{during}");
        assert_eq!(symbol_lines(&during), symbol_lines(&before));
        drop(w);
    }

    /// R-7: a first index (no head, changes queued) gives no map, promptly, and says why.
    #[test]
    fn session_start_first_index_skips_map() {
        let s = setup();
        let w = writer(&s);
        {
            let mut tx = w.write().unwrap();
            tx.store_rank(&[], 0).unwrap();
            tx.commit().unwrap();
        }
        start_drain(&w);
        let t = Instant::now();
        let during = session(&s);
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
        assert!(symbol_lines(&during).is_empty(), "{during}");
        assert!(
            during.contains("map appears when indexing finishes"),
            "{during}"
        );
        assert!(during.contains("1 pending"), "{during}");
        drop(w);
    }

    /// R-7: with no stored head and nothing queued the full map answers, as before.
    #[test]
    fn session_start_no_head_idle_uses_full_map() {
        let s = setup();
        let before = session(&s);
        let w = writer(&s);
        {
            let mut tx = w.write().unwrap();
            tx.store_rank(&[], 0).unwrap();
            tx.commit().unwrap();
        }
        let idle = session(&s);
        assert!(!idle.contains("still building"), "{idle}");
        assert_eq!(symbol_lines(&idle), symbol_lines(&before));
        assert!(!symbol_lines(&idle).is_empty());
        drop(w);
    }

    /// R-5: `stale-ok` is never timed out, so a no-match keeps its own outcome and code 4.
    #[test]
    fn stale_ok_no_match_keeps_exit_four() {
        let s = setup();
        let w = writer(&s);
        start_drain(&w);
        let k = Katalogr::resolve(&s.repo, &overrides(&s.kx)).unwrap();
        let reply = k
            .find(
                "no_such_symbol_anywhere",
                &Options {
                    consistency: Some(Consistency::StaleOk),
                    timeout: Some(5.0),
                    ..Options::default()
                },
            )
            .unwrap();
        assert!(matches!(reply.outcome, Outcome::NoMatch(_)));
        assert!(reply.report.stale && !reply.report.timed_out);
        drop(w);
    }
}
