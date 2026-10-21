//! FEAT-047 R-1 and R-11: crash injection. The test re-runs its own binary as a child with
//! `KATALOGR_FAILPOINT` set; the child aborts at the failpoint and the parent checks the
//! recovery invariants of section 8. Run with `--features failpoints`.
#![cfg(feature = "failpoints")]

mod support;

use camino::{Utf8Path, Utf8PathBuf};
use katalogr_engine::Engine;
use katalogr_engine::queue::Processor;
use katalogr_store::Reader;
use sha2::{Digest, Sha256};
use std::process::Command;
use support::{
    drained_graph, file_event, fresh_index, full_index, init_project, open_engine,
    open_engine_threads,
};

const CHILD_ENV: &str = "KATALOGR_CRASH_CHILD";
/// The paths the child reports, in order. `a` is rewritten, `b` deleted, `c` new.
const PATHS: [&str; 3] = ["a.tx", "b.tx", "c.tx"];

const FAILPOINTS: [&str; 6] = [
    "after_inbox_rename",
    "after_ingest_commit",
    "before_inbox_delete",
    "after_publish_commit",
    "mid_renames",
    "before_journal_clear",
];

struct Scenario {
    _tmp: tempfile::TempDir,
    kx: Utf8PathBuf,
    work: Utf8PathBuf,
}

impl Scenario {
    fn acks(&self) -> Utf8PathBuf {
        self.kx.with_file_name("acks.txt")
    }
}

/// An indexed project, then edits on disk that are not yet reported: `a` rewritten, `b`
/// deleted, `c` added.
fn scenario() -> Scenario {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let kx = root.join("kx");
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("a.tx"), "sym fn A\nexport k:a A\n").unwrap();
    std::fs::write(work.join("b.tx"), "sym fn B\nref B calls k:a\n").unwrap();
    std::fs::write(work.join("d.tx"), "sym fn D\n").unwrap();
    init_project(&kx);
    {
        let e = open_engine(&kx, &work);
        full_index(&e);
    }
    std::fs::write(work.join("a.tx"), "sym fn A\nsym fn A2\nexport k:a A2\n").unwrap();
    std::fs::remove_file(work.join("b.tx")).unwrap();
    std::fs::write(work.join("c.tx"), "sym fn C\nref C calls k:a\n").unwrap();
    Scenario {
        _tmp: tmp,
        kx,
        work,
    }
}

/// Run the child test in a new process with the failpoint set; it must die.
fn run_child(s: &Scenario, failpoint: &str, threads: usize) {
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, format!("{}|{}", s.kx, s.work))
        .env("KATALOGR_THREADS", threads.to_string())
        .env("KATALOGR_FAILPOINT", failpoint)
        .status()
        .unwrap();
    assert!(!status.success(), "child survived failpoint {failpoint}");
}

/// Not a test of its own: the child half of the crash suite. It does nothing unless the parent
/// set `KATALOGR_CRASH_CHILD`.
#[test]
fn crash_child() {
    let Ok(spec) = std::env::var(CHILD_ENV) else {
        return;
    };
    let (kx, work) = spec.split_once('|').unwrap();
    let (kx, work) = (Utf8Path::new(kx), Utf8Path::new(work));
    let acks = kx.with_file_name("acks.txt");
    let threads = std::env::var("KATALOGR_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let e = open_engine_threads(kx, work, threads);
    for p in PATHS {
        let id = e.inbox().append(&file_event(p)).unwrap();
        // The acknowledgement is recorded only after `append` returned.
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&acks)
            .unwrap();
        use std::io::Write;
        writeln!(f, "{id}").unwrap();
        f.sync_all().unwrap();
    }
    drained_graph(&e);
}

fn acked(s: &Scenario) -> Vec<String> {
    std::fs::read_to_string(s.acks())
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn check_hashes(e: &Engine) {
    let r = e.cache().read().unwrap();
    let hashes = r.shard_hashes().unwrap();
    assert_eq!(e.dir().list_shards().unwrap().len(), hashes.len());
    for ((repo, path), hash) in hashes {
        let bytes = std::fs::read(e.dir().shard_path(&repo, &path)).unwrap();
        assert_eq!(<[u8; 32]>::from(Sha256::digest(&bytes)), hash, "{path}");
    }
}

mod queue {
    use super::*;

    /// The crash-injection scenario with the child (and the recovering parent) at `threads`.
    fn no_loss(threads: usize) {
        for name in FAILPOINTS {
            let s = scenario();
            run_child(&s, name, threads);
            let acks = acked(&s);

            let e = open_engine_threads(&s.kx, &s.work, threads);

            // Every acknowledged event is either ingested or still in the inbox.
            let inbox: Vec<String> = e
                .inbox()
                .list()
                .unwrap()
                .iter()
                .map(|f| f.file_stem().unwrap().to_string())
                .collect();
            {
                let r = e.cache().read().unwrap();
                for id in &acks {
                    assert!(
                        r.seen_seq(id).unwrap().is_some() || inbox.contains(id),
                        "{name}: acknowledged event {id} lost"
                    );
                }
                // The watermark is not ahead of published results.
                let wm = r.watermark("worktree").unwrap();
                assert!(wm <= r.last_seq("worktree").unwrap(), "{name}");
                if let Some(low) = r
                    .pending_entries(None)
                    .unwrap()
                    .into_iter()
                    .map(|(_, q)| q)
                    .min()
                {
                    assert!(wm < low, "{name}: watermark {wm} ahead of pending {low}");
                }
            }

            // After recovery every shard file matches its recorded hash.
            let mut p = Processor::try_acquire(&e).unwrap().unwrap();
            p.recover().unwrap();
            drop(p);
            assert!(
                e.cache()
                    .read()
                    .unwrap()
                    .publish_journal()
                    .unwrap()
                    .is_empty()
            );
            check_hashes(&e);

            // Producers re-send what was never acknowledged; then the queue is drained.
            for p in &PATHS[acks.len()..] {
                e.inbox().append(&file_event(p)).unwrap();
            }
            let graph = drained_graph(&e);
            check_hashes(&e);
            let fresh_root = Utf8PathBuf::from_path_buf(s._tmp.path().to_path_buf()).unwrap();
            let (_, fresh) = fresh_index(&fresh_root, "fresh", &s.work);
            assert_eq!(
                graph, fresh,
                "{name}: final graph differs from a fresh index"
            );
            assert!(graph.contains("fn:A2") && graph.contains("fn:C") && !graph.contains("fn:B"));
        }
    }

    #[test]
    fn crash_injection_no_loss() {
        no_loss(1);
    }

    #[test]
    fn crash_injection_no_loss_parallel() {
        no_loss(4);
    }

    #[test]
    fn ack_after_durable_append() {
        let s = scenario();
        run_child(&s, "after_inbox_rename", 1);
        assert!(acked(&s).is_empty(), "the child died before acknowledging");

        // The event file exists after the crash, and is ingested on reopen.
        let e = open_engine(&s.kx, &s.work);
        let files = e.inbox().list().unwrap();
        assert_eq!(files.len(), 1);
        let id = e.inbox().read(&files[0]).unwrap().id;
        assert!(e.cache().read().unwrap().seen_seq(&id).unwrap().is_none());
        let mut p = Processor::try_acquire(&e).unwrap().unwrap();
        assert_eq!(p.ingest().unwrap(), 1);
        drop(p);
        assert!(e.cache().read().unwrap().seen_seq(&id).unwrap().is_some());
    }
}
