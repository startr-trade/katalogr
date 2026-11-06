//! FEAT-094 R-7: the graph is byte-identical for the same files at 1 and 8 threads, in any file
//! order and from a checkout at any path. Needs the `.tx` test extractor:
//! `cargo test -p katalogr-dist --features test-extractor`.
#![cfg(feature = "test-extractor")]

mod determinism {
    use katalogr_dist::{Katalogr, Options, Overrides, kindex_init, project_init};
    use std::path::PathBuf;

    /// A tree with files that share keys and refer to each other across directories.
    fn files() -> Vec<(String, String)> {
        (0..48)
            .map(|i| {
                (
                    format!("d{}/f{i:02}.tx", i % 5),
                    format!(
                        "sym fn F{i}\nsym fn G{i}\nlink F{i} calls G{i}\nexport ns:k{i} F{i}\n\
                         ref G{i} calls ns:k{}\nref G{i} calls ns:k{}\nref G{i} calls ns:missing{i}\n",
                        (i + 1) % 48,
                        (i * 7 + 3) % 48
                    ),
                )
            })
            .collect()
    }

    struct Built {
        _tmp: tempfile::TempDir,
        repo: PathBuf,
        kx: PathBuf,
    }

    /// A project at `tmp/<dir>/work`, the files written in `order`, `threads` configured.
    fn build(dir: &str, threads: usize, order: &[(String, String)]) -> Built {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join(dir);
        let (kx, repo) = (base.join("kx"), base.join("work"));
        std::fs::create_dir_all(&repo).unwrap();
        for (p, t) in order {
            let f = repo.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, t).unwrap();
        }
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), repo.clone())], true).unwrap();
        let config = kx.join("projects/demo/config.yaml");
        let mut text = std::fs::read_to_string(&config).unwrap();
        assert!(!text.contains("settings:"), "{text}");
        text.push_str(&format!("settings:\n  index.threads: {threads}\n"));
        std::fs::write(config, text).unwrap();
        Built {
            _tmp: tmp,
            repo,
            kx,
        }
    }

    fn katalogr(b: &Built) -> Katalogr {
        Katalogr::resolve(
            &b.repo,
            &Overrides {
                kindex: Some(b.kx.display().to_string()),
                project: None,
                ..Default::default()
            },
        )
        .unwrap()
    }

    /// The SHA-256 of the canonical graph, as the audit record takes it.
    fn digest(b: &Built) -> String {
        katalogr(b)
            .audit_record(&Options::default())
            .unwrap()
            .graph_digest
    }

    /// R-7. One thread in the order written; eight threads in reverse order at a deeper path;
    /// four threads from change events sent in reverse order.
    #[test]
    fn threads_order_and_path_identical() {
        let forward = files();
        let mut reverse = forward.clone();
        reverse.reverse();

        let a = build("a", 1, &forward);
        katalogr(&a)
            .index(None, true, None, &Options::default())
            .unwrap();
        let want = digest(&a);
        assert_eq!(want.len(), 64);

        let b = build("deeper/than/the/first/checkout", 8, &reverse);
        katalogr(&b)
            .index(None, true, None, &Options::default())
            .unwrap();
        assert_eq!(digest(&b), want, "8 threads, reverse order, other path");

        let c = build("c", 4, &forward);
        let k = katalogr(&c);
        let paths: Vec<String> = reverse.iter().map(|(p, _)| p.clone()).collect();
        for p in &paths {
            k.notify(std::slice::from_ref(p), false).unwrap();
        }
        k.index(None, false, None, &Options::default()).unwrap();
        assert_eq!(digest(&c), want, "4 threads, change events in reverse order");
    }
}
