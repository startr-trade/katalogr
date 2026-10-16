mod support;

use camino::Utf8PathBuf;
use katalogr_engine::registry::ExtensionRegistry;
use katalogr_engine::{
    Consistency, Engine, EventKind, NewEvent, QueryOptions, Scope, queue::Processor,
};
use katalogr_model::{ExtractError, ExtractInput, Extractor, Fragment, RelPath};
use katalogr_store::{Kindex, ProjectConfig, ProjectName, Reader, RepoConfig};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use support::TestExtractor;

/// Delegates to the test extractor and counts extractions.
struct Counting(Arc<AtomicUsize>);

impl Extractor for Counting {
    fn id(&self) -> &'static str {
        "test"
    }
    fn handles(&self, path: &RelPath) -> bool {
        TestExtractor.handles(path)
    }
    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        TestExtractor.extract(input)
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    kx: Utf8PathBuf,
    work: Utf8PathBuf,
    count: Arc<AtomicUsize>,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let kx = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
    let work = Utf8PathBuf::from_path_buf(tmp.path().join("work")).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    Kindex::init(&kx)
        .unwrap()
        .create_project(
            &ProjectName::new("p").unwrap(),
            ProjectConfig {
                repos: vec![RepoConfig::new("r")],
                ..Default::default()
            },
        )
        .unwrap();
    Fixture {
        _tmp: tmp,
        kx,
        work,
        count: Arc::new(AtomicUsize::new(0)),
    }
}

impl Fixture {
    fn engine(&self) -> Engine {
        let dir = Kindex::open(&self.kx)
            .unwrap()
            .project(&ProjectName::new("p").unwrap())
            .unwrap();
        let mut reg = ExtensionRegistry::new();
        reg.register(Box::new(Counting(self.count.clone())));
        let roots = BTreeMap::from([("r".to_string(), self.work.clone())]);
        Engine::open(dir, reg, roots, Instant::now() + Duration::from_secs(5)).unwrap()
    }

    fn write(&self, path: &str, text: &str) {
        std::fs::write(self.work.join(path), text).unwrap();
    }

    fn notify(&self, e: &Engine, path: &str) -> String {
        e.inbox()
            .append(&NewEvent::file("r", &rel(path), EventKind::Changed))
            .unwrap()
    }

    fn extractions(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

fn rel(p: &str) -> RelPath {
    RelPath::new(p).unwrap()
}

fn opts(consistency: Consistency, scope: Scope) -> QueryOptions {
    QueryOptions {
        consistency,
        timeout: Duration::from_secs(5),
        scope,
    }
}

fn scope_of(paths: &[&str]) -> Scope {
    Scope::Paths(
        paths
            .iter()
            .map(|p| ("r".to_string(), rel(p)))
            .collect::<BTreeSet<_>>(),
    )
}

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

fn artifact_count(tx: &impl Reader) -> usize {
    tx.artifacts().unwrap().len()
}

mod queue {
    use super::*;

    #[test]
    fn monotonic_sequence() {
        let f = fixture();
        let e = f.engine();
        let ids: Vec<String> = ["a.tx", "b.tx", "c.tx"]
            .iter()
            .map(|p| f.notify(&e, p))
            .collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "inbox ids sort in arrival order");

        let mut p = Processor::try_acquire(&e).unwrap().unwrap();
        assert_eq!(p.ingest().unwrap(), 3);
        let r = e.cache().read().unwrap();
        assert_eq!(r.last_seq("worktree").unwrap(), 3);
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(r.seen_seq(id).unwrap(), Some(i as u64 + 1));
        }
        let seqs: Vec<u64> = r
            .pending_entries(Some("worktree"))
            .unwrap()
            .into_iter()
            .map(|(_, s)| s)
            .collect();
        assert_eq!(seqs, vec![1, 2, 3]);
        drop(r);

        // The next batch continues the numbering, and a re-delivered id is ignored.
        let id4 = f.notify(&e, "d.tx");
        let again = e.inbox().dir().join(format!("{}.json", ids[0]));
        std::fs::write(
            &again,
            format!(
                r#"{{"v":1,"id":"{}","context":"worktree","repo":"r","path":"a.tx","kind":"changed"}}"#,
                ids[0]
            ),
        )
        .unwrap();
        assert_eq!(p.ingest().unwrap(), 1);
        let r = e.cache().read().unwrap();
        assert_eq!(r.seen_seq(&id4).unwrap(), Some(4));
        assert_eq!(r.last_seq("worktree").unwrap(), 4);
        assert!(e.inbox().list().unwrap().is_empty());
    }

