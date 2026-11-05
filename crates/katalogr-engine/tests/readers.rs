//! FEAT-080: queries beside a writer. A writer `Engine` and read-only engines live in one
//! process, as redb's own tests do; the crash tests run a child process (feature `failpoints`).

mod support;

use camino::{Utf8Path, Utf8PathBuf};
use katalogr_engine::queue::Processor;
use katalogr_engine::registry::ExtensionRegistry;
use katalogr_engine::{Consistency, Engine, QueryOptions, Scope};
use katalogr_model::RelPath;
use katalogr_store::{Kindex, PendingKey, ProjectDir, ProjectName, Reader, StoreError};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};
use support::{
    TestExtractor, drained_graph, file_event, fresh_index, init_project, open_engine, shard_bytes,
};

struct Fixture {
    _tmp: tempfile::TempDir,
    kx: Utf8PathBuf,
    work: Utf8PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let kx = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
    let work = Utf8PathBuf::from_path_buf(tmp.path().join("work")).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    init_project(&kx);
    Fixture {
        _tmp: tmp,
        kx,
        work,
    }
}

fn project(kx: &Utf8Path) -> ProjectDir {
    Kindex::open(kx)
        .unwrap()
        .project(&ProjectName::new("p").unwrap())
        .unwrap()
}

impl Fixture {
    fn dir(&self) -> ProjectDir {
        project(&self.kx)
    }

    fn write(&self, path: &str, text: &str) {
        std::fs::write(self.work.join(path), text).unwrap();
    }

    /// A read-only engine, or `None`.
    fn reader(&self) -> Option<Engine> {
        let mut reg = ExtensionRegistry::new();
        reg.register(Box::new(TestExtractor));
        let roots = BTreeMap::from([("r".to_string(), self.work.clone())]);
        Engine::open_reader(self.dir(), reg, roots).unwrap()
    }

    /// An indexed project with `a.tx` and its writer.
    fn indexed(&self) -> Engine {
        self.write("a.tx", "sym fn A\n");
        let e = open_engine(&self.kx, &self.work);
        e.inbox().append(&file_event("a.tx")).unwrap();
        drained_graph(&e);
        e
    }

    /// Queue `path` in the writer's cache without processing it.
    fn queue_only(&self, e: &Engine, path: &str) {
        self.write(path, "sym fn B\n");
        e.inbox().append(&file_event(path)).unwrap();
        let mut p = Processor::try_acquire(e).unwrap().unwrap();
        p.ingest().unwrap();
    }
}

fn opts(consistency: Consistency, timeout: Duration, scope: Scope) -> QueryOptions {
    QueryOptions {
        consistency,
        timeout,
        scope,
    }
}

fn scope_of(paths: &[&str]) -> Scope {
    Scope::Paths(
        paths
            .iter()
            .map(|p| ("r".to_string(), RelPath::new(p).unwrap()))
            .collect::<BTreeSet<_>>(),
    )
}

fn artifacts(r: &Engine, o: &QueryOptions) -> katalogr_engine::Answer<usize> {
    r.with_consistency(o, |tx| tx.artifacts().unwrap().len())
        .unwrap()
}

#[allow(non_snake_case)]
fn STALE() -> QueryOptions {
    opts(Consistency::StaleOk, Duration::from_secs(5), Scope::Project)
}

fn ls(dir: &Utf8Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    v.sort();
    v
}

mod readers {
    use super::*;

    /// R-1: a read-only engine answers from the last committed state while the writer is open,
    /// with a write transaction in flight.
    #[test]
    fn stale_ok_answers_beside_writer() {
        let f = fixture();
        let w = f.indexed();
        let r = f.reader().expect("a read-only handle beside the writer");
        assert!(r.cache().is_reader());
        let in_flight = w.cache().write().unwrap();
        let a = artifacts(&r, &STALE());
        assert_eq!((a.value, a.pending, a.stale), (1, 0, false));
        drop(in_flight);
    }

    /// R-1: nothing is written: not the cache file, not a lock file, not a shard.
    #[test]
    fn stale_ok_writes_nothing() {
        let f = fixture();
        let w = f.indexed();
        let dir = f.dir();
        let file = dir.cache_dir().join("index.redb");
        let before = std::fs::read(&file).unwrap();
        let cache_files = ls(&dir.cache_dir());
        let shards = shard_bytes(&f.kx);
        let commits = katalogr_store::cache::commits_on_this_thread();
        let r = f.reader().unwrap();
        for _ in 0..3 {
            artifacts(&r, &STALE());
        }
        assert!(matches!(r.cache().write(), Err(StoreError::Busy)));
        assert_eq!(std::fs::read(&file).unwrap(), before);
        assert_eq!(ls(&dir.cache_dir()), cache_files);
        assert_eq!(shard_bytes(&f.kx), shards);
        assert_eq!(katalogr_store::cache::commits_on_this_thread(), commits);
        drop(w);
    }

