//! Contract nodes made and removed by the stitcher (FEAT-002 R-2, R-5).

mod support;

use camino::Utf8PathBuf;
use katalogr_engine::Stitcher;
use katalogr_model::{
    ArtifactName, ContractName, ExtractInput, Extractor, Fragment, Id, LinkKind, Name, RelPath,
};
use katalogr_store::{Cache, Kindex, LinkKey, ProjectConfig, ProjectName, Reader, RepoConfig};
use std::time::{Duration, Instant};
use support::TestExtractor;

struct Env {
    _tmp: tempfile::TempDir,
    cache: Cache,
    stitcher: Stitcher,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
    let dir = Kindex::init(&root)
        .unwrap()
        .create_project(
            &ProjectName::new("p").unwrap(),
            ProjectConfig {
                repos: vec![RepoConfig::new("r"), RepoConfig::new("s")],
                ..Default::default()
            },
        )
        .unwrap();
    let stitcher = Stitcher::new();
    let cache =
        Cache::open_or_rebuild(&dir, &stitcher, Instant::now() + Duration::from_secs(5)).unwrap();
    Env {
        _tmp: tmp,
        cache,
        stitcher,
    }
}

fn extract(repo: &str, path: &str, text: &str) -> Fragment {
    let path = RelPath::new(path).unwrap();
    TestExtractor
        .extract(&ExtractInput {
            repo,
            path: &path,
            revision: "ce013625030ba8dba906f756967f9e9ca394464a",
            content: text.as_bytes(),
            context: &[],
        })
        .unwrap()
}

impl Env {
    fn put(&self, repo: &str, path: &str, text: &str) {
        let mut tx = self.cache.write().unwrap();
        self.stitcher
            .replace_fragment(&mut tx, extract(repo, path, text))
            .unwrap();
        tx.commit().unwrap();
    }

    fn remove(&self, repo: &str, path: &str) {
        let mut tx = self.cache.write().unwrap();
        let name = ArtifactName::new(repo, path).unwrap();
        self.stitcher.remove_artifact(&mut tx, &name).unwrap();
        tx.commit().unwrap();
    }
}

fn contract_id(kind: &str, key: &str) -> Id {
    ContractName {
        kind: kind.into(),
        key: key.into(),
    }
    .id()
}

fn sym(repo: &str, path: &str, name: &str) -> Id {
    extract(repo, path, &format!("sym fn {name}\n")).symbols[0]
        .name
        .id()
}

fn link(from: Id, kind: &str, to: Id) -> LinkKey {
    LinkKey {
        from,
        kind: LinkKind(kind.into()),
        to,
    }
}

mod contract {
    use super::*;

    #[test]
    fn matched_across_repos() {
        let e = env();
        e.put(
            "r",
            "api.tx",
            "sym fn serve\nbind serve provides endpoint:get/orders\n",
        );
        e.put(
            "s",
            "client.tx",
            "sym fn fetch\nbind fetch consumes endpoint:get/orders\n",
        );
        let c = contract_id("endpoint", "get/orders");
        let snap = e.cache.read().unwrap();
        let row = snap.contract(c).unwrap().expect("one contract node");
        assert_eq!(row.name.canonical(), "contract:endpoint:get%2Forders");
        assert_eq!((row.providers, row.consumers, row.declarers), (1, 1, 0));
        assert_eq!(snap.contracts().unwrap().len(), 1);

        let provider = sym("r", "api.tx", "serve");
        let consumer = sym("s", "client.tx", "fetch");
        let mut incoming = snap.links_in(c).unwrap();
        incoming.sort();
        let mut want = vec![link(provider, "provides", c), link(consumer, "consumes", c)];
        want.sort();
        assert_eq!(incoming, want);
        assert!(snap.link(&link(provider, "provides", c)).unwrap().is_some());

        // A declaration is counted apart from both ends.
        drop(snap);
        e.put(
            "s",
            "spec.tx",
            "sym fn op\nbind op declares endpoint:get/orders\n",
        );
        let row = e.cache.read().unwrap().contract(c).unwrap().unwrap();
        assert_eq!((row.providers, row.consumers, row.declarers), (1, 1, 1));
    }

    #[test]
    fn node_removed_with_last_binding() {
        let e = env();
        let c = contract_id("channel", "orders");
        e.put(
            "r",
            "a.tx",
            "sym fn pub\nbind pub provides channel:orders\n",
        );
        e.put(
            "r",
            "b.tx",
            "sym fn sub\nbind sub consumes channel:orders\n",
        );
        assert!(e.cache.read().unwrap().contract(c).unwrap().is_some());

        // Re-extracting a.tx without its binding withdraws one end; the node stays.
        e.put("r", "a.tx", "sym fn pub\n");
        let snap = e.cache.read().unwrap();
        let row = snap.contract(c).unwrap().expect("still bound by b.tx");
        assert_eq!((row.providers, row.consumers), (0, 1));
        assert_eq!(
            snap.bindings_of(ArtifactName::new("r", "a.tx").unwrap().id())
                .unwrap(),
            vec![]
        );
        assert_eq!(snap.links_in(c).unwrap().len(), 1);
        drop(snap);

        // Removing the artifact that holds the last binding removes the node and its links.
        e.remove("r", "b.tx");
        let snap = e.cache.read().unwrap();
        assert!(snap.contract(c).unwrap().is_none());
        assert!(snap.contracts().unwrap().is_empty());
        assert!(snap.links_in(c).unwrap().is_empty());
        assert!(
            snap.bindings_of(ArtifactName::new("r", "b.tx").unwrap().id())
                .unwrap()
                .is_empty()
        );
        drop(snap);

        // A binding re-added creates the node again; changing it to another contract moves it.
        e.put(
            "r",
            "a.tx",
            "sym fn pub\nbind pub provides channel:orders\n",
        );
        assert!(e.cache.read().unwrap().contract(c).unwrap().is_some());
        e.put(
            "r",
            "a.tx",
            "sym fn pub\nbind pub provides channel:refunds\n",
        );
        let snap = e.cache.read().unwrap();
        assert!(snap.contract(c).unwrap().is_none());
        let moved = snap
            .contract(contract_id("channel", "refunds"))
            .unwrap()
            .unwrap();
        assert_eq!(moved.providers, 1);
        assert_eq!(snap.contracts().unwrap().len(), 1);
    }
}