    #[test]
    fn coalesce_per_file() {
        let f = fixture();
        let e = f.engine();
        f.write("a.tx", "sym fn A\n");
        for _ in 0..5 {
            f.notify(&e, "a.tx");
        }
        let mut p = Processor::try_acquire(&e).unwrap().unwrap();
        assert_eq!(p.ingest().unwrap(), 5);
        let rows = e.cache().read().unwrap().pending_entries(None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, 5, "the row carries the latest seq");

        let wm = p.process(&Scope::Project, 256, soon()).unwrap();
        assert_eq!(f.extractions(), 1);
        assert_eq!(wm.0, 5);
        assert_eq!(artifact_count(&e.cache().read().unwrap()), 1);
    }

    #[test]
    fn lock_is_exclusive() {
        let f = fixture();
        let e = f.engine();
        let first = Processor::try_acquire(&e).unwrap().unwrap();
        assert!(Processor::try_acquire(&e).unwrap().is_none());
        drop(first);
        assert!(Processor::try_acquire(&e).unwrap().is_some());
    }

    #[test]
    fn rescan_expands_to_files() {
        let f = fixture();
        let e = f.engine();
        f.write("a.tx", "sym fn A\n");
        f.write("b.tx", "sym fn B\n");
        f.write("notes.md", "not handled\n");
        e.inbox().append(&NewEvent::rescan("r")).unwrap();
        let mut p = Processor::try_acquire(&e).unwrap().unwrap();
        p.ingest().unwrap();
        assert_eq!(
            e.cache()
                .read()
                .unwrap()
                .pending_entries(None)
                .unwrap()
                .len(),
            2
        );
        p.process(&Scope::Project, 256, soon()).unwrap();
        assert_eq!(artifact_count(&e.cache().read().unwrap()), 2);
    }

    #[test]
    fn results_and_watermark_atomic() {
        let f = fixture();
        let e = f.engine();
        f.write("a.tx", "sym fn A\n");
        f.write("b.tx", "sym fn B\n");
        f.notify(&e, "a.tx");
        f.notify(&e, "b.tx");

        // Crash after the publish commit: no renames, journal left behind.
        let mut p = Processor::try_acquire(&e).unwrap().unwrap();
        p.ingest().unwrap();
        let wm = p.process_to_commit(&Scope::Project, 256, soon()).unwrap();
        assert_eq!(wm.0, 2);
        drop(p);
        assert_eq!(
            e.cache().read().unwrap().publish_journal().unwrap().len(),
            2
        );
        let dir = e.dir().clone();
        assert!(!dir.shard_path("r", &rel("a.tx")).exists());

        let mut p = Processor::try_acquire(&e).unwrap().unwrap();
        p.recover().unwrap();
        drop(p);
        check_consistent(&e);

        // Same again, with a reopen of the whole engine in between.
        f.write("a.tx", "sym fn A\nsym fn A2\n");
        f.notify(&e, "a.tx");
        let mut p = Processor::try_acquire(&e).unwrap().unwrap();
        p.ingest().unwrap();
        p.process_to_commit(&Scope::Project, 256, soon()).unwrap();
        drop(p);
        drop(e);
        let e = f.engine();
        let mut p = Processor::try_acquire(&e).unwrap().unwrap();
        p.recover().unwrap();
        drop(p);
        check_consistent(&e);
        assert_eq!(e.cache().read().unwrap().symbols().unwrap().len(), 3);
        assert_eq!(e.cache().read().unwrap().watermark("worktree").unwrap(), 3);

        // A deletion removes the shard file through the journal.
        std::fs::remove_file(f.work.join("b.tx")).unwrap();
        f.notify(&e, "b.tx");
        let mut p = Processor::try_acquire(&e).unwrap().unwrap();
        p.ingest().unwrap();
        p.process_to_commit(&Scope::Project, 256, soon()).unwrap();
        drop(p);
        let mut p = Processor::try_acquire(&e).unwrap().unwrap();
        p.recover().unwrap();
        drop(p);
        check_consistent(&e);
        assert_eq!(artifact_count(&e.cache().read().unwrap()), 1);
    }

    /// Every shard file's hash equals `shard_hashes`; nothing is pending or journaled.
    fn check_consistent(e: &Engine) {
        let r = e.cache().read().unwrap();
        assert!(r.publish_journal().unwrap().is_empty());
        assert!(r.pending_entries(None).unwrap().is_empty());
        let on_disk = e.dir().list_shards().unwrap();
        let hashes = r.shard_hashes().unwrap();
        assert_eq!(on_disk.len(), hashes.len());
        for ((repo, path), hash) in hashes {
            let bytes = std::fs::read(e.dir().shard_path(&repo, &path)).unwrap();
            assert_eq!(<[u8; 32]>::from(Sha256::digest(&bytes)), hash);
        }
        assert_eq!(
            r.watermark("worktree").unwrap(),
            r.last_seq("worktree").unwrap()
        );
        assert_eq!(r.event_count("worktree").unwrap(), 0, "events are pruned");
    }

