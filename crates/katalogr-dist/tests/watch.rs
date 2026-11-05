//! FEAT-039 `watch`: a tick notices an edit made outside Claude, an idle tick takes no lock, a
//! failed tick does not stop the loop. Needs the `.tx` test extractor:
//! `cargo test -p katalogr-dist --features test-extractor` (a workspace run enables it).
#![cfg(feature = "test-extractor")]

mod watch {
    use katalogr_dist::watch::{WatchTick, watch};
    use katalogr_dist::{
        Consistency, Katalogr, Options, Outcome, Overrides, kindex_init, project_init,
    };
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, channel};
    use std::time::{Duration, Instant};

    struct Fx {
        _tmp: tempfile::TempDir,
        repo: PathBuf,
        cache: PathBuf,
        k: Katalogr,
    }

    fn fixture() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let (kx, repo) = (base.join("kx"), base.join("app"));
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("a.tx"), "sym fn first\n").unwrap();
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), repo.clone())], true).unwrap();
        let k = Katalogr::resolve(
            &repo,
            &Overrides {
                kindex: Some(kx.display().to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        k.index(None, false, None, &Options::default()).unwrap();
        let cache = kx.join("projects/demo/.cache");
        Fx {
            _tmp: tmp,
            repo,
            cache,
            k,
        }
    }

    const STEP: Duration = Duration::from_secs(30);

    /// Receive ticks until one satisfies `want`; the others are returned too.
    fn until(rx: &Receiver<WatchTick>, want: impl Fn(&WatchTick) -> bool) -> Vec<WatchTick> {
        let end = Instant::now() + STEP;
        let mut seen = Vec::new();
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let t = rx.recv_timeout(left).expect("a tick in time");
            let done = want(&t);
            seen.push(t);
            if done {
                return seen;
            }
        }
    }

    fn processed(t: &WatchTick) -> bool {
        matches!(t, WatchTick::Processed { .. })
    }

    /// Run `watch` at 50 ms in a thread while `body` drives the checkout.
    fn with_watch(f: &Fx, body: impl FnOnce(&Receiver<WatchTick>)) {
        let stop = AtomicBool::new(false);
        let (tx, rx) = channel();
        std::thread::scope(|s| {
            s.spawn(|| {
                watch(&f.k, Duration::from_millis(50), &stop, &mut |t| {
                    let _ = tx.send(t);
                })
                .unwrap();
            });
            // Whatever `body` does, the loop must be told to stop.
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&rx)));
            stop.store(true, Ordering::Relaxed);
            if let Err(e) = r {
                std::panic::resume_unwind(e);
            }
        });
    }

    #[test]
    fn tick_queues_outside_edit() {
        let f = fixture();
        with_watch(&f, |rx| {
            until(rx, processed);
            std::fs::write(f.repo.join("b.tx"), "sym fn outside_edit\n").unwrap();
            until(rx, processed);
        });
        let stale = Options {
            consistency: Some(Consistency::StaleOk),
            ..Options::default()
        };
        let r = f.k.find("outside_edit", &stale).unwrap();
        assert!(matches!(r.outcome, Outcome::Found(_)), "{:?}", r.outcome);
    }

    #[test]
    fn idle_tick_takes_no_lock() {
        let f = fixture();
        with_watch(&f, |rx| {
            until(rx, processed);
            // With the cache unusable, any tick that opened the engine would fail. Idle ticks
            // read the position only, so they go on reporting idle.
            std::fs::write(f.cache.join("index.redb"), b"not a database at all").unwrap();
            let mut idle = 0;
            let end = Instant::now() + STEP;
            while idle < 5 {
                let left = end.saturating_duration_since(Instant::now());
                match rx.recv_timeout(left).expect("a tick in time") {
                    WatchTick::Idle => idle += 1,
                    other => panic!("expected idle ticks, got {other:?}"),
                }
            }
        });
    }

    #[test]
    fn failed_tick_continues() {
        let f = fixture();
        let moved: PathBuf = Path::new(&f.repo).with_extension("moved");
        with_watch(&f, |rx| {
            until(rx, processed);
            std::fs::rename(&f.repo, &moved).unwrap();
            until(rx, |t| matches!(t, WatchTick::Failed(_)));
            std::fs::rename(&moved, &f.repo).unwrap();
            // The loop is still running: ticks come again and are not failures.
            until(rx, |t| !matches!(t, WatchTick::Failed(_)));
        });
    }
}
