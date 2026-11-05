//! FEAT-094 R-4 and R-12: planning compares a computed revision with the graph's file revision.
mod support;

use camino::Utf8PathBuf;
use katalogr_engine::registry::ExtensionRegistry;
use katalogr_engine::{Engine, NewEvent};
use katalogr_model::{ExtractError, ExtractInput, Extractor, Fragment, RelPath};
use katalogr_store::{Kindex, ProjectName, Reader};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use support::{TestExtractor, drained_graph, init_project};

/// The test format with `ctx.tx` as its context file, counting extractions.
struct Counting(Arc<AtomicUsize>);

impl Extractor for Counting {
    fn id(&self) -> &'static str {
        "plan-test"
    }

    fn revision_salt(&self) -> &'static str {
        "test-1"
    }

    fn handles(&self, path: &RelPath) -> bool {
        TestExtractor.handles(path)
    }

    fn context_files(&self) -> &'static [&'static str] {
        &["ctx.tx"]
    }

    fn build_context(&self, files: &[(RelPath, Vec<u8>)]) -> Result<Vec<u8>, ExtractError> {
        Ok(files.iter().flat_map(|(_, b)| b.clone()).collect())
    }

    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let content: &[u8] = if input.path.as_str() == "ctx.tx" {
            b""
        } else {
            input.content
        };
        let mut f = TestExtractor.extract(&ExtractInput { content, ..*input })?;
        f.extension = self.id().to_string();
        Ok(f)
    }
}

struct Fx {
    _tmp: tempfile::TempDir,
    work: Utf8PathBuf,
    e: Engine,
    count: Arc<AtomicUsize>,
}

fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let (kx, work) = (root.join("kx"), root.join("work"));
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("a.tx"), "sym fn A\n").unwrap();
    std::fs::write(work.join("b.tx"), "sym fn B\n").unwrap();
    std::fs::write(work.join("ctx.tx"), "one").unwrap();
    init_project(&kx);
    let dir = Kindex::open(&kx)
        .unwrap()
        .project(&ProjectName::new("p").unwrap())
        .unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let mut reg = ExtensionRegistry::new();
    reg.register(Box::new(Counting(count.clone())));
    let roots = BTreeMap::from([("r".to_string(), work.clone())]);
    let e = Engine::open(dir, reg, roots, Instant::now() + Duration::from_secs(10)).unwrap();
    Fx {
        _tmp: tmp,
        work,
        e,
        count,
    }
}

impl Fx {
    fn rescan(&self) {
        self.e.inbox().append(&NewEvent::rescan("r")).unwrap();
        drained_graph(&self.e);
    }

    fn extractions(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    fn paths(&self) -> Vec<String> {
        let r = self.e.cache().read().unwrap();
        let mut v: Vec<String> = r
            .file_revisions()
            .unwrap()
            .into_iter()
            .map(|((_, p), _)| p.as_str().to_string())
            .collect();
        v.sort();
        v
    }
}

mod plan {
    use super::*;

    /// R-4.
    #[test]
    fn unchanged_revision_not_extracted() {
        let f = fixture();
        f.rescan();
        let first = f.extractions();
        assert_eq!(first, 3);
        f.rescan();
        assert_eq!(f.extractions(), first, "an unchanged file is not extracted");
        std::fs::write(f.work.join("a.tx"), "sym fn A\nsym fn A2\n").unwrap();
        f.rescan();
        assert_eq!(f.extractions(), first + 1, "only the changed file");
    }

    /// R-4. The extension's context is part of the revision.
    #[test]
    fn changed_context_extracts_again() {
        let f = fixture();
        f.rescan();
        let first = f.extractions();
        std::fs::write(f.work.join("ctx.tx"), "two").unwrap();
        f.rescan();
        assert!(
            f.extractions() >= first + 2,
            "both source files are extracted against the new context"
        );
    }

    /// R-12.
    #[test]
    fn vanished_file_removed_on_rescan() {
        let f = fixture();
        f.rescan();
        assert!(f.paths().contains(&"b.tx".to_string()));
        std::fs::remove_file(f.work.join("b.tx")).unwrap();
        f.rescan();
        assert!(!f.paths().contains(&"b.tx".to_string()));
        let r = f.e.cache().read().unwrap();
        assert!(
            r.artifacts()
                .unwrap()
                .iter()
                .all(|(_, a)| a.name.path.as_str() != "b.tx")
        );
    }

    /// R-12.
    #[test]
    fn excluded_file_removed_on_rescan() {
        let mut f = fixture();
        f.rescan();
        assert!(f.paths().contains(&"b.tx".to_string()));
        let (include, mut exclude) = f.e.repo_filters("r");
        exclude.push("b.tx".to_string());
        f.e.set_repo_filters("r", include, exclude);
        f.rescan();
        assert!(!f.paths().contains(&"b.tx".to_string()));
        assert!(f.paths().contains(&"a.tx".to_string()));
        let r = f.e.cache().read().unwrap();
        assert!(
            r.artifacts()
                .unwrap()
                .iter()
                .all(|(_, a)| a.name.path.as_str() != "b.tx")
        );
    }
}
