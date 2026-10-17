mod support;

use camino::Utf8PathBuf;
use katalogr_engine::{StitchDelta, Stitcher};
use katalogr_model::{
    Confidence, ExportKey, ExtractInput, Extractor, Fragment, Id, LinkKind, Name, RelPath,
};
use katalogr_store::{
    Cache, Kindex, LinkKey, ProjectConfig, ProjectDir, ProjectName, Reader, RefId, RepoConfig,
};
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
    let kx = Kindex::init(&root).unwrap();
    let dir: ProjectDir = kx
        .create_project(
            &ProjectName::new("p").unwrap(),
            ProjectConfig {
                repos: vec![RepoConfig::new("r")],
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

fn extract(path: &str, text: &str) -> Fragment {
    let path = RelPath::new(path).unwrap();
    TestExtractor
        .extract(&ExtractInput {
            repo: "r",
            path: &path,
            revision: "ce013625030ba8dba906f756967f9e9ca394464a",
            content: text.as_bytes(),
            context: &[],
        })
        .unwrap()
}

impl Env {
    fn put(&self, path: &str, text: &str) -> StitchDelta {
        let mut tx = self.cache.write().unwrap();
        let d = self
            .stitcher
            .replace_fragment(&mut tx, extract(path, text))
            .unwrap();
        tx.commit().unwrap();
        d
    }

    fn put_fragment(&self, f: Fragment) -> StitchDelta {
        let mut tx = self.cache.write().unwrap();
        let d = self.stitcher.replace_fragment(&mut tx, f).unwrap();
        tx.commit().unwrap();
        d
    }

    fn remove(&self, path: &str) -> StitchDelta {
        let mut tx = self.cache.write().unwrap();
        let name = katalogr_model::ArtifactName::new("r", path).unwrap();
        let d = self.stitcher.remove_artifact(&mut tx, &name).unwrap();
        tx.commit().unwrap();
        d
    }
}

fn art(path: &str) -> Id {
    katalogr_model::ArtifactName::new("r", path).unwrap().id()
}

fn sym(path: &str, name: &str) -> Id {
    extract(path, &format!("sym fn {name}\n")).symbols[0]
        .name
        .id()
}

fn lk(from: Id, kind: &str, to: Id) -> LinkKey {
    LinkKey {
        from,
        kind: LinkKind(kind.into()),
        to,
    }
}

mod graph {
    use super::*;

    #[test]
    fn incremental_reextract_single_file() {
        let e = env();
        let a = "sym fn A\nexport ns:k A\n";
        e.put("a.tx", a);
        e.put("b.tx", "sym fn B\nref B calls ns:k\n");
        e.put("c.tx", "sym fn C\nexport ns:z C\nref C calls ns:z\n");

        let (a_sym, b_sym, c_sym) = (sym("a.tx", "A"), sym("b.tx", "B"), sym("c.tx", "C"));
        let rx = e.cache.read().unwrap();
        assert_eq!(
            rx.links_out(b_sym, None).unwrap()[0].0,
            lk(b_sym, "calls", a_sym)
        );
        assert_eq!(
            rx.links_out(c_sym, None).unwrap()[0].0,
            lk(c_sym, "calls", c_sym)
        );
        drop(rx);

        // Re-extracting an unchanged file re-resolves only that file's own refs (none here).
        let same = e.put("a.tx", a);
        assert_eq!(same, StitchDelta::default());

        // Change the one export: only the ref that names that key is re-resolved.
        let delta = e.put("a.tx", "sym fn A\nexport ns:k2 A\n");
        assert_eq!(
            delta.refs_resolved,
            vec![RefId {
                artifact: art("b.tx"),
                index: 0
            }]
        );
        assert_eq!(delta.links_removed, vec![lk(b_sym, "calls", a_sym)]);
        assert!(delta.links_added.is_empty());

        let rx = e.cache.read().unwrap();
        assert!(rx.links_out(b_sym, None).unwrap().is_empty());
        assert_eq!(rx.unresolved_refs(None).unwrap().len(), 1);
        assert_eq!(
            rx.links_out(c_sym, None).unwrap()[0].0,
            lk(c_sym, "calls", c_sym)
        );
        drop(rx);

        // The new key is picked up by the reverse index without touching c.
        let delta = e.put("a.tx", "sym fn A\nexport ns:k A\n");
        assert_eq!(
            delta.refs_resolved,
            vec![RefId {
                artifact: art("b.tx"),
                index: 0
            }]
        );
        assert_eq!(delta.links_added, vec![lk(b_sym, "calls", a_sym)]);
        assert!(
            e.cache
                .read()
                .unwrap()
                .unresolved_refs(None)
                .unwrap()
                .is_empty()
        );
    }
}

mod stitch {
    use super::*;

    #[test]
    fn ambiguous_ref_links_all_inferred() {
        let e = env();
        e.put("x.tx", "sym fn X\nexport ns:k X\n");
        e.put("y.tx", "sym fn Y\nexport ns:k Y\n");
        e.put("z.tx", "sym fn Z\nref Z calls ns:k\n");
        let (x, y, z) = (sym("x.tx", "X"), sym("y.tx", "Y"), sym("z.tx", "Z"));

        let rx = e.cache.read().unwrap();
        let links = rx.links_out(z, None).unwrap();
        let keys: Vec<_> = links.iter().map(|(k, _)| k.clone()).collect();
        let mut want = vec![lk(z, "calls", x), lk(z, "calls", y)];
        want.sort();
        assert_eq!(keys, want);
        for (_, row) in &links {
            assert_eq!(row.entries[0].confidence, Confidence::Inferred);
        }
        assert_eq!(rx.links_in(x).unwrap(), vec![lk(z, "calls", x)]);
        drop(rx);

        // Dropping one exporter leaves one target, and the ref's own confidence returns.
        e.remove("y.tx");
        let rx = e.cache.read().unwrap();
        let links = rx.links_out(z, None).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].0, lk(z, "calls", x));
        assert_eq!(links[0].1.entries[0].confidence, Confidence::Extracted);
    }

    #[test]
    fn unresolved_ref_recorded() {
        let e = env();
        e.put("z.tx", "sym fn Z\nref Z calls ns:nope\n");
        let z = sym("z.tx", "Z");
        let me = RefId {
            artifact: art("z.tx"),
            index: 0,
        };
        let rx = e.cache.read().unwrap();
        assert!(rx.links_out(z, None).unwrap().is_empty());
        let un = rx.unresolved_refs(None).unwrap();
        assert_eq!(un.len(), 1);
        assert_eq!(un[0].0, me);
        assert_eq!(rx.unresolved(me).unwrap().unwrap().from, z);
        drop(rx);

        e.put("w.tx", "sym fn W\nexport ns:nope W\n");
        let rx = e.cache.read().unwrap();
        assert!(rx.unresolved_refs(None).unwrap().is_empty());
        assert_eq!(rx.links_out(z, None).unwrap().len(), 1);
        drop(rx);

        e.remove("w.tx");
        let rx = e.cache.read().unwrap();
        assert!(rx.links_out(z, None).unwrap().is_empty());
        assert_eq!(rx.unresolved_refs(None).unwrap().len(), 1);
    }

    #[test]
    fn first_candidate_with_exports_wins() {
        let e = env();
        e.put("p.tx", "sym fn P\nexport ns:specific P\n");
        e.put("q.tx", "sym fn Q\nexport ns:general Q\n");
        let mut z = extract("z.tx", "sym fn Z\nref Z calls ns:none\n");
        let key = |k: &str| ExportKey {
            namespace: "ns".into(),
            key: k.into(),
        };
        z.refs[0].candidates = vec![key("missing"), key("specific"), key("general")];
        e.put_fragment(z);
        let z = sym("z.tx", "Z");
        let rx = e.cache.read().unwrap();
        let links = rx.links_out(z, None).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].0.to, sym("p.tx", "P"));
    }
}
