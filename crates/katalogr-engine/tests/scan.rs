mod support;

mod graph {
    use katalogr_engine::scan::{PathChange, ScanError, changed_paths};
    use katalogr_model::RelPath;
    use std::path::Path;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) {
        let st = Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .expect("run git");
        assert!(st.success(), "git {args:?} failed");
    }

    #[test]
    fn reindex_from_diff_range() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        git(root, &["config", "user.name", "Test"]);
        git(root, &["config", "user.email", "test@example.com"]);
        git(root, &["config", "commit.gpgsign", "false"]);

        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        std::fs::write(root.join("b.txt"), "bee\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "c1"]);

        std::fs::write(root.join("a.txt"), "two\n").unwrap();
        std::fs::remove_file(root.join("b.txt")).unwrap();
        std::fs::write(root.join("c.txt"), "sea\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "c2"]);

        let expected = vec![
            PathChange::Modified(RelPath::new("a.txt").unwrap()),
            PathChange::Deleted(RelPath::new("b.txt").unwrap()),
            PathChange::Added(RelPath::new("c.txt").unwrap()),
        ];
        assert_eq!(changed_paths(root, "HEAD~1..HEAD").unwrap(), expected);
        assert_eq!(changed_paths(root, "HEAD~1").unwrap(), expected);

        match changed_paths(root, "no-such-rev") {
            Err(ScanError::BadRevision { rev, .. }) => assert_eq!(rev, "no-such-rev"),
            other => panic!("expected BadRevision, got {other:?}"),
        }
    }
}

mod scan {
    use camino::Utf8PathBuf;
    use katalogr_engine::registry::ExtensionRegistry;
    use katalogr_engine::scan::{list_files, list_files_warned};
    use katalogr_engine::{NewEvent, Scope, queue::Processor};
    use katalogr_model::RelPath;
    use katalogr_store::{Kindex, ProjectConfig, ProjectName, Reader, RepoConfig};
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::process::Command;
    use std::time::{Duration, Instant};

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

    fn put(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn init(root: &Path) {
        git(root, &["init", "-q"]);
        git(root, &["config", "user.name", "Test"]);
        git(root, &["config", "user.email", "test@example.com"]);
        git(root, &["config", "commit.gpgsign", "false"]);
    }

    fn names(root: &Path) -> Vec<String> {
        list_files(root, &["**".to_string()], &[])
            .unwrap()
            .iter()
            .map(|p| p.as_str().to_string())
            .collect()
    }

    #[test]
    fn ignored_not_listed() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        init(r);
        put(r, ".gitignore", "*.log\nbuild/\n");
        put(r, "src/.gitignore", "gen.txt\n");
        put(r, "a.txt", "a\n");
        put(r, "x.log", "noise\n");
        put(r, "build/out.txt", "o\n");
        put(r, "src/b.txt", "b\n");
        put(r, "src/gen.txt", "g\n");
        assert_eq!(
            names(r),
            [".gitignore", "a.txt", "src/.gitignore", "src/b.txt"]
        );
    }

