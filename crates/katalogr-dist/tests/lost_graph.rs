//! FEAT-094 R-8 to R-10 through the facade: what `Katalogr` does on opening a lost, old or
//! missing graph. Needs the `.tx` test extractor:
//! `cargo test -p katalogr-dist --features test-extractor`.
#![cfg(feature = "test-extractor")]

mod recover {
    use katalogr_dist::{Katalogr, Options, Overrides, kindex_init, project_init};
    use katalogr_store::{CACHE_SCHEMA, Cache, Kindex, ProjectName};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    struct Setup {
        _tmp: tempfile::TempDir,
        kx: PathBuf,
        repo: PathBuf,
    }

    fn setup() -> Setup {
        let tmp = tempfile::tempdir().unwrap();
        let kx = tmp.path().join("kx");
        let repo = tmp.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("s.tx"), "sym fn alpha\nsym fn beta\n").unwrap();
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), repo.clone())], true).unwrap();
        let s = Setup {
            _tmp: tmp,
            kx,
            repo,
        };
        let k = katalogr(&s);
        k.index(None, true, None, &Options::default()).unwrap();
        s
    }

    fn overrides(kx: &Path) -> Overrides {
        Overrides {
            kindex: Some(kx.display().to_string()),
            project: None,
            ..Default::default()
        }
    }

    fn katalogr(s: &Setup) -> Katalogr {
        Katalogr::resolve(&s.repo, &overrides(&s.kx)).unwrap()
    }

    fn project_dir(s: &Setup) -> katalogr_store::ProjectDir {
        Kindex::open(camino::Utf8Path::from_path(&s.kx).unwrap())
            .unwrap()
            .project(&ProjectName::new("demo").unwrap())
            .unwrap()
    }

    fn inbox_files(s: &Setup) -> usize {
        let dir = project_dir(s).cache_dir().join("inbox");
        std::fs::read_dir(dir).map_or(0, |rd| rd.flatten().count())
    }

    /// R-8: another schema is recreated empty, and a rescan of the repo is queued.
    #[test]
    fn schema_mismatch_requeues_through_facade() {
        let s = setup();
        {
            let c = Cache::open(&project_dir(&s), Instant::now() + Duration::from_secs(5)).unwrap();
            let mut tx = c.write().unwrap();
            tx.set_meta("cache_schema", &(CACHE_SCHEMA + 1).to_le_bytes())
                .unwrap();
            tx.commit().unwrap();
        }
        let report = katalogr(&s).status(&Options::default()).unwrap();
        assert!(report.stale || report.pending > 0, "{report:?}");
        assert!(
            !report.warnings.iter().any(|w| w.contains("no index yet")),
            "{:?}",
            report.warnings
        );
    }

    /// R-9: a missing file is created empty, nothing is queued, and the read says so.
    #[test]
    fn missing_cache_warns_and_queues_nothing_through_facade() {
        let s = setup();
        std::fs::remove_file(project_dir(&s).cache_dir().join("index.redb")).unwrap();
        let before = inbox_files(&s);
        let report = katalogr(&s).status(&Options::default()).unwrap();
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w == "no index yet: run katalogr index --full"),
            "{:?}",
            report.warnings
        );
        assert_eq!(inbox_files(&s), before, "nothing was queued");
        assert_eq!(report.pending, 0);
    }

    /// R-10: a repo with no root here is named, once the graph has been replaced.
    #[test]
    fn rootless_repo_reported() {
        let s = setup();
        let config = project_dir(&s).root().join("config.yaml");
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(text.contains("repos:\n"), "{text}");
        let text = text.replacen(
            "repos:\n",
            "repos:\n- name: lib\n  include: ['**']\n  exclude: []\n",
            1,
        );
        std::fs::write(&config, text).unwrap();
        {
            let c = Cache::open(&project_dir(&s), Instant::now() + Duration::from_secs(5)).unwrap();
            let mut tx = c.write().unwrap();
            tx.set_meta("cache_schema", &(CACHE_SCHEMA + 1).to_le_bytes())
                .unwrap();
            tx.commit().unwrap();
        }
        let report = katalogr(&s).status(&Options::default()).unwrap();
        let want = "repo lib has no root here, so it is absent from this index; add it with katalogr project add-root";
        assert!(
            report.warnings.iter().any(|w| w == want),
            "{:?}",
            report.warnings
        );
    }
}
