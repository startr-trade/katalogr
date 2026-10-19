//! FEAT-005 T1: extension context and rescan on change.
mod support;

use camino::Utf8PathBuf;
use katalogr_engine::registry::ExtensionRegistry;
use katalogr_engine::{Engine, scan};
use katalogr_model::{ExtractError, ExtractInput, Extractor, Fact, Fragment, RelPath};
use katalogr_store::{Kindex, ProjectName};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use support::{SALT, SaltedExtractor, TestExtractor, drained_graph, file_event, init_project};

/// The test format, with `ctx.tx` as the context file. The context is the concatenated text of
/// every `ctx.tx` in the repo, and each artifact records it in the fact `ctx`.
struct CtxExtractor;

impl Extractor for CtxExtractor {
    fn id(&self) -> &'static str {
        "ctx-test"
    }

    fn handles(&self, path: &RelPath) -> bool {
        TestExtractor.handles(path)
    }

    fn context_files(&self) -> &'static [&'static str] {
        &["ctx.tx"]
    }

    fn build_context(&self, files: &[(RelPath, Vec<u8>)]) -> Result<Vec<u8>, ExtractError> {
        let mut out = Vec::new();
        for (p, bytes) in files {
            out.extend_from_slice(p.as_str().as_bytes());
            out.push(b'=');
            out.extend_from_slice(bytes);
        }
        Ok(out)
    }

    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        // `ctx.tx` itself holds free text, not directives.
        let content: &[u8] = if input.path.as_str().ends_with("ctx.tx") {
            b""
        } else {
            input.content
        };
        let mut f = TestExtractor.extract(&ExtractInput { content, ..*input })?;
        f.extension = self.id().to_string();
        f.artifact.facts.insert(
            "ctx".to_string(),
            Fact::Str(String::from_utf8_lossy(input.context).into_owned()),
        );
        Ok(f)
    }
}

fn open(kx: &camino::Utf8Path, work: &camino::Utf8Path) -> Engine {
    let dir = Kindex::open(kx)
        .unwrap()
        .project(&ProjectName::new("p").unwrap())
        .unwrap();
    let mut reg = ExtensionRegistry::new();
    reg.register(Box::new(CtxExtractor));
    let roots = BTreeMap::from([("r".to_string(), work.to_path_buf())]);
    Engine::open(dir, reg, roots, Instant::now() + Duration::from_secs(10)).unwrap()
}

mod context {
    use super::*;

    #[test]
    fn rescan_on_context_change() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        std::fs::write(work.join("a.tx"), "sym fn A\n").unwrap();
        std::fs::write(work.join("sub/b.tx"), "sym fn B\n").unwrap();
        std::fs::write(work.join("ctx.tx"), "one").unwrap();
        let kx = root.join("kx");
        init_project(&kx);
        let e = open(&kx, &work);

