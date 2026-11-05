//! FEAT-047 R-1 and R-11: crash injection. The test re-runs its own binary as a child with
//! `KATALOGR_FAILPOINT` set; the child aborts at the failpoint and the parent checks the
//! recovery invariants of section 8. Run with `--features failpoints`.
#![cfg(feature = "failpoints")]

mod support;

use camino::{Utf8Path, Utf8PathBuf};
use katalogr_engine::Engine;
use katalogr_engine::queue::Processor;
use katalogr_store::Reader;
use std::process::Command;
use support::{
    drained_graph, file_event, fresh_index, full_index, init_project, open_engine,
    open_engine_threads,
};

const CHILD_ENV: &str = "KATALOGR_CRASH_CHILD";
/// The paths the child reports, in order. `a` is rewritten, `b` deleted, `c` new.
const PATHS: [&str; 3] = ["a.tx", "b.tx", "c.tx"];

/// Set for the child to compact the cache with a rewrite after it drained (FEAT-075 R-7).
const REWRITE_ENV: &str = "KATALOGR_CRASH_REWRITE";

/// The failpoints of a cache rewrite.
const REWRITE_FAILPOINTS: [&str; 2] = ["before_rewrite_rename", "after_rewrite_rename"];

const FAILPOINTS: [&str; 4] = [
    "after_inbox_rename",
    "after_ingest_commit",
    "before_inbox_delete",
    "after_publish_commit",
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
    run_child_with(s, failpoint, threads, false);
}

fn run_child_with(s: &Scenario, failpoint: &str, threads: usize, rewrite: bool) {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    if rewrite {
        cmd.env(REWRITE_ENV, "1");
    }
    let status = cmd
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
    if std::env::var(REWRITE_ENV).is_ok() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        e.compact_cache(katalogr_store::CompactMode::Rewrite, deadline)
            .unwrap();
    }
}

const KILL_CHILD_ENV: &str = "KATALOGR_KILL_CHILD";

/// The child half of `killed_index_finishes_identical`: indexes everything and is killed by the
/// parent meanwhile.
#[test]
fn kill_child() {
    let Ok(spec) = std::env::var(KILL_CHILD_ENV) else {
        return;
    };
    let (kx, work) = spec.split_once('|').unwrap();
    let e = open_engine_threads(Utf8Path::new(kx), Utf8Path::new(work), 1);
    full_index(&e);
}