    #[test]
    fn inline_processing_without_daemon() {
        let f = fixture();
        let e = f.engine();
        f.write("a.tx", "sym fn A\n");
        f.notify(&e, "a.tx");
        let a = e
            .with_consistency(&opts(Consistency::Wait, Scope::Project), |tx| {
                artifact_count(tx)
            })
            .unwrap();
        assert_eq!(a.value, 1);
        assert!(!a.stale);
        assert_eq!(a.pending, 0);
        assert!(e.inbox().list().unwrap().is_empty());
    }
}

mod query {
    use super::*;

    #[test]
    fn result_carries_watermark() {
        let f = fixture();
        let e = f.engine();
        f.write("a.tx", "sym fn A\n");
        f.write("b.tx", "sym fn B\n");
        f.notify(&e, "a.tx");
        f.notify(&e, "b.tx");
        let a = e
            .with_consistency(&opts(Consistency::Wait, Scope::Project), |tx| {
                artifact_count(tx)
            })
            .unwrap();
        assert_eq!(a.watermark.0, 2);
        assert_eq!(a.value, 2);
        assert!(!a.stale);
    }

    #[test]
    fn wait_blocks_on_scoped_pending() {
        let f = fixture();
        let e = f.engine();
        f.write("a.tx", "sym fn A\n");
        f.write("b.tx", "sym fn B\n");

        // Pending only outside the scope: returns at once, nothing is processed.
        f.notify(&e, "b.tx");
        let a = e
            .with_consistency(&opts(Consistency::Wait, scope_of(&["a.tx"])), |tx| {
                artifact_count(tx)
            })
            .unwrap();
        assert_eq!((a.value, a.pending, a.stale), (0, 0, false));
        assert_eq!(e.inbox().list().unwrap().len(), 1);

        // Pending inside the scope: processed before the answer.
        f.notify(&e, "a.tx");
        let a = e
            .with_consistency(&opts(Consistency::Wait, scope_of(&["a.tx"])), |tx| {
                tx.artifacts().unwrap().len()
            })
            .unwrap();
        assert!(a.value >= 1);
        assert!(!a.stale);

        // Scope paths are processed first when the budget is small.
        f.write("c.tx", "sym fn C\n");
        f.write("d.tx", "sym fn D\n");
        f.notify(&e, "c.tx");
        f.notify(&e, "d.tx");
        let mut p = Processor::try_acquire(&e).unwrap().unwrap();
        p.ingest().unwrap();
        p.process(&scope_of(&["d.tx"]), 1, soon()).unwrap();
        drop(p);
        let left: Vec<String> = e
            .cache()
            .read()
            .unwrap()
            .pending_entries(None)
            .unwrap()
            .into_iter()
            .map(|(k, _)| k.path.as_str().to_string())
            .collect();
        assert_eq!(left, vec!["c.tx".to_string()]);
    }

    #[test]
    fn wait_all_drains() {
        let f = fixture();
        let e = f.engine();
        for p in ["a.tx", "b.tx", "c.tx"] {
            f.write(p, "sym fn X\n");
            f.notify(&e, p);
        }
        let a = e
            .with_consistency(&opts(Consistency::WaitAll, scope_of(&["a.tx"])), |tx| {
                artifact_count(tx)
            })
            .unwrap();
        assert_eq!(a.value, 3);
        assert_eq!(a.pending, 0);
        assert!(!a.stale);
        assert!(e.inbox().list().unwrap().is_empty());
        assert!(
            e.cache()
                .read()
                .unwrap()
                .pending_entries(None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn stale_ok_marks_stale() {
        let f = fixture();
        let e = f.engine();
        f.write("a.tx", "sym fn A\n");
        f.notify(&e, "a.tx");
        let a = e
            .with_consistency(&opts(Consistency::StaleOk, Scope::Project), |tx| {
                artifact_count(tx)
            })
            .unwrap();
        assert_eq!(a.value, 0);
        assert_eq!(a.pending, 1);
        assert!(a.stale);
        assert_eq!(e.inbox().list().unwrap().len(), 1, "nothing was ingested");
        assert_eq!(f.extractions(), 0);
    }

    #[test]
    fn wait_polls_when_lock_held() {
        let f = fixture();
        let e = f.engine();
        f.write("a.tx", "sym fn A\n");
        f.notify(&e, "a.tx");
        let _held = Processor::try_acquire(&e).unwrap().unwrap();
        let t = Instant::now();
        let a = e
            .with_consistency(
                &QueryOptions {
                    consistency: Consistency::Wait,
                    timeout: Duration::from_millis(200),
                    scope: Scope::Project,
                },
                |tx| artifact_count(tx),
            )
            .unwrap();
        assert!(a.stale);
        assert_eq!(a.pending, 1);
        assert!(t.elapsed() >= Duration::from_millis(200));
    }
}
