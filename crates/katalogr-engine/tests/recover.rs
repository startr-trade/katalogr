//! FEAT-094 R-8 to R-11: a lost, old or corrupt graph database is rebuilt by extracting. The
//! engine reports what opening found (`Cache::state`); the caller queues the rescans (the facade
//! does, in `katalogr-dist`).
mod support;

use camino::{Utf8Path, Utf8PathBuf};
use katalogr_engine::{Engine, NewEvent};
use katalogr_store::{CACHE_SCHEMA, Cache, OpenState, Reader};
use std::time::{Duration, Instant};
use support::{drained_graph, full_index, init_project, open_engine};

struct Fx {
    _tmp: tempfile::TempDir,
    kx: Utf8PathBuf,
    work: Utf8PathBuf,
}

fn cache_file(f: &Fx) -> Utf8PathBuf {
    f.kx.join("projects/p/.cache/index.redb")
}

/// A project with a few files, indexed fully; the first graph is returned with it.
fn indexed(files: &[(&str, String)]) -> (Fx, String) {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let (kx, work) = (root.join("kx"), root.join("work"));
    std::fs::create_dir_all(&work).unwrap();
    for (name, text) in files {
        std::fs::write(work.join(name), text).unwrap();
    }
    init_project(&kx);
    let f = Fx {
        _tmp: tmp,
        kx,
        work,
    };
    let graph = {
        let e = open_engine(&f.kx, &f.work);
        assert_eq!(e.cache().state(), OpenState::Created);
        full_index(&e)
    };
    (f, graph)
}

fn plain() -> Vec<(&'static str, String)> {
    vec![
        ("a.tx", "sym fn A\nexport k:a A\n".to_string()),
        ("b.tx", "sym fn B\nref B calls k:a\n".to_string()),
    ]
}

/// What the facade does on a replaced graph: queue one rescan for the repo, then drain.
fn rebuild(e: &Engine) -> String {
    e.inbox().append(&NewEvent::rescan("r")).unwrap();
    drained_graph(e)
}

fn project_dir(f: &Fx) -> katalogr_store::ProjectDir {
    katalogr_store::Kindex::open(&f.kx)
        .unwrap()
        .project(&katalogr_store::ProjectName::new("p").unwrap())
        .unwrap()
}

fn set_schema(f: &Fx, schema: u32) {
    let dir = project_dir(f);
    let c = Cache::open(&dir, Instant::now() + Duration::from_secs(5)).unwrap();
    let mut tx = c.write().unwrap();
    tx.set_meta("cache_schema", &schema.to_le_bytes()).unwrap();
    tx.commit().unwrap();
}

mod recover {
    use super::*;

    /// R-8.
    #[test]
    fn schema_mismatch_requeues() {
        let (f, first) = indexed(&plain());
        set_schema(&f, CACHE_SCHEMA + 1);
        let e = open_engine(&f.kx, &f.work);
        assert_eq!(e.cache().state(), OpenState::Replaced);
        assert!(e.cache().read().unwrap().artifacts().unwrap().is_empty());
        assert_eq!(rebuild(&e), first);
    }

    /// R-8.
    #[test]
    fn corrupt_cache_requeues() {
        let (f, first) = indexed(&plain());
        std::fs::write(cache_file(&f), vec![0xA5u8; 16 * 1024]).unwrap();
        let e = open_engine(&f.kx, &f.work);
        assert_eq!(e.cache().state(), OpenState::Replaced);
        assert_eq!(rebuild(&e), first);
    }

    /// R-9.
    #[test]
    fn missing_cache_warns_and_queues_nothing() {
        let (f, _) = indexed(&plain());
        std::fs::remove_file(cache_file(&f)).unwrap();
        let e = open_engine(&f.kx, &f.work);
        assert_eq!(e.cache().state(), OpenState::Created);
        assert!(e.inbox().list().unwrap().is_empty(), "nothing is queued");
        let r = e.cache().read().unwrap();
        assert!(r.pending_entries(None).unwrap().is_empty());
        assert!(r.artifacts().unwrap().is_empty(), "an empty graph answers");
    }

    /// R-9.
    #[test]
    fn deleted_cache_reindexes_identical() {
        let (f, first) = indexed(&plain());
        std::fs::remove_file(cache_file(&f)).unwrap();
        let e = open_engine(&f.kx, &f.work);
        assert_eq!(full_index(&e), first);
    }

    /// R-11. Refs in the file are in reverse-sorted order; the rebuilt graph is the graph
    /// extraction produces, reference indexes included.
    #[test]
    fn rebuilt_graph_keeps_reference_indexes() {
        let files = vec![
            ("a.tx", "sym fn A\nexport k:a A\nexport k:b A\nexport k:c A\n".to_string()),
            (
                "z.tx",
                "sym fn Z\nref Z calls k:c\nref Z calls k:b\nref Z calls k:a\nref Z uses k:missing\n"
                    .to_string(),
            ),
        ];
        let (f, first) = indexed(&files);
        let dump = |f: &Fx| {
            let e = open_engine(&f.kx, &f.work);
            let d = e.cache().read().unwrap().dump_graph().unwrap();
            drop(e);
            d
        };
        let before = dump(&f);
        std::fs::remove_file(cache_file(&f)).unwrap();
        let e = open_engine(&f.kx, &f.work);
        assert_eq!(full_index(&e), first);
        drop(e);
        assert_eq!(
            dump(&f),
            before,
            "tables, including refs_by_key and fan_out"
        );
    }
}

/// A file the privacy guard refuses leaves an index-error row and no rows.
fn bad_files(kx: &Utf8Path) -> Vec<(&'static str, String)> {
    vec![
        ("ok.tx", "sym fn alpha\n".to_string()),
        ("bad.tx", format!("sym {kx} beta\n")),
    ]
}

fn error_rows(e: &Engine) -> usize {
    e.cache()
        .read()
        .unwrap()
        .index_errors_all()
        .unwrap()
        .iter()
        .map(|(_, rows)| rows.len())
        .sum()
}

mod errors {
    use super::*;

    /// R-11.
    #[test]
    fn survive_rescan() {
        // The kindex root is only known once the temp dir exists: build the files after.
        let (f, _) = indexed(&[("ok.tx", "sym fn alpha\n".to_string())]);
        for (name, text) in bad_files(&f.kx) {
            std::fs::write(f.work.join(name), text).unwrap();
        }
        let e = open_engine(&f.kx, &f.work);
        full_index(&e);
        assert_eq!(error_rows(&e), 1);
        rebuild(&e);
        assert_eq!(error_rows(&e), 1, "the error row survives a rescan");
    }

    /// R-11.
    #[test]
    fn survive_rebuild() {
        let (f, _) = indexed(&[("ok.tx", "sym fn alpha\n".to_string())]);
        for (name, text) in bad_files(&f.kx) {
            std::fs::write(f.work.join(name), text).unwrap();
        }
        let first = {
            let e = open_engine(&f.kx, &f.work);
            let g = full_index(&e);
            assert_eq!(error_rows(&e), 1);
            g
        };
        std::fs::remove_file(cache_file(&f)).unwrap();
        let e = open_engine(&f.kx, &f.work);
        assert_eq!(error_rows(&e), 0, "the new graph is empty");
        assert_eq!(full_index(&e), first);
        assert_eq!(error_rows(&e), 1, "the rebuild records the error again");
    }
}