fn acked(s: &Scenario) -> Vec<String> {
    std::fs::read_to_string(s.acks())
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Every artifact has exactly one file revision, and the other way round.
fn check_revisions(e: &Engine) {
    let r = e.cache().read().unwrap();
    let revisions = r.file_revisions().unwrap();
    let artifacts = r.artifacts().unwrap();
    assert_eq!(artifacts.len(), revisions.len());
    for (_, row) in artifacts {
        let found = r
            .file_revision(&row.provenance.repo, &row.provenance.path)
            .unwrap();
        assert!(found.is_some(), "{}", row.provenance.path);
    }
}

mod crash {
    use super::*;

    /// The crash-injection scenario with the child (and the recovering parent) at `threads`.
    fn no_loss(threads: usize) {
        no_loss_at(threads, &FAILPOINTS);
    }

    fn no_loss_at(threads: usize, failpoints: &[&str]) {
        no_loss_with(threads, failpoints, false);
    }

    fn no_loss_with(threads: usize, failpoints: &[&str], rewrite: bool) {
        for &name in failpoints {
            let s = scenario();
            run_child_with(&s, name, threads, rewrite);
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

            // There is nothing to finish: one commit holds rows and file revisions together.
            check_revisions(&e);

            // Producers re-send what was never acknowledged; then the queue is drained.
            for p in &PATHS[acks.len()..] {
                e.inbox().append(&file_event(p)).unwrap();
            }
            let graph = drained_graph(&e);
            check_revisions(&e);
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

    /// FEAT-075 R-7. A process that dies before or after the rename of a cache rewrite loses
    /// nothing: the reopened cache holds the drained graph and every acknowledged event, and no
    /// rewrite file is left behind.
    #[test]
    fn crash_during_rewrite_no_loss() {
        no_loss_with(1, &REWRITE_FAILPOINTS, true);
        no_loss_with(4, &REWRITE_FAILPOINTS, true);
        // The orphan of a crash before the rename is removed by the next open.
        let s = scenario();
        run_child_with(&s, "before_rewrite_rename", 1, true);
        let orphan =
            s.kx.join("projects/p/.cache")
                .join(katalogr_store::REWRITE_FILE);
        assert!(orphan.exists(), "the child left its rewrite file");
        let _e = open_engine(&s.kx, &s.work);
        assert!(!orphan.exists());
    }

    /// R-3. A child process indexing a tree is killed with SIGKILL at several moments; the
    /// reopened project drains to the graph a fresh index of the same tree gives.
    #[test]
    fn killed_index_finishes_identical() {
        for delay_ms in [30u64, 120, 400] {
            let tmp = tempfile::tempdir().unwrap();
            let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
            let (kx, work) = (root.join("kx"), root.join("work"));
            std::fs::create_dir_all(&work).unwrap();
            for i in 0..1500 {
                std::fs::write(
                    work.join(format!("f{i}.tx")),
                    format!(
                        "sym fn F{i}\nexport k:f{i} F{i}\nref F{i} calls k:f{}\n",
                        i / 2
                    ),
                )
                .unwrap();
            }
            init_project(&kx);
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "kill_child", "--nocapture", "--test-threads=1"])
                .env(KILL_CHILD_ENV, format!("{kx}|{work}"))
                .env("KATALOGR_THREADS", "1")
                .spawn()
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            // `kill` sends SIGKILL.
            let _ = child.kill();
            let _ = child.wait();

            let e = open_engine_threads(&kx, &work, 1);
            check_revisions(&e);
            let graph = full_index(&e);
            check_revisions(&e);
            let (_, fresh) = fresh_index(&root, "fresh", &work);
            assert_eq!(graph, fresh, "killed after {delay_ms} ms");
        }
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

const SEED_CHILD_ENV: &str = "KATALOGR_SEED_CHILD";

fn seed_dl() -> std::time::Instant {
    std::time::Instant::now() + std::time::Duration::from_secs(10)
}

fn seed_manifest(id: &katalogr_store::ContextId) -> katalogr_store::ContextManifest {
    katalogr_store::ContextManifest {
        format: 1,
        id: id.clone(),
        kind: katalogr_store::ContextKind::Checkout,
        repo: "r".into(),
        root: None,
        seed_commit: None,
        commit: None,
    }
}

/// The child half of `seed_interrupted_leaves_no_layer`.
#[test]
fn seed_child() {
    let Ok(spec) = std::env::var(SEED_CHILD_ENV) else {
        return;
    };
    let (kx, work) = spec.split_once('|').unwrap();
    let e = open_engine(Utf8Path::new(kx), Utf8Path::new(work));
    let id = katalogr_store::ContextId::new("w-crash").unwrap();
    let dir = e.dir().clone();
    drop(e);
    katalogr_store::seed_layer(&dir, &dir.layer(&id), &seed_manifest(&id), seed_dl()).unwrap();
}

/// R-11: a seed that dies leaves no usable layer, and the next seed completes.
#[test]
fn seed_interrupted_leaves_no_layer() {
    for name in ["seed_after_copy", "seed_before_rename"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let (kx, work) = (root.join("kx"), root.join("work"));
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("a.tx"), "sym fn A\nexport k:a A\n").unwrap();
        init_project(&kx);
        {
            let e = open_engine(&kx, &work);
            full_index(&e);
        }
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "seed_child", "--nocapture", "--test-threads=1"])
            .env(SEED_CHILD_ENV, format!("{kx}|{work}"))
            .env("KATALOGR_FAILPOINT", name)
            .status()
            .unwrap();
        assert!(!status.success(), "child survived {name}");

        let e = open_engine(&kx, &work);
        let id = katalogr_store::ContextId::new("w-crash").unwrap();
        let dir = e.dir().clone();
        let layer = dir.layer(&id);
        assert!(!layer.root().join("context.json").exists(), "{name}");
        assert!(dir.contexts().unwrap().is_empty(), "{name}");
        // The base is intact and the layer can be seeded again.
        assert!(
            !e.cache()
                .read()
                .unwrap()
                .file_revisions()
                .unwrap()
                .is_empty()
        );
        drop(e);
        katalogr_store::seed_layer(&dir, &layer, &seed_manifest(&id), seed_dl()).unwrap();
        assert_eq!(dir.contexts().unwrap().len(), 1, "{name}");
        assert!(!layer.cache_dir().join("index.redb.tmp").exists(), "{name}");
    }
}

