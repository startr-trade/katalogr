//! The embed pass (FEAT-017 R-14, R-22, R-24, R-25; ADR-0021).
mod support;

use camino::Utf8PathBuf;
use katalogr_engine::Engine;
use katalogr_engine::registry::ExtensionRegistry;
use katalogr_model::{Confidence, ContractName, ContractRole, Fragment, Name, RelPath};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use support::{
    CountedExtractor, TestGuest, drained_graph, file_event, full_index, init_project,
    open_engine_with,
};

struct Env {
    _tmp: tempfile::TempDir,
    kx: Utf8PathBuf,
    work: Utf8PathBuf,
    count: Arc<AtomicUsize>,
}

fn env(files: &[(&str, &str)]) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    for (p, t) in files {
        std::fs::write(work.join(p), t).unwrap();
    }
    let kx = root.join("kx");
    init_project(&kx);
    Env {
        _tmp: tmp,
        kx,
        work,
        count: Arc::new(AtomicUsize::new(0)),
    }
}

impl Env {
    fn open(&self, guest_version: &'static str) -> Engine {
        let mut reg = ExtensionRegistry::new();
        reg.register(Box::new(CountedExtractor(self.count.clone())));
        reg.register(Box::new(TestGuest(guest_version)));
        open_engine_with(&self.kx, &self.work, reg)
    }
    fn n(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

fn shard(e: &Engine, path: &str) -> Fragment {
    e.dir()
        .read_shard("r", &RelPath::new(path).unwrap())
        .unwrap()
        .expect("shard")
}

fn shard_bytes(e: &Engine, path: &str) -> Vec<u8> {
    std::fs::read(e.dir().shard_path("r", &RelPath::new(path).unwrap())).unwrap()
}

fn res(key: &str) -> String {
    ContractName {
        kind: "res".into(),
        key: key.into(),
    }
    .canonical()
}

mod embed {
    use super::*;

    #[test]
    fn guest_bindings_attached() {
        let t = env(&[("a.tx", "sym fn f\nlit f use x\n")]);
        let e = t.open("1");
        full_index(&e);
        let f = shard(&e, "a.tx");
        let owner = f.symbols[0].name.id();
        assert_eq!(f.bindings.len(), 1, "{:?}", f.bindings);
        assert_eq!(f.bindings[0].symbol, owner);
        assert_eq!(f.bindings[0].contract.canonical(), res("x"));
    }

    #[test]
    fn embedded_bindings_forced_inferred() {
        let t = env(&[("a.tx", "sym fn f\nlit f use x\n")]);
        let e = t.open("1");
        full_index(&e);
        let f = shard(&e, "a.tx");
        let b = &f.bindings[0];
        assert_eq!(b.confidence, Confidence::Inferred);
        assert_eq!(b.origin.as_deref(), Some("embedded:tguest"));
        assert_eq!(b.symbol, f.symbols[0].name.id());
        assert_eq!(b.role, ContractRole::Consumes);
        assert!(b.span.is_some());
    }

    #[test]
    fn guest_change_triggers_rescan() {
        let t = env(&[("a.tx", "sym fn f\nlit f use x\n")]);
        let e = t.open("1");
        full_index(&e);
        let first = t.n();
        assert!(first >= 1);
        drop(e);
        // A new guest version re-extracts the host file.
        let e = t.open("2");
        drained_graph(&e);
        assert_eq!(t.n(), first + 1);
        drop(e);
        // Reopening with the same guest skips it.
        let e = t.open("2");
        drained_graph(&e);
        assert_eq!(t.n(), first + 1);
    }

    #[test]
    fn call_context_reaches_guest() {
        let t = env(&[(
            "a.tx",
            "sym fn f\nsym fn g\nlit f call=http.get:0 use x\nlit g use y\n",
        )]);
        let e = t.open("1");
        full_index(&e);
        let f = shard(&e, "a.tx");
        let keys: Vec<String> = f.bindings.iter().map(|b| b.contract.canonical()).collect();
        assert!(keys.contains(&res("call.http.get.0")), "{keys:?}");
        assert!(keys.contains(&res("x")) && keys.contains(&res("y")));
        let call_owner = f
            .bindings
            .iter()
            .find(|b| b.contract.canonical() == res("call.http.get.0"))
            .unwrap()
            .symbol;
        assert_eq!(call_owner, f.symbols[0].name.id());
        // Without `call=` there is no such binding for g.
        assert_eq!(keys.iter().filter(|k| k.contains("call.")).count(), 1);
    }

    #[test]
    fn guest_context_change_reextracts_hosts() {
        let t = env(&[("a.tx", "sym fn f\nlit f use x\n"), ("tguest.ctx", "a")]);
        let e = t.open("1");
        full_index(&e);
        assert_eq!(
            shard(&e, "a.tx").bindings[0].contract.canonical(),
            res("a.x")
        );
        let before = t.n();
        std::fs::write(t.work.join("tguest.ctx"), "b").unwrap();
        e.inbox().append(&file_event("tguest.ctx")).unwrap();
        drained_graph(&e);
        assert_eq!(t.n(), before + 1, "re-extracted once");
        assert_eq!(
            shard(&e, "a.tx").bindings[0].contract.canonical(),
            res("b.x")
        );
    }

    #[test]
    fn unrelated_guest_context_change_skips_hosts() {
        let t = env(&[("a.tx", "sym fn f\nlit f use x\n"), ("tguest.ctx", "a")]);
        let e = t.open("1");
        full_index(&e);
        let before = t.n();
        let bytes = shard_bytes(&e, "a.tx");
        std::fs::write(t.work.join("tguest.ctx"), "a\nsecond line").unwrap();
        e.inbox().append(&file_event("tguest.ctx")).unwrap();
        drained_graph(&e);
        assert_eq!(t.n(), before, "host not re-extracted");
        assert_eq!(shard_bytes(&e, "a.tx"), bytes);
    }
}
