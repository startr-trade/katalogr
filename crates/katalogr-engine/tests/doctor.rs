//! FEAT-064: `katalogr doctor` checks, over real projects built with the engine.
mod support;

mod doctor {
    use super::support::{TestExtractor, full_index, init_project, open_engine, open_engine_with};
    use camino::{Utf8Path, Utf8PathBuf};
    use fs4::FileExt;
    use katalogr_engine::doctor::{
        CheckStatus, DoctorOptions, Finding, Report, Severity, Thresholds, run, schema,
    };
    use katalogr_engine::queue::Inbox;
    use katalogr_engine::registry::ExtensionRegistry;
    use katalogr_engine::{EventKind, NewEvent};
    use katalogr_model::{
        ArtifactName, ArtifactNode, ExtractError, ExtractInput, Extractor, Fragment, Provenance,
        RelPath,
    };
    use katalogr_store::{CACHE_SCHEMA, ErrorRow, Kindex, ProjectDir, ProjectName};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;

    struct P {
        _tmp: tempfile::TempDir,
        kx: Utf8PathBuf,
        work: Utf8PathBuf,
    }

    fn git(root: &Utf8Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(root)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A project of three `.tx` files, indexed, with the kindex committed.
    fn healthy() -> P {
        let tmp = tempfile::tempdir().unwrap();
        let base = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let work = base.join("work");
        std::fs::create_dir_all(&work).unwrap();
        for (name, text) in [
            ("a.tx", "sym fn a\n"),
            ("b.tx", "sym fn b\n"),
            ("c.tx", "sym fn c\n"),
        ] {
            std::fs::write(work.join(name), text).unwrap();
        }
        let kx = base.join("kx");
        init_project(&kx);
        {
            let e = open_engine(&kx, &work);
            full_index(&e);
        }
        git(&kx, &["add", "-A"]);
        git(&kx, &["commit", "-m", "index"]);
        P {
            _tmp: tmp,
            kx,
            work,
        }
    }

    fn dir(p: &P) -> ProjectDir {
        Kindex::open(&p.kx)
            .unwrap()
            .project(&ProjectName::new("p").unwrap())
            .unwrap()
    }

    fn registry() -> ExtensionRegistry {
        let mut r = ExtensionRegistry::new();
        r.register(Box::new(TestExtractor));
        r
    }

    fn roots(p: &P) -> BTreeMap<String, Utf8PathBuf> {
        BTreeMap::from([("r".to_string(), p.work.clone())])
    }

    fn doctor_with(p: &P, opts: &DoctorOptions) -> Report {
        run(&dir(p), &registry(), &roots(p), Some("r"), opts).unwrap()
    }

    fn doctor(p: &P) -> Report {
        doctor_with(p, &DoctorOptions::default())
    }

    fn find<'a>(r: &'a Report, code: &str) -> Option<&'a Finding> {
        r.findings.iter().find(|f| f.code == code)
    }

    fn codes(r: &Report) -> Vec<&str> {
        r.findings.iter().map(|f| f.code.as_str()).collect()
    }

    fn status(r: &Report, area: &str) -> CheckStatus {
        r.checks.iter().find(|c| c.area == area).unwrap().status
    }

    /// Writes a publish journal into the cache (the way an interrupted publish leaves one) and
    /// creates the temp file it names.
    fn interrupt_publish(p: &P) {
        let e = open_engine(&p.kx, &p.work);
        let tmp = ".tmp-a.tx.kshard.json";
        std::fs::write(e.dir().root().join("shards/r").join(tmp), "{}").unwrap();
        let mut tx = e.cache().write().unwrap();
        tx.set_publish_journal(&[
            (
                format!("shards/r/{tmp}"),
                "shards/r/a.tx.kshard.json".to_string(),
            ),
            (String::new(), "shards/r/b.tx.kshard.json".to_string()),
        ])
        .unwrap();
        tx.commit().unwrap();
    }

    /// Holds the project's processor lock the way a running processor does.
    fn hold_lock(p: &P) -> std::fs::File {
        let path = dir(p).cache_dir().join("processor.lock");
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .unwrap();
        FileExt::try_lock(&f).unwrap();
        f
    }

    fn skip_git() -> DoctorOptions {
        DoctorOptions {
            skip_git: true,
            ..Default::default()
        }
    }

    #[test]
    fn healthy_index_has_no_findings() {
        let p = healthy();
        let r = doctor(&p);
        assert!(r.findings.is_empty(), "{:?}", r.findings);
        assert!(r.healthy && !r.exit_failing());
        assert_eq!(r.checks.len(), 11);
        assert!(
            r.checks.iter().all(|c| c.status == CheckStatus::Ok),
            "{:?}",
            r.checks
        );
        assert_eq!(r.schema, 1);
        assert_eq!(r.versions.cache_schema, CACHE_SCHEMA);
        assert_eq!(r.versions.stored_cache_schema, Some(CACHE_SCHEMA));
        assert_eq!(
            r.versions.kindex_shard_format,
            Some(r.versions.shard_format)
        );
        assert!(r.watermark > 0);
    }

    #[test]
    fn findings_have_code_severity_fix() {
        let p = healthy();
        std::fs::create_dir_all(dir(&p).root().join("profiles")).unwrap();
        std::fs::write(
            dir(&p).root().join("profiles/bad.yaml"),
            "profile: p\nversion: 2\nrules: []\n",
        )
        .unwrap();
        {
            let e = open_engine(&p.kx, &p.work);
            std::fs::write(p.work.join("d.tx"), "sym fn d\n").unwrap();
            e.inbox()
                .append(&NewEvent::file(
                    "r",
                    &RelPath::new("d.tx").unwrap(),
                    EventKind::Changed,
                ))
                .unwrap();
            super::support::drained_graph(&e);
        }
        interrupt_publish(&p);
        let r = doctor(&p);
        let j = find(&r, "KD-JNL-001").expect("interrupted publish");
        assert_eq!(j.severity, Severity::Error);
        assert_eq!(j.area, "journal");
        assert_eq!(j.fix.as_deref(), Some("katalogr index"));
        let prf = find(&r, "KD-PRF-001").expect("profile refusal");
        assert_eq!(prf.severity, Severity::Warning);
        assert!(
            prf.fix
                .as_deref()
                .unwrap()
                .starts_with("edit projects/p/profiles/")
        );
        for f in &r.findings {
            assert!(
                f.code.starts_with("KD-") && f.code.len() == 10,
                "{}",
                f.code
            );
            assert!(!f.message.is_empty() && !f.message.contains('\n'));
            if let Some(fix) = &f.fix {
                assert!(
                    ["katalogr ", "git ", "edit ", "install ", "remove "]
                        .iter()
                        .any(|p| fix.starts_with(p)),
                    "{fix}"
                );
            }
        }
        // Worst first.
        assert!(
            r.findings
                .windows(2)
                .all(|w| w[0].severity >= w[1].severity)
        );
        assert!(r.exit_failing() && !r.healthy);
        assert_eq!(r.summary.error, 1);
    }

    #[test]
    fn shard_cache_mismatch_found() {
        let p = healthy();
        let d = dir(&p);
        let path = |s: &str| d.shard_path("r", &RelPath::new(s).unwrap());
        // Edited after indexing, deleted, and a stray new shard.
        let mut bytes = std::fs::read(path("a.tx")).unwrap();
        bytes.push(b' ');
        std::fs::write(path("a.tx"), bytes).unwrap();
        std::fs::remove_file(path("b.tx")).unwrap();
        std::fs::write(path("stray.tx"), "{}").unwrap();
        let r = doctor_with(
            &p,
            &DoctorOptions {
                verbose: true,
                skip_git: true,
                ..Default::default()
            },
        );
        let s1 = find(&r, "KD-SHD-001").expect("changed and new");
        assert_eq!((s1.severity, s1.count), (Severity::Warning, Some(2)));
        assert!(s1.message.contains("1 new, 1 changed"), "{}", s1.message);
        assert_eq!(
            s1.sample,
            vec!["r:stray.tx".to_string(), "r:a.tx".to_string()]
        );
        let s2 = find(&r, "KD-SHD-002").expect("row without file");
        assert_eq!(s2.count, Some(1));
        assert_eq!(s2.sample, vec!["r:b.tx".to_string()]);
        // Without --verbose no keys are listed.
        let r = doctor_with(&p, &skip_git());
        assert!(find(&r, "KD-SHD-001").unwrap().sample.is_empty());
        // A publish in flight makes both informational.
        let _lock = hold_lock(&p);
        let r = doctor_with(&p, &skip_git());
        assert_eq!(find(&r, "KD-SHD-001").unwrap().severity, Severity::Info);
        assert_eq!(find(&r, "KD-SHD-002").unwrap().severity, Severity::Info);
    }

    #[test]
    fn quarantine_reported() {
        let p = healthy();
        let d = dir(&p);
        std::fs::write(
            d.shard_path("r", &RelPath::new("junk.tx").unwrap()),
            "not a shard",
        )
        .unwrap();
        // A normal open sets the undecodable shard aside.
        let e = open_engine(&p.kx, &p.work);
        drop(e);
        let r = doctor_with(
            &p,
            &DoctorOptions {
                verbose: true,
                skip_git: true,
                ..Default::default()
            },
        );
        let q = find(&r, "KD-SHD-003").expect("quarantine");
        assert_eq!((q.severity, q.count), (Severity::Warning, Some(1)));
        assert_eq!(q.fix.as_deref(), Some("katalogr index --full"));
        assert_eq!(q.sample, vec!["r:junk.tx.kshard.json".to_string()]);
    }

    #[test]
    fn interrupted_publish_found() {
        let p = healthy();
        interrupt_publish(&p);
        let r = doctor_with(&p, &skip_git());
        let f = find(&r, "KD-JNL-001").expect("journal");
        assert_eq!((f.severity, f.count), (Severity::Error, Some(2)));
        assert!(
            f.message.contains("1 temp files still present"),
            "{}",
            f.message
        );
        assert_eq!(f.fix.as_deref(), Some("katalogr index"));
        assert!(find(&r, "KD-JNL-002").is_none() && find(&r, "KD-LCK-001").is_none());
        assert!(r.exit_failing());
    }

    #[test]
    fn journal_with_processor_is_info() {
        let p = healthy();
        interrupt_publish(&p);
        let _lock = hold_lock(&p);
        let r = doctor_with(&p, &skip_git());
        let f = find(&r, "KD-JNL-002").expect("journal while running");
        assert_eq!(f.severity, Severity::Info);
        assert!(find(&r, "KD-JNL-001").is_none());
        assert_eq!(find(&r, "KD-LCK-001").unwrap().severity, Severity::Info);
        assert!(find(&r, "KD-SHD-001").is_none_or(|f| f.severity == Severity::Info));
    }

    #[test]
    fn orphan_temps_found() {
        let p = healthy();
        let d = dir(&p);
        std::fs::write(d.root().join("shards/r/.tmp-x.kshard.json"), "{}").unwrap();
        let r = doctor_with(&p, &skip_git());
        let f = find(&r, "KD-JNL-003").expect("orphan temp");
        assert_eq!((f.severity, f.count), (Severity::Warning, Some(1)));
        assert!(find(&r, "KD-JNL-001").is_none());
        // With a processor running it is not reported.
        let _lock = hold_lock(&p);
        assert!(find(&doctor_with(&p, &skip_git()), "KD-JNL-003").is_none());
    }

    #[test]
    fn backlog_over_threshold() {
        let p = healthy();
        let d = dir(&p);
        let inbox = Inbox::new(&d);
        let ev = NewEvent::file("r", &RelPath::new("a.tx").unwrap(), EventKind::Changed);
        for _ in 0..999 {
            inbox.append(&ev).unwrap();
        }
        let r = doctor_with(&p, &skip_git());
        let f = find(&r, "KD-QUE-002").expect("below threshold");
        assert_eq!((f.severity, f.count), (Severity::Info, Some(999)));
        assert!(r.healthy);
        // At the threshold it warns; one more makes 1,001.
        inbox.append(&ev).unwrap();
        let r = doctor_with(&p, &skip_git());
        assert_eq!(find(&r, "KD-QUE-001").map(|f| f.count), Some(Some(1000)));
        inbox.append(&ev).unwrap();
        let r = doctor_with(&p, &skip_git());
        let f = find(&r, "KD-QUE-001").expect("over threshold");
        assert_eq!((f.severity, f.count), (Severity::Warning, Some(1001)));
        assert!(find(&r, "KD-QUE-002").is_none() && r.exit_failing());
        // The threshold is configurable.
        let opts = DoctorOptions {
            skip_git: true,
            thresholds: Thresholds {
                backlog_warn: 5000,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(find(&doctor_with(&p, &opts), "KD-QUE-001").is_none());
    }

    #[test]
    fn lock_state_reported() {
        let p = healthy();
        assert!(find(&doctor_with(&p, &skip_git()), "KD-LCK-001").is_none());
        let lock = hold_lock(&p);
        let r = doctor_with(&p, &skip_git());
        let f = find(&r, "KD-LCK-001").expect("held");
        assert_eq!(f.severity, Severity::Info);
        assert_eq!(status(&r, "lock"), CheckStatus::Findings);
        drop(lock);
        assert!(find(&doctor_with(&p, &skip_git()), "KD-LCK-001").is_none());
        // A real processor holds the cache too, so the cache-based checks are skipped.
        let e = open_engine(&p.kx, &p.work);
        let _proc = katalogr_engine::queue::Processor::try_acquire(&e)
            .unwrap()
            .unwrap();
        let r = doctor_with(&p, &skip_git());
        assert!(
            codes(&r).contains(&"KD-LCK-001") && codes(&r).contains(&"KD-CCH-004"),
            "{:?}",
            codes(&r)
        );
        assert_eq!(status(&r, "queue"), CheckStatus::Skipped);
    }

    #[test]
    fn set_aside_events_found() {
        let p = healthy();
        let d = dir(&p);
        let inbox = d.cache_dir().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::write(inbox.join("0001-1-aa.json.bad"), "garbage").unwrap();
        let r = doctor_with(&p, &skip_git());
        let f = find(&r, "KD-QUE-003").expect("set aside");
        assert_eq!((f.severity, f.count), (Severity::Warning, Some(1)));
        assert!(
            f.fix
                .as_deref()
                .unwrap()
                .starts_with("remove the *.bad files")
        );
        assert!(!f.message.contains("garbage"));
    }

    /// A trivial extractor with its own id and file suffix.
    struct Simple(&'static str, &'static str);

    impl Extractor for Simple {
        fn id(&self) -> &'static str {
            self.0
        }
        fn handles(&self, path: &RelPath) -> bool {
            path.as_str().ends_with(self.1)
        }
        fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
            Ok(Fragment {
                provenance: Provenance {
                    repo: input.repo.to_string(),
                    path: input.path.clone(),
                    revision: input.revision.to_string(),
                },
                extension: self.0.to_string(),
                artifact: ArtifactNode {
                    name: ArtifactName {
                        repo: input.repo.to_string(),
                        path: input.path.clone(),
                    },
                    facts: BTreeMap::new(),
                },
                symbols: Vec::new(),
                links: Vec::new(),
                refs: Vec::new(),
                exports: Vec::new(),
                bindings: Vec::new(),
            })
        }
    }

    #[test]
    fn parse_error_rate_per_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let base = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let work = base.join("work");
        std::fs::create_dir_all(&work).unwrap();
        // (extension, suffix, files, files with errors)
        let plan = [
            ("alpha", ".aa", 10, 3),
            ("beta", ".bb", 10, 6),
            ("gamma", ".gg", 4, 2),
        ];
        let mut bad: Vec<String> = Vec::new();
        for (_, suffix, n, nbad) in plan {
            for i in 0..n {
                let name = format!("f{i}{suffix}");
                std::fs::write(work.join(&name), "x").unwrap();
                if i < nbad {
                    bad.push(name);
                }
            }
        }
        let kx = base.join("kx");
        init_project(&kx);
        let make = || {
            let mut reg = ExtensionRegistry::new();
            for (id, suffix, ..) in plan {
                reg.register(Box::new(Simple(id, suffix)));
            }
            reg
        };
        {
            let e = open_engine_with(&kx, &work, make());
            full_index(&e);
            let mut tx = e.cache().write().unwrap();
            for name in &bad {
                tx.put_index_errors(
                    "r",
                    &RelPath::new(name).unwrap(),
                    &[ErrorRow {
                        span: None,
                        message: "x".into(),
                    }],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        let d = Kindex::open(&kx)
            .unwrap()
            .project(&ProjectName::new("p").unwrap())
            .unwrap();
        let roots = BTreeMap::from([("r".to_string(), work.clone())]);
        let r = run(
            &d,
            &make(),
            &roots,
            None,
            &DoctorOptions {
                skip_git: true,
                ..Default::default()
            },
        )
        .unwrap();
        let a = find(&r, "KD-PRS-001").expect("alpha warns");
        assert_eq!((a.severity, a.count), (Severity::Warning, Some(3)));
        assert!(
            a.message.contains("alpha")
                && a.message.contains("3 of 10")
                && a.message.contains("30.0%"),
            "{}",
            a.message
        );
        let b = find(&r, "KD-PRS-002").expect("beta errors");
        assert_eq!((b.severity, b.count), (Severity::Error, Some(6)));
        assert!(
            b.message.contains("beta") && b.message.contains("60.0%"),
            "{}",
            b.message
        );
        assert!(
            r.findings.iter().all(|f| !f.message.contains("gamma")),
            "below the minimum file count"
        );
        assert_eq!(r.findings.iter().filter(|f| f.area == "parse").count(), 2);
    }

    #[test]
    fn cache_schema_mismatch() {
        let p = healthy();
        {
            let e = open_engine(&p.kx, &p.work);
            let mut tx = e.cache().write().unwrap();
            tx.set_meta("cache_schema", &(CACHE_SCHEMA + 1).to_le_bytes())
                .unwrap();
            tx.commit().unwrap();
        }
        let r = doctor_with(&p, &skip_git());
        let f = find(&r, "KD-CCH-002").expect("schema differs");
        assert_eq!(f.severity, Severity::Warning);
        assert_eq!(r.versions.stored_cache_schema, Some(CACHE_SCHEMA + 1));
        assert_eq!(r.versions.cache_schema, CACHE_SCHEMA);
        for area in ["queue", "parse", "profile"] {
            assert_eq!(status(&r, area), CheckStatus::Skipped, "{area}");
        }
        assert!(r.exit_failing());
    }

    #[test]
    fn newer_shard_format_error() {
        let p = healthy();
        let d = dir(&p);
        std::fs::write(
            d.root().join("format.json"),
            r#"{"min_katalogr":"9.9.9","shard_format":99}"#,
        )
        .unwrap();
        let r = run(&d, &registry(), &roots(&p), Some("r"), &skip_git()).unwrap();
        let f = find(&r, "KD-FMT-002").expect("newer format");
        assert_eq!(f.severity, Severity::Error);
        assert_eq!(f.fix.as_deref(), Some("install katalogr >= 9.9.9"));
        assert_eq!(r.versions.kindex_shard_format, Some(99));
        std::fs::write(d.root().join("format.json"), "{").unwrap();
        let r = run(&d, &registry(), &roots(&p), Some("r"), &skip_git()).unwrap();
        assert_eq!(find(&r, "KD-FMT-001").unwrap().severity, Severity::Error);
        assert_eq!(r.versions.kindex_shard_format, None);
    }

    #[test]
    fn corrupt_cache_reported() {
        let p = healthy();
        let d = dir(&p);
        let file = d.cache_dir().join("index.redb");
        let bytes = std::fs::read(&file).unwrap();
        std::fs::write(&file, &bytes[..100]).unwrap();
        std::fs::write(d.root().join("shards/r/.tmp-x.kshard.json"), "{}").unwrap();
        let r = doctor_with(&p, &skip_git());
        let f = find(&r, "KD-CCH-003").expect("corrupt cache");
        assert_eq!(f.severity, Severity::Warning);
        assert!(r.exit_failing());
        assert_eq!(
            std::fs::read(&file).unwrap(),
            &bytes[..100],
            "the cache file is untouched"
        );
        // The file-based checks still ran.
        assert_ne!(status(&r, "shards"), CheckStatus::Skipped);
        assert_ne!(status(&r, "roots"), CheckStatus::Skipped);
        assert!(
            find(&r, "KD-JNL-003").is_some(),
            "orphan temps are still found"
        );
        assert_eq!(status(&r, "queue"), CheckStatus::Skipped);
    }

    #[test]
    fn uncommitted_shards_found() {
        let p = healthy();
        let d = dir(&p);
        let path = |s: &str| d.shard_path("r", &RelPath::new(s).unwrap());
        assert!(doctor(&p).findings.is_empty());
        let mut bytes = std::fs::read(path("a.tx")).unwrap();
        bytes.push(b' ');
        std::fs::write(path("a.tx"), bytes).unwrap();
        std::fs::write(path("new.tx"), "{}").unwrap();
        std::fs::remove_file(path("c.tx")).unwrap();
        let r = doctor_with(
            &p,
            &DoctorOptions {
                verbose: true,
                ..Default::default()
            },
        );
        let f = find(&r, "KD-GIT-001").expect("differs from HEAD");
        assert_eq!((f.severity, f.count), (Severity::Warning, Some(3)));
        assert!(
            f.message.contains("1 modified, 1 added, 1 deleted"),
            "{}",
            f.message
        );
        assert!(
            f.fix
                .as_deref()
                .unwrap()
                .contains("git -C <kindex-dir> add projects/p/shards")
        );
        assert_eq!(f.sample.len(), 3);
        // --skip-git skips the check.
        let r = doctor_with(&p, &skip_git());
        assert_eq!(status(&r, "git"), CheckStatus::Skipped);
        assert!(find(&r, "KD-GIT-001").is_none());
    }

    #[test]
    fn not_a_git_repo() {
        let p = healthy();
        std::fs::remove_dir_all(p.kx.join(".git")).unwrap();
        let r = doctor(&p);
        let f = find(&r, "KD-GIT-002").expect("not a repository");
        assert_eq!(f.severity, Severity::Warning);
        assert_eq!(f.fix.as_deref(), Some("katalogr kindex init <kindex-dir>"));
        // A repository with no commit is information.
        git(&p.kx, &["init"]);
        let r = doctor(&p);
        let f = find(&r, "KD-GIT-003").expect("no commit");
        assert_eq!(f.severity, Severity::Info);
    }

    #[test]
    fn profile_refusals_listed() {
        let p = healthy();
        let d = dir(&p);
        std::fs::create_dir_all(d.root().join("profiles")).unwrap();
        std::fs::write(
            d.root().join("profiles/good.yaml"),
            "profile: g\nversion: 1\nrules: []\n",
        )
        .unwrap();
        std::fs::write(
            d.root().join("profiles/bad.yaml"),
            "profile: p\nversion: 2\nrules: []\n",
        )
        .unwrap();
        {
            let e = open_engine(&p.kx, &p.work);
            std::fs::write(p.work.join("d.tx"), "sym fn d\n").unwrap();
            e.inbox()
                .append(&NewEvent::file(
                    "r",
                    &RelPath::new("d.tx").unwrap(),
                    EventKind::Changed,
                ))
                .unwrap();
            super::support::drained_graph(&e);
        }
        let r = doctor_with(&p, &skip_git());
        let prf: Vec<&Finding> = r
            .findings
            .iter()
            .filter(|f| f.code == "KD-PRF-001")
            .collect();
        assert_eq!(prf.len(), 1, "{:?}", r.findings);
        assert!(
            prf[0].message.starts_with("profiles/bad.yaml:2: "),
            "{}",
            prf[0].message
        );
        assert!(prf[0].message.chars().count() <= 200);
        assert_eq!(
            prf[0].fix.as_deref(),
            Some("edit projects/p/profiles/bad.yaml")
        );
    }

    #[test]
    fn repo_without_root() {
        let p = healthy();
        let r = run(&dir(&p), &registry(), &BTreeMap::new(), None, &skip_git()).unwrap();
        let f = find(&r, "KD-ROOT-001").expect("no root");
        assert_eq!(f.severity, Severity::Warning);
        assert_eq!(f.fix.as_deref(), Some("katalogr project add-root r <path>"));
        assert!(find(&doctor(&p), "KD-ROOT-001").is_none());
    }

    #[test]
    fn marker_names_unknown_repo() {
        let p = healthy();
        let r = run(&dir(&p), &registry(), &roots(&p), Some("nope"), &skip_git()).unwrap();
        let f = find(&r, "KD-MRK-004").expect("unknown repo");
        assert_eq!(f.fix.as_deref(), Some("edit .katalogr"));
        assert_eq!(status(&r, "marker"), CheckStatus::Findings);
    }

    fn validate(
        root: &serde_json::Value,
        schema: &serde_json::Value,
        v: &serde_json::Value,
        at: &str,
    ) -> Result<(), String> {
        use serde_json::Value;
        if let Some(r) = schema.get("$ref") {
            let target = r
                .as_str()
                .and_then(|r| r.strip_prefix('#'))
                .and_then(|p| root.pointer(p))
                .ok_or_else(|| format!("{at}: unresolvable $ref {r}"))?;
            return validate(root, target, v, at);
        }
        for key in ["anyOf", "oneOf"] {
            if let Some(options) = schema.get(key).and_then(Value::as_array) {
                return if options.iter().any(|o| validate(root, o, v, at).is_ok()) {
                    Ok(())
                } else {
                    Err(format!("{at}: matches none of {key}"))
                };
            }
        }
        if let Some(t) = schema.get("type") {
            let names: Vec<&str> = match t {
                Value::String(s) => vec![s.as_str()],
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                _ => return Err(format!("{at}: bad type keyword")),
            };
            let ok = names.iter().any(|n| match *n {
                "null" => v.is_null(),
                "boolean" => v.is_boolean(),
                "string" => v.is_string(),
                "array" => v.is_array(),
                "object" => v.is_object(),
                "integer" => v.is_i64() || v.is_u64(),
                "number" => v.is_number(),
                _ => false,
            });
            if !ok {
                return Err(format!("{at}: expected {names:?}, got {v}"));
            }
        }
        if let Some(values) = schema.get("enum").and_then(Value::as_array)
            && !values.contains(v)
        {
            return Err(format!("{at}: {v} not in {values:?}"));
        }
        if let Some(obj) = v.as_object() {
            for k in schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if !obj.contains_key(k) {
                    return Err(format!("{at}: missing required key {k:?}"));
                }
            }
            if let Some(props) = schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
            {
                for (k, x) in obj {
                    if let Some(p) = props.get(k) {
                        validate(root, p, x, &format!("{at}.{k}"))?;
                    }
                }
            }
        }
        if let (Some(items), Some(arr)) = (schema.get("items"), v.as_array()) {
            for (i, x) in arr.iter().enumerate() {
                validate(root, items, x, &format!("{at}[{i}]"))?;
            }
        }
        Ok(())
    }

    fn schema_file() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/v1/doctor.schema.json")
    }

    #[test]
    fn json_matches_schema() {
        let p = healthy();
        interrupt_publish(&p);
        let r = doctor(&p);
        assert!(!r.findings.is_empty());
        let v = serde_json::to_value(&r).unwrap();
        let s = schema();
        validate(&s, &s, &v, "$").unwrap_or_else(|e| panic!("{e}\n{v}"));
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(schema_file()).unwrap()).unwrap();
        validate(&on_disk, &on_disk, &v, "$").unwrap();
        assert_eq!(v["schema"], 1);
        assert_eq!(v["findings"][0]["severity"], "error");
        // The validator rejects what it should.
        let mut bad = v.clone();
        bad["findings"][0]["severity"] = "fatal".into();
        assert!(validate(&s, &s, &bad, "$").is_err());
        let mut bad = v;
        bad.as_object_mut().unwrap().remove("versions");
        assert!(
            validate(&s, &s, &bad, "$")
                .unwrap_err()
                .contains("missing required key")
        );
    }

    #[test]
    fn schema_file_current() {
        let mut want = serde_json::to_string_pretty(&schema()).unwrap();
        want.push('\n');
        if std::env::var_os("KATALOGR_BLESS").is_some() {
            std::fs::write(schema_file(), &want).unwrap();
        }
        let have = std::fs::read_to_string(schema_file()).unwrap_or_default();
        assert_eq!(
            have, want,
            "doctor schema is out of date: run with KATALOGR_BLESS=1"
        );
    }

    /// Every file under `root` with its length, modification time and SHA-256.
    fn snapshot(root: &Utf8Path) -> BTreeMap<String, (u64, std::time::SystemTime, String)> {
        fn walk(
            dir: &Utf8Path,
            root: &Utf8Path,
            out: &mut BTreeMap<String, (u64, std::time::SystemTime, String)>,
        ) {
            for e in std::fs::read_dir(dir).unwrap() {
                let e = e.unwrap();
                let path = Utf8PathBuf::from_path_buf(e.path()).unwrap();
                if e.file_type().unwrap().is_dir() {
                    out.insert(
                        format!("{}/", path.strip_prefix(root).unwrap()),
                        (0, std::time::UNIX_EPOCH, String::new()),
                    );
                    walk(&path, root, out);
                } else {
                    let m = e.metadata().unwrap();
                    let hash: String = Sha256::digest(std::fs::read(&path).unwrap())
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect();
                    out.insert(
                        path.strip_prefix(root).unwrap().to_string(),
                        (m.len(), m.modified().unwrap(), hash),
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    #[test]
    fn does_not_modify_index() {
        let p = healthy();
        interrupt_publish(&p);
        std::fs::write(dir(&p).root().join("shards/r/.tmp-x.kshard.json"), "{}").unwrap();
        let before = snapshot(&p.kx);
        let r = doctor(&p);
        assert!(codes(&r).contains(&"KD-JNL-001"));
        assert_eq!(
            snapshot(&p.kx),
            before,
            "doctor changed the kindex or the cache"
        );
        // Not even a lock file appears in a project that never had one.
        std::fs::remove_file(dir(&p).cache_dir().join("processor.lock")).unwrap();
        let before = snapshot(&p.kx);
        doctor(&p);
        assert_eq!(snapshot(&p.kx), before);
        assert!(!dir(&p).cache_dir().join("processor.lock").exists());
    }
}
