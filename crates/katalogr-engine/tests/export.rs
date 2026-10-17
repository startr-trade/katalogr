//! FEAT-001 R-7. The FEAT-023 fixture corpus does not exist yet, so this test indexes a small
//! inline multi-file repository.
mod support;

use camino::Utf8PathBuf;
use katalogr_model::RelPath;
use katalogr_store::{Kindex, ProjectName};
use support::{fresh_index, open_engine};

mod graph {
    use super::*;

    fn shard_bytes(kx: &Utf8PathBuf) -> Vec<(String, Vec<u8>)> {
        let dir = Kindex::open(kx)
            .unwrap()
            .project(&ProjectName::new("p").unwrap())
            .unwrap();
        dir.list_shards()
            .unwrap()
            .into_iter()
            .map(|(repo, path): (String, RelPath)| {
                let bytes = std::fs::read(dir.shard_path(&repo, &path)).unwrap();
                (format!("{repo}:{path}"), bytes)
            })
            .collect()
    }

    #[test]
    fn deterministic_output() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        std::fs::write(work.join("a.tx"), "sym fn A\nexport k:a A\n").unwrap();
        std::fs::write(
            work.join("b.tx"),
            "sym fn B\nsym struct S\nref B calls k:a\nlink S contains B\n",
        )
        .unwrap();
        std::fs::write(work.join("sub/c.tx"), "sym fn C\nref C calls k:a\n").unwrap();

        let (kx1, g1) = fresh_index(&root, "kx1", &work);
        let (kx2, g2) = fresh_index(&root, "kx2", &work);
        assert_eq!(g1, g2);
        assert!(g1.ends_with('\n'));
        assert!(g1.contains(r#""node":"artifact""#));
        assert!(g1.contains(r#""link":true"#));
        assert!(g1.contains("r:a.tx#fn:A"), "canonical names, not ids");
        let s1 = shard_bytes(&kx1);
        assert_eq!(s1.len(), 3);
        assert_eq!(s1, shard_bytes(&kx2), "shard files are byte-identical");

        // Re-opening and re-reading gives the same text.
        let e = open_engine(&kx1, &work);
        assert_eq!(support::drained_graph(&e), g1);
    }
}
