//! FEAT-094 R-14: audit records and verification through the facade. Needs the `.tx` test
//! extractor: `cargo test -p katalogr-dist --features test-extractor`.
#![cfg(feature = "test-extractor")]

mod audit {
    use camino::Utf8PathBuf;
    use katalogr_dist::{AuditOutcome, AuditRecord, Katalogr, Options, Overrides};
    use katalogr_dist::{kindex_init, project_init};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    struct Setup {
        _tmp: tempfile::TempDir,
        kx: PathBuf,
        repo: PathBuf,
    }

    fn git(root: &Path, args: &[&str]) {
        let st = Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("run git");
        assert!(st.success(), "git {args:?} failed");
    }

    fn setup() -> Setup {
        let tmp = tempfile::tempdir().unwrap();
        let kx = tmp.path().join("kx");
        let repo = tmp.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("s.tx"), "sym fn alpha\nsym fn beta\n").unwrap();
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), repo.clone())], true).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c1"]);
        let s = Setup {
            _tmp: tmp,
            kx,
            repo,
        };
        katalogr(&s)
            .index(None, true, None, &Options::default())
            .unwrap();
        s
    }

    fn katalogr(s: &Setup) -> Katalogr {
        Katalogr::resolve(
            &s.repo,
            &Overrides {
                kindex: Some(s.kx.display().to_string()),
                project: None,
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn opts() -> Options {
        Options {
            timeout: Some(20.0),
            ..Default::default()
        }
    }

    /// The path of the record file `record` was written to.
    fn record_file(s: &Setup, r: &AuditRecord) -> Utf8PathBuf {
        let kx = Utf8PathBuf::from_path_buf(s.kx.clone()).unwrap();
        kx.join("projects/demo/audit").join(r.file_name())
    }

    #[test]
    fn record_is_sorted_text() {
        let s = setup();
        let r = katalogr(&s).audit_record(&opts()).unwrap();
        let file = record_file(&s, &r);
        let text = std::fs::read_to_string(&file).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(text, katalogr_store::shard::to_sorted_json(&value));
        assert!(text.ends_with("}\n") && !text.contains('\r'));
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["facts_version", "graph_digest", "positions"]);
        let name = file.file_name().unwrap();
        assert_eq!(name.len(), 12 + ".json".len());
        assert!(name[..12].bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(r.graph_digest.len(), 64);
        assert_eq!(r.positions.len(), 1);
        assert_eq!(r.positions[0].repo, "app");
        assert_eq!(r.positions[0].commit.as_ref().map(String::len), Some(40));
        assert_eq!(r.positions[0].tree, None);
        // A second record at the same positions replaces the file.
        let again = katalogr(&s).audit_record(&opts()).unwrap();
        assert_eq!(again, r);
        let dir = file.parent().unwrap();
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 1);
    }

    #[test]
    fn record_has_no_absolute_paths() {
        let s = setup();
        let r = katalogr(&s).audit_record(&opts()).unwrap();
        let text = std::fs::read_to_string(record_file(&s, &r)).unwrap();
        assert!(!text.contains(s.repo.to_str().unwrap()), "{text}");
        assert!(!text.contains(s.kx.to_str().unwrap()), "{text}");
        assert!(!text.contains('/'), "{text}");
    }

    #[test]
    fn verify_match() {
        let s = setup();
        let k = katalogr(&s);
        k.audit_record(&opts()).unwrap();
        assert_eq!(k.audit_verify(None, &opts()).unwrap(), AuditOutcome::Match);
        // Reindexing from nothing at the same position gives the same digest.
        std::fs::remove_file(s.kx.join("projects/demo/.cache/index.redb")).unwrap();
        let k = katalogr(&s);
        k.index(None, true, None, &opts()).unwrap();
        assert_eq!(k.audit_verify(None, &opts()).unwrap(), AuditOutcome::Match);
    }

    #[test]
    fn verify_mismatch_exit_1() {
        let s = setup();
        let k = katalogr(&s);
        let r = k.audit_record(&opts()).unwrap();
        let file = record_file(&s, &r);
        let wrong = AuditRecord {
            graph_digest: "0".repeat(64),
            ..r.clone()
        };
        std::fs::write(&file, serde_json::to_string(&wrong).unwrap()).unwrap();
        assert_eq!(
            k.audit_verify(Some(&file), &opts()).unwrap(),
            AuditOutcome::Mismatch {
                expected: "0".repeat(64),
                found: r.graph_digest
            }
        );
    }

    #[test]
    fn verify_other_facts_not_comparable() {
        let s = setup();
        let k = katalogr(&s);
        let r = k.audit_record(&opts()).unwrap();
        let file = record_file(&s, &r);
        let other = AuditRecord {
            facts_version: "f".repeat(64),
            graph_digest: "0".repeat(64),
            ..r.clone()
        };
        std::fs::write(&file, serde_json::to_string(&other).unwrap()).unwrap();
        assert_eq!(
            k.audit_verify(None, &opts()).unwrap(),
            AuditOutcome::NotComparable {
                record_facts: "f".repeat(64),
                current_facts: r.facts_version
            }
        );
    }

    #[test]
    fn verify_not_at_position_names_command() {
        let s = setup();
        let k = katalogr(&s);
        let r = k.audit_record(&opts()).unwrap();
        let file = record_file(&s, &r);
        git(&s.repo, &["commit", "-q", "--allow-empty", "-m", "c2"]);
        let k = katalogr(&s);
        match k.audit_verify(Some(&file), &opts()).unwrap() {
            AuditOutcome::NotAtPosition {
                want,
                have,
                command,
            } => {
                assert_eq!(want, r.positions);
                assert_ne!(have, r.positions);
                let rev = r.positions[0].commit.clone().unwrap();
                assert_eq!(command, format!("katalogr context add --commit {rev}"));
            }
            other => panic!("{other:?}"),
        }
        // With no record stored for the current positions the lookup is a refusal, status 4.
        let err = k.audit_verify(None, &opts()).unwrap_err();
        assert_eq!(err.refused_exit(), Some(4), "{err}");
    }
}