    /// R-2: queued changes are reported with the snapshot's watermark.
    #[test]
    fn stale_ok_marks_pending() {
        let f = fixture();
        let w = f.indexed();
        let r = f.reader().unwrap();
        let clean = artifacts(&r, &STALE());
        f.queue_only(&w, "b.tx");
        let a = artifacts(&r, &STALE());
        assert_eq!(a.value, 1, "the queued file is not in the snapshot");
        assert_eq!(a.pending, 1);
        assert!(a.stale);
        assert_eq!(a.watermark, clean.watermark);
        drained_graph(&w);
        let a = artifacts(&r, &STALE());
        assert_eq!((a.value, a.pending, a.stale), (2, 0, false));
    }

    /// R-2: a snapshot holds whole published rounds; an open write transaction is invisible
    /// until it commits.
    #[test]
    fn reader_sees_whole_rounds() {
        let f = fixture();
        let w = f.indexed();
        let r = f.reader().unwrap();
        let key = |p: &str| PendingKey {
            context: "worktree".into(),
            repo: "r".into(),
            path: RelPath::new(p).unwrap(),
        };
        let mut tx = w.cache().write().unwrap();
        tx.put_pending(&key("x.tx"), 7).unwrap();
        tx.put_pending(&key("y.tx"), 8).unwrap();
        let during = artifacts(&r, &STALE());
        assert_eq!(during.pending, 0);
        tx.commit().unwrap();
        let after = artifacts(&r, &STALE());
        assert_eq!(after.pending, 2, "both entries of the round, never one");
    }