        e.inbox()
            .append(&katalogr_engine::NewEvent::rescan("r"))
            .unwrap();
        let g1 = drained_graph(&e);
        assert!(g1.contains(r#""ctx":{"Str":"ctx.tx=one"}"#), "{g1}");
        assert_eq!(
            g1.matches(r#""ctx":{"Str":"ctx.tx=one"}"#).count(),
            3,
            "{g1}"
        );

        // Only the context file changes; a.tx and sub/b.tx are untouched on disk.
        std::fs::write(work.join("ctx.tx"), "two").unwrap();
        e.inbox().append(&file_event("ctx.tx")).unwrap();
        let g2 = drained_graph(&e);
        assert!(!g2.contains("ctx.tx=one"), "{g2}");
        assert_eq!(
            g2.matches(r#""ctx":{"Str":"ctx.tx=two"}"#).count(),
            3,
            "{g2}"
        );

        // A nested context file joins the context (match on the last path component).
        std::fs::write(work.join("sub/ctx.tx"), "x").unwrap();
        e.inbox().append(&file_event("sub/ctx.tx")).unwrap();
        let g3 = drained_graph(&e);
        let want = r#""ctx":{"Str":"ctx.tx=twosub/ctx.tx=x"}"#;
        assert_eq!(g3.matches(want).count(), 4, "{g3}");

        // A touched but unchanged context file queues nothing: the graph stays the same.
        e.inbox().append(&file_event("ctx.tx")).unwrap();
        assert_eq!(drained_graph(&e), g3);

        // Removing every context file empties the context.
        std::fs::remove_file(work.join("ctx.tx")).unwrap();
        std::fs::remove_file(work.join("sub/ctx.tx")).unwrap();
        e.inbox()
            .append(&katalogr_engine::NewEvent::file(
                "r",
                &RelPath::new("ctx.tx").unwrap(),
                katalogr_engine::EventKind::Deleted,
            ))
            .unwrap();
        e.inbox()
            .append(&katalogr_engine::NewEvent::file(
                "r",
                &RelPath::new("sub/ctx.tx").unwrap(),
                katalogr_engine::EventKind::Deleted,
            ))
            .unwrap();
        let g4 = drained_graph(&e);
        assert_eq!(g4.matches(r#""ctx":{"Str":""}"#).count(), 2, "{g4}");
    }

    #[test]
    fn list_files_applies_globs() {
        let tmp = tempfile::tempdir().unwrap();
        let w = tmp.path();
        std::fs::create_dir_all(w.join(".git")).unwrap();
        std::fs::create_dir_all(w.join("target/x")).unwrap();
        std::fs::create_dir_all(w.join("src")).unwrap();
        std::fs::write(w.join(".git/config"), "").unwrap();
        std::fs::write(w.join("target/x/y.rs"), "").unwrap();
        std::fs::write(w.join("src/b.rs"), "").unwrap();
        std::fs::write(w.join("src/a.rs"), "").unwrap();
        std::fs::write(w.join("Cargo.toml"), "").unwrap();
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let all = scan::list_files(w, &s(&["**"]), &s(&[".git/**", "target/**"])).unwrap();
        let all: Vec<_> = all.iter().map(|p| p.as_str()).collect();
        assert_eq!(all, ["Cargo.toml", "src/a.rs", "src/b.rs"]);
        let rs = scan::list_files(w, &s(&["**/*.rs"]), &s(&["src/b.rs"])).unwrap();
        let rs: Vec<_> = rs.iter().map(|p| p.as_str()).collect();
        assert_eq!(rs, ["src/a.rs", "target/x/y.rs"]);
    }

    #[test]
    fn extractor_salt_forces_reextract() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("a.salt"), "one\ntwo\n").unwrap();
        let kx = root.join("kx");
        init_project(&kx);
        let dir = Kindex::open(&kx)
            .unwrap()
            .project(&ProjectName::new("p").unwrap())
            .unwrap();
        let mut reg = ExtensionRegistry::new();
        reg.register(Box::new(SaltedExtractor));
        let roots = BTreeMap::from([("r".to_string(), work.clone())]);
        let e = Engine::open(dir, reg, roots, Instant::now() + Duration::from_secs(10)).unwrap();
        let revision = |e: &Engine| {
            let shard = e
                .dir()
                .read_shard("r", &RelPath::new("a.salt").unwrap())
                .unwrap()
                .expect("shard");
            shard.provenance.revision
        };

        SALT.store(0, Ordering::SeqCst);
        e.inbox()
            .append(&katalogr_engine::NewEvent::rescan("r"))
            .unwrap();
        drained_graph(&e);
        let r0 = revision(&e);
        let shard_file = e.dir().shard_path("r", &RelPath::new("a.salt").unwrap());
        let m0 = std::fs::metadata(&shard_file).unwrap().modified().unwrap();

        // Nothing changed: the rescan skips the shard.
        std::thread::sleep(Duration::from_millis(20));
        e.inbox()
            .append(&katalogr_engine::NewEvent::rescan("r"))
            .unwrap();
        drained_graph(&e);
        assert_eq!(revision(&e), r0);
        assert_eq!(
            std::fs::metadata(&shard_file).unwrap().modified().unwrap(),
            m0
        );

        // The extractor bumps its salt: a full rescan rewrites the shard under a new revision.
        SALT.store(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(20));
        e.inbox()
            .append(&katalogr_engine::NewEvent::rescan("r"))
            .unwrap();
        drained_graph(&e);
        let r1 = revision(&e);
        SALT.store(0, Ordering::SeqCst);
        assert_ne!(r1, r0, "the salt is part of the revision");
        assert!(std::fs::metadata(&shard_file).unwrap().modified().unwrap() > m0);
    }
}