    #[test]
    fn info_exclude_respected() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        init(r);
        put(r, ".git/info/exclude", "secret.txt\n");
        put(r, "a.txt", "a\n");
        put(r, "secret.txt", "s\n");
        assert_eq!(names(r), ["a.txt"]);
    }

    #[test]
    fn tracked_ignored_still_listed() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        init(r);
        put(r, "kept.log", "tracked\n");
        put(r, "keptdir/in.txt", "tracked\n");
        git(r, &["add", "."]);
        git(r, &["commit", "-q", "-m", "c1"]);
        put(r, ".gitignore", "*.log\nkeptdir/\n");
        put(r, "other.log", "x\n");
        put(r, "keptdir/new.txt", "x\n");
        let (files, warnings) = list_files_warned(r, &["**".to_string()], &[]).unwrap();
        let files: Vec<&str> = files.iter().map(RelPath::as_str).collect();
        assert_eq!(files, [".gitignore", "kept.log", "keptdir/in.txt"]);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn plain_directory_lists_all() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        put(r, ".gitignore", "*.log\n");
        put(r, "a.txt", "a\n");
        put(r, "x.log", "noise\n");
        assert_eq!(names(r), [".gitignore", "a.txt", "x.log"]);
    }

    #[test]
    fn rescan_removes_newly_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let kx = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let work = Utf8PathBuf::from_path_buf(tmp.path().join("work")).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        Kindex::init(&kx)
            .unwrap()
            .create_project(
                &ProjectName::new("p").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        init(work.as_std_path());
        put(work.as_std_path(), "keep.tx", "sym fn Keep\n");
        put(work.as_std_path(), "gen.tx", "sym fn Gen\n");
        let dir = Kindex::open(&kx)
            .unwrap()
            .project(&ProjectName::new("p").unwrap())
            .unwrap();
        let mut reg = ExtensionRegistry::new();
        reg.register(Box::new(crate::support::TestExtractor));
        let roots = BTreeMap::from([("r".to_string(), work.clone())]);
        let e =
            katalogr_engine::Engine::open(dir, reg, roots, Instant::now() + Duration::from_secs(5))
                .unwrap();
        let soon = || Instant::now() + Duration::from_secs(5);
        let round = || {
            e.inbox().append(&NewEvent::rescan("r")).unwrap();
            let mut p = Processor::try_acquire(&e).unwrap().unwrap();
            p.ingest().unwrap();
            p.process(&Scope::Project, 256, soon()).unwrap();
        };
        round();
        assert_eq!(e.cache().read().unwrap().artifacts().unwrap().len(), 2);
        // The file becomes ignored; the next rescan sees it as gone.
        put(work.as_std_path(), ".gitignore", "gen.tx\n");
        round();
        assert_eq!(e.cache().read().unwrap().artifacts().unwrap().len(), 1);
        assert!(
            !crate::support::published(&e)
                .contains(&("r".to_string(), RelPath::new("gen.tx").unwrap())),
            "gen.tx has no file revision"
        );
    }

    #[test]
    fn event_for_ignored_file_not_indexed() {
        let tmp = tempfile::tempdir().unwrap();
        let kx = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let work = Utf8PathBuf::from_path_buf(tmp.path().join("work")).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        Kindex::init(&kx)
            .unwrap()
            .create_project(
                &ProjectName::new("p").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        init(work.as_std_path());
        put(work.as_std_path(), ".gitignore", "gen.tx\n");
        put(work.as_std_path(), "keep.tx", "sym fn Keep\n");
        put(work.as_std_path(), "gen.tx", "sym fn Gen\n");
        let dir = Kindex::open(&kx)
            .unwrap()
            .project(&ProjectName::new("p").unwrap())
            .unwrap();
        let mut reg = ExtensionRegistry::new();
        reg.register(Box::new(crate::support::TestExtractor));
        let roots = BTreeMap::from([("r".to_string(), work.clone())]);
        let e =
            katalogr_engine::Engine::open(dir, reg, roots, Instant::now() + Duration::from_secs(5))
                .unwrap();
        for path in ["gen.tx", "keep.tx"] {
            e.inbox()
                .append(&NewEvent::file(
                    "r",
                    &RelPath::new(path).unwrap(),
                    katalogr_engine::EventKind::Changed,
                ))
                .unwrap();
        }
        let mut p = Processor::try_acquire(&e).unwrap().unwrap();
        p.ingest().unwrap();
        p.process(
            &Scope::Project,
            256,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(e.cache().read().unwrap().artifacts().unwrap().len(), 1);
        assert!(
            !crate::support::published(&e)
                .contains(&("r".to_string(), RelPath::new("gen.tx").unwrap())),
            "gen.tx has no file revision"
        );
    }
}
