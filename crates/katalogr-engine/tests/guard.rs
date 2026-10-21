mod support;

use camino::Utf8PathBuf;
use katalogr_engine::registry::ExtensionRegistry;
use katalogr_engine::{Consistency, Engine, QueryOptions, Scope, queue::Processor};
use katalogr_model::RelPath;
use katalogr_store::Reader;
use std::time::{Duration, Instant};
use support::{TestExtractor, file_event, init_project, open_engine_with};

struct Fx {
    _tmp: tempfile::TempDir,
    kx: Utf8PathBuf,
    e: Engine,
}

/// A project with `ok.tx` (clean) and `bad.tx` (its symbol kind holds the kindex root), both
/// enqueued but not yet processed.
fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let kx = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
    let work = Utf8PathBuf::from_path_buf(tmp.path().join("work")).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    init_project(&kx);
    std::fs::write(work.join("ok.tx"), "sym fn alpha\n").unwrap();
    std::fs::write(work.join("bad.tx"), format!("sym {kx} beta\n")).unwrap();
    let mut reg = ExtensionRegistry::new();
    reg.register(Box::new(TestExtractor));
    let e = open_engine_with(&kx, &work, reg);
    e.inbox().append(&file_event("ok.tx")).unwrap();
    e.inbox().append(&file_event("bad.tx")).unwrap();
    Fx { _tmp: tmp, kx, e }
}

fn publish(e: &Engine) {
    let mut p = Processor::try_acquire(e).unwrap().unwrap();
    p.ingest().unwrap();
    p.process(
        &Scope::Project,
        256,
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
}

mod guard {
    use super::*;

    #[test]
    fn refusal_skips_only_that_file() {
        let f = fixture();
        publish(&f.e);
        let rel = |p: &str| RelPath::new(p).unwrap();
        assert!(f.e.dir().shard_path("r", &rel("ok.tx")).exists());
        assert!(!f.e.dir().shard_path("r", &rel("bad.tx")).exists());
        let shards = f.e.dir().root().join("shards/r");
        for entry in std::fs::read_dir(shards).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().to_string();
            assert!(!name.starts_with(".tmp-"), "leftover temp file {name}");
        }
        let r = f.e.cache().read().unwrap();
        let names: Vec<String> = r
            .artifacts()
            .unwrap()
            .into_iter()
            .map(|(_, a)| a.name.path.as_str().to_string())
            .collect();
        assert_eq!(names, vec!["ok.tx".to_string()]);
        assert!(r.pending_entries(None).unwrap().is_empty());
        assert!(f.e.inbox().list().unwrap().is_empty());
    }

    #[test]
    fn refusal_row_does_not_echo_value() {
        let f = fixture();
        publish(&f.e);
        let r = f.e.cache().read().unwrap();
        let all = r.index_errors_all().unwrap();
        assert_eq!(all.len(), 1, "{all:?}");
        let ((repo, path), rows) = &all[0];
        assert_eq!((repo.as_str(), path.as_str()), ("r", "bad.tx"));
        assert_eq!(rows.len(), 1);
        let m = &rows[0].message;
        assert!(m.starts_with("shard refused:"), "{m}");
        assert!(m.contains("symbols[0].name"), "{m}");
        assert!(!m.contains(f.kx.as_str()), "value echoed: {m}");
    }

    #[test]
    fn query_after_refusal_answers() {
        let f = fixture();
        publish(&f.e);
        let o = QueryOptions {
            consistency: Consistency::Wait,
            timeout: Duration::from_secs(5),
            scope: Scope::Project,
        };
        let a =
            f.e.with_consistency(&o, |tx| {
                tx.symbols()
                    .unwrap()
                    .into_iter()
                    .map(|(_, s)| s.name.segments[0].name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap();
        assert_eq!(a.value, vec!["alpha".to_string()]);
    }
}