const LAYER_CHILD_ENV: &str = "KATALOGR_LAYER_CHILD";

fn layer_dir_of(kx: &Utf8Path) -> katalogr_store::ProjectDir {
    let base = katalogr_store::Kindex::open(kx)
        .unwrap()
        .project(&katalogr_store::ProjectName::new("p").unwrap())
        .unwrap();
    base.layer(&katalogr_store::ContextId::new("w-crash").unwrap())
}

fn open_layer(kx: &Utf8Path, work: &Utf8Path) -> Engine {
    let mut reg = katalogr_engine::registry::ExtensionRegistry::new();
    reg.register(Box::new(support::TestExtractor));
    reg.register(Box::new(support::TestAltExtractor));
    reg.register(Box::new(support::TestGuest("1")));
    let roots = std::collections::BTreeMap::from([("r".to_string(), work.to_path_buf())]);
    Engine::open(
        layer_dir_of(kx),
        reg,
        roots,
        std::time::Instant::now() + std::time::Duration::from_secs(10),
    )
    .unwrap()
}

/// The child half of `publish_in_layer_recovers`.
#[test]
fn layer_child() {
    let Ok(spec) = std::env::var(LAYER_CHILD_ENV) else {
        return;
    };
    let (kx, work) = spec.split_once('|').unwrap();
    let (kx, work) = (Utf8Path::new(kx), Utf8Path::new(work));
    let e = open_layer(kx, work);
    for p in PATHS {
        e.inbox().append(&file_event(p)).unwrap();
    }
    drained_graph(&e);
}

/// R-11: a publish that dies inside a context layer is finished on the next open, because the
/// journal paths are relative to the layer root, and the result equals a fresh index.
#[test]
fn publish_in_layer_recovers() {
    for name in ["after_publish_commit"] {
        let s = scenario();
        // The scenario's base is indexed; the edits are still on disk. Seed the layer from it.
        let base = katalogr_store::Kindex::open(&s.kx)
            .unwrap()
            .project(&katalogr_store::ProjectName::new("p").unwrap())
            .unwrap();
        let id = katalogr_store::ContextId::new("w-crash").unwrap();
        katalogr_store::seed_layer(&base, &base.layer(&id), &seed_manifest(&id), seed_dl())
            .unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "layer_child", "--nocapture", "--test-threads=1"])
            .env(LAYER_CHILD_ENV, format!("{}|{}", s.kx, s.work))
            .env("KATALOGR_FAILPOINT", name)
            .status()
            .unwrap();
        assert!(!status.success(), "child survived {name}");

        let e = open_layer(&s.kx, &s.work);
        check_revisions(&e);
        for p in PATHS {
            e.inbox().append(&file_event(p)).unwrap();
        }
        let graph = drained_graph(&e);
        check_revisions(&e);
        let fresh_root = Utf8PathBuf::from_path_buf(s._tmp.path().to_path_buf()).unwrap();
        let (_, fresh) = fresh_index(&fresh_root, "fresh", &s.work);
        assert_eq!(
            graph, fresh,
            "{name}: layer graph differs from a fresh index"
        );
    }
}