    /// R-3: `wait` polls on a read-only handle and returns fresh once the writer drains.
    #[test]
    fn wait_returns_once_queue_drains() {
        let f = fixture();
        let w = f.indexed();
        f.queue_only(&w, "b.tx");
        let r = f.reader().unwrap();
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(300));
                drained_graph(&w);
            });
            let t = Instant::now();
            let a = artifacts(
                &r,
                &opts(Consistency::Wait, Duration::from_secs(20), Scope::Project),
            );
            assert_eq!((a.value, a.pending, a.stale), (2, 0, false));
            assert!(t.elapsed() < Duration::from_secs(15));
        });
        assert!(r.cache().is_reader(), "no takeover while the writer lives");
    }

    /// R-3: at the deadline the snapshot is answered, marked stale.
    #[test]
    fn wait_times_out_stale_beside_writer() {
        let f = fixture();
        let w = f.indexed();
        f.queue_only(&w, "b.tx");
        let r = f.reader().unwrap();
        let t = Instant::now();
        let a = artifacts(
            &r,
            &opts(
                Consistency::Wait,
                Duration::from_millis(400),
                scope_of(&["b.tx"]),
            ),
        );
        assert!(t.elapsed() >= Duration::from_millis(400));
        assert!(t.elapsed() < Duration::from_secs(5));
        assert_eq!((a.value, a.pending, a.stale), (1, 1, true));
        // A scope the queue does not intersect answers at once, fresh.
        let t = Instant::now();
        let a = artifacts(
            &r,
            &opts(
                Consistency::Wait,
                Duration::from_secs(5),
                scope_of(&["a.tx"]),
            ),
        );
        assert!(t.elapsed() < Duration::from_secs(2));
        assert!(!a.stale);
    }

    #[test]
    fn wait_all_times_out_stale_beside_writer() {
        let f = fixture();
        let w = f.indexed();
        f.queue_only(&w, "b.tx");
        let r = f.reader().unwrap();
        let t = Instant::now();
        let a = artifacts(
            &r,
            &opts(
                Consistency::WaitAll,
                Duration::from_millis(400),
                scope_of(&["a.tx"]),
            ),
        );
        assert!(t.elapsed() >= Duration::from_millis(400));
        assert_eq!((a.value, a.pending, a.stale), (1, 1, true));
    }

    /// R-4: the writer exits with paths queued; the polling query becomes the writer and drains.
    #[test]
    fn wait_takes_over_when_writer_exits() {
        let f = fixture();
        let w = f.indexed();
        f.queue_only(&w, "b.tx");
        let r = f.reader().unwrap();
        let gone = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(w);
        });
        let a = artifacts(
            &r,
            &opts(Consistency::Wait, Duration::from_secs(30), Scope::Project),
        );
        gone.join().unwrap();
        assert_eq!((a.value, a.pending, a.stale), (2, 0, false));
        assert!(!r.cache().is_reader(), "the query took the writer role");
    }

    /// R-5: `stale-ok` never times out, so a no-match keeps its own code. The exit mapping is in
    /// `render::json::tests`; here the report's `timed_out` input is false by construction:
    /// the answer is stale but no wait was asked for.
    #[test]
    fn stale_ok_no_match_keeps_exit_four() {
        let f = fixture();
        let w = f.indexed();
        f.queue_only(&w, "b.tx");
        let r = f.reader().unwrap();
        let a = r
            .with_consistency(&STALE(), |tx| {
                tx.symbol_ids_by_plain_ci("never_indexed_name")
                    .unwrap()
                    .len()
            })
            .unwrap();
        assert_eq!(a.value, 0);
        assert!(a.stale, "stale-ok reports staleness without waiting");
    }

    /// R-6: no file yet.
    #[test]
    fn missing_file_uses_writer_open() {
        let f = fixture();
        assert!(f.reader().is_none());
        assert!(!f.dir().cache_dir().join("index.redb").exists());
        let w = open_engine(&f.kx, &f.work);
        assert!(f.reader().is_some());
        drop(w);
    }

    /// R-6: a file a writer has just created, not yet initialised, is not an error for a reader.
    #[test]
    fn empty_file_uses_writer_open() {
        let f = fixture();
        let path = f.dir().cache_dir().join("index.redb");
        std::fs::create_dir_all(f.dir().cache_dir()).unwrap();
        std::fs::write(&path, b"").unwrap();
        assert!(f.reader().is_none());
    }

    /// R-6: a cache of another schema is left for the writer open to recreate.
    #[test]
    fn other_schema_uses_writer_open() {
        let f = fixture();
        let w = f.indexed();
        let mut tx = w.cache().write().unwrap();
        tx.set_meta(
            "cache_schema",
            &(katalogr_store::CACHE_SCHEMA + 1).to_le_bytes(),
        )
        .unwrap();
        tx.commit().unwrap();
        assert!(f.reader().is_none());
        drop(w);
        let w = open_engine(&f.kx, &f.work);
        assert!(f.reader().is_some(), "recreated at the current schema");
        drop(w);
    }

    /// R-11: an index beside readers publishes the same shards and graph as one without.
    #[test]
    fn index_beside_readers_identical() {
        let f = fixture();
        for i in 0..40 {
            f.write(
                &format!("f{i}.tx"),
                &format!("sym fn S{i}\nsym fn T{i}\nlink S{i} calls T{i}\n"),
            );
        }
        let stop = std::sync::atomic::AtomicBool::new(false);
        let answered = std::sync::atomic::AtomicUsize::new(0);
        let graph = std::thread::scope(|s| {
            for _ in 0..2 {
                s.spawn(|| {
                    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                        if let Some(r) = f.reader() {
                            artifacts(&r, &STALE());
                            answered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                });
            }
            let w = open_engine(&f.kx, &f.work);
            let g = support::full_index(&w);
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
            g
        });
        let root = Utf8PathBuf::from_path_buf(f._tmp.path().to_path_buf()).unwrap();
        let (fresh_kx, fresh) = fresh_index(&root, "fresh", &f.work);
        assert_eq!(graph, fresh);
        assert_eq!(shard_bytes(&f.kx), shard_bytes(&fresh_kx));
    }

    #[cfg(feature = "failpoints")]
    use katalogr_store::Cache;
    #[cfg(feature = "failpoints")]
    use std::process::Command;

    #[cfg(feature = "failpoints")]
    const CHILD_ENV: &str = "KATALOGR_READERS_CHILD";

    /// The child half: not a test of its own; does nothing unless the parent set the variable.
    #[cfg(feature = "failpoints")]
    #[test]
    fn readers_child() {
        let Ok(spec) = std::env::var(CHILD_ENV) else {
            return;
        };
        let (kx, work) = spec.split_once('|').unwrap();
        let e = open_engine(Utf8Path::new(kx), Utf8Path::new(work));
        for p in ["a.tx", "b.tx"] {
            e.inbox().append(&file_event(p)).unwrap();
        }
        drained_graph(&e);
    }

    /// An index whose child dies right after a publish commit.
    #[cfg(feature = "failpoints")]
    fn crashed() -> Fixture {
        let f = fixture();
        f.write("a.tx", "sym fn A\n");
        f.write("b.tx", "sym fn B\n");
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "readers::readers_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, format!("{}|{}", f.kx, f.work))
            .env("KATALOGR_FAILPOINT", "after_publish_commit")
            .status()
            .unwrap();
        assert!(!status.success(), "the child survived the failpoint");
        f
    }

    /// R-6: a crashed writer leaves a file no reader can open; the writer open repairs it, and
    /// readers work again.
    #[cfg(feature = "failpoints")]
    #[test]
    fn after_writer_crash_repairs_then_reads() {
        let f = crashed();
        assert!(
            f.reader().is_none(),
            "needs repair: the writer open is used"
        );
        let w = open_engine(&f.kx, &f.work);
        let r = f.reader().expect("readable after the repair");
        let a = artifacts(&r, &STALE());
        assert_eq!(a.pending, 0);
        drop(w);
    }

    /// R-6: a repair past its deadline gives up with `Busy`; a longer one finishes it.
    #[cfg(feature = "failpoints")]
    #[test]
    fn repair_stops_at_deadline() {
        let f = crashed();
        let dir = f.dir();
        let past = Instant::now();
        let err = Cache::open_or_rebuild(&dir, &katalogr_engine::Stitcher::new(), past)
            .err()
            .expect("the repair is aborted");
        assert!(matches!(err, StoreError::Busy), "{err:?}");
        let ok = Cache::open_or_rebuild(
            &dir,
            &katalogr_engine::Stitcher::new(),
            Instant::now() + Duration::from_secs(30),
        );
        assert!(ok.is_ok());
    }
}
