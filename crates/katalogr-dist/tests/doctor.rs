//! FEAT-064 T8: doctor resolves the project and turns resolution failures into findings.

mod doctor {
    use katalogr_dist::doctor::{CheckStatus, DoctorOptions, Report, doctor};
    use katalogr_dist::{Overrides, kindex_init, project_init};
    use std::path::Path;

    fn opts() -> DoctorOptions {
        DoctorOptions {
            skip_git: true,
            ..Default::default()
        }
    }

    fn codes(r: &Report) -> Vec<&str> {
        r.findings.iter().map(|f| f.code.as_str()).collect()
    }

    /// A kindex with project `demo` and one marked repo `app`.
    fn setup(tmp: &Path) -> std::path::PathBuf {
        let repo = tmp.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        let kx = kindex_init(&tmp.join("kx")).unwrap();
        project_init(&kx, "demo", vec![("app".to_string(), repo.clone())], true).unwrap();
        repo
    }

    #[test]
    fn marker_unresolved() {
        let tmp = tempfile::tempdir().unwrap();
        let r = doctor(tmp.path(), &Overrides::default(), &opts()).unwrap();
        assert_eq!(codes(&r), vec!["KD-MRK-001"]);
        assert!(r.exit_failing());
        assert_eq!(r.checks.len(), 11);
        for c in &r.checks {
            let want = if c.area == "marker" {
                CheckStatus::Findings
            } else {
                CheckStatus::Skipped
            };
            assert_eq!(c.status, want, "{}", c.area);
        }
        let f = &r.findings[0];
        assert!(
            f.fix
                .as_deref()
                .unwrap()
                .starts_with("katalogr project init")
        );

        // A kindex that is not one, and a project that is not in it.
        let missing = Overrides {
            kindex: Some(tmp.path().join("nope").to_str().unwrap().to_string()),
            project: Some("demo".to_string()),
        };
        let r = doctor(tmp.path(), &missing, &opts()).unwrap();
        assert_eq!(codes(&r), vec!["KD-MRK-002"]);
        let kx = kindex_init(&tmp.path().join("kx")).unwrap();
        let other = Overrides {
            kindex: Some(kx.to_str().unwrap().to_string()),
            project: Some("demo".to_string()),
        };
        let r = doctor(tmp.path(), &other, &opts()).unwrap();
        assert_eq!(codes(&r), vec!["KD-MRK-003"]);
    }

    #[test]
    fn unknown_repo_in_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = setup(tmp.path());
        let r = doctor(&repo, &Overrides::default(), &opts()).unwrap();
        // Nothing is indexed yet, which is only information.
        assert_eq!(codes(&r), vec!["KD-CCH-001"]);
        assert!(r.healthy);
        let marker = repo.join(".katalogr");
        let text = std::fs::read_to_string(&marker).unwrap();
        std::fs::write(&marker, text.replace("repo: app", "repo: ghost")).unwrap();
        let r = doctor(&repo, &Overrides::default(), &opts()).unwrap();
        assert_eq!(codes(&r), vec!["KD-MRK-004", "KD-CCH-001"]);
        assert_eq!(r.findings[0].fix.as_deref(), Some("edit .katalogr"));
    }

    #[test]
    fn newer_format_before_open() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = setup(tmp.path());
        std::fs::write(
            tmp.path().join("kx/projects/demo/format.json"),
            r#"{"min_katalogr":"9.9.9","shard_format":99}"#,
        )
        .unwrap();
        let r = doctor(&repo, &Overrides::default(), &opts()).unwrap();
        assert_eq!(codes(&r), vec!["KD-FMT-002"]);
        assert_eq!(r.versions.kindex_shard_format, Some(99));
    }
}
