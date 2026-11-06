//! Reports at each merge (FEAT-095 R-16). Needs the `.tx` test extractor.
#![cfg(feature = "test-extractor")]

mod diff_requests {
    use katalogr_dist::diff_requests::{MAX_REQUESTS, queue_diff_request};
    use katalogr_dist::{Katalogr, Options, Overrides, kindex_init, project_init};
    use katalogr_store::{AnswerStore, DirAnswerStore};
    use std::path::{Path, PathBuf};

    fn git(root: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("run git");
        assert!(st.success(), "git {args:?} failed");
    }

    fn opts() -> Options {
        Options {
            timeout: Some(120.0),
            consistency: Some(katalogr_dist::Consistency::WaitAll),
            ..Options::default()
        }
    }

    struct Fx {
        _tmp: tempfile::TempDir,
        kx: PathBuf,
        repo: PathBuf,
        k: Katalogr,
    }

    /// A repo on `main` with two commits, a project over it, the base indexed, answers on.
    fn fx() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let kx = tmp.path().join("kx");
        let repo = tmp.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("g.tx"), "sym fn a\nsym fn b\nlink b calls a\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c1"]);
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), repo.clone())], true).unwrap();
        let mut k = Katalogr::resolve(
            &repo,
            &Overrides {
                kindex: Some(kx.display().to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        k.index(None, true, None, &opts()).unwrap();
        k.set_answer_build(Some("b1".into()));
        Fx {
            _tmp: tmp,
            kx,
            repo,
            k,
        }
    }

    impl Fx {
        fn requests(&self) -> Vec<String> {
            let dir = self.kx.join("projects/demo/.cache/diff-requests");
            let mut v: Vec<String> = std::fs::read_dir(dir)
                .map(|d| {
                    d.flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            v.sort();
            v
        }

        fn entries(&self, op: &str) -> usize {
            DirAnswerStore::new(
                camino::Utf8PathBuf::from_path_buf(self.kx.join("projects/demo/.cache/answers"))
                    .unwrap(),
            )
            .list()
            .unwrap()
            .iter()
            .filter(|e| e.header.op == op)
            .count()
        }

        /// A merge of a side branch, then the post-merge hook's event.
        fn merge_and_hook(&self) {
            git(&self.repo, &["checkout", "-q", "-b", "feat"]);
            std::fs::write(
                self.repo.join("g.tx"),
                "sym fn a\nsym fn b\nsym fn c\nlink b calls a\nlink c calls a\n",
            )
            .unwrap();
            git(&self.repo, &["add", "-A"]);
            git(&self.repo, &["commit", "-q", "-m", "feature"]);
            git(&self.repo, &["checkout", "-q", "main"]);
            git(
                &self.repo,
                &["merge", "-q", "--no-ff", "feat", "-m", "merge"],
            );
            self.k.notify_from_git("post-merge", &[]).unwrap();
        }
    }

    #[test]
    fn hook_queues_and_processes_nothing() {
        let f = fx();
        f.merge_and_hook();
        let reqs = f.requests();
        assert_eq!(reqs.len(), 1, "{reqs:?}");
        assert!(reqs[0].starts_with("app-") && reqs[0].ends_with(".req"));
        let path =
            f.kx.join("projects/demo/.cache/diff-requests")
                .join(&reqs[0]);
        assert_eq!(std::fs::metadata(path).unwrap().len(), 0, "an empty file");
        assert_eq!(f.entries("diff"), 0, "no report is built by the hook");
        assert!(
            f.k.context_list()
                .unwrap()
                .iter()
                .all(|c| c.kind != "commit"),
            "no commit context is made by the hook"
        );
    }

    #[test]
    fn index_builds_requested_reports() {
        let f = fx();
        f.merge_and_hook();
        let out = f.k.diff_requested(&opts()).unwrap();
        assert_eq!((out.built, out.failed), (1, 0), "{out:?}");
        assert!(f.requests().is_empty(), "a built request is removed");
        assert_eq!(f.entries("diff"), 3, "JSON, Markdown and HTML");
        // Nothing queued, nothing to do.
        assert_eq!(f.k.diff_requested(&opts()).unwrap().built, 0);
    }

    #[test]
    fn failure_keeps_request_and_warns() {
        let f = fx();
        let dir = f.kx.join("projects/demo/.cache/diff-requests");
        std::fs::create_dir_all(&dir).unwrap();
        let name = format!("app-{}-{}.req", "1".repeat(40), "2".repeat(40));
        std::fs::write(dir.join(&name), b"").unwrap();
        let out = f.k.diff_requested(&opts()).unwrap();
        assert_eq!((out.built, out.failed), (0, 1), "{out:?}");
        assert!(
            out.warnings.iter().any(|w| w.contains("stays queued")),
            "{out:?}"
        );
        assert_eq!(f.requests(), [name]);
    }

    #[test]
    fn queue_is_capped() {
        let f = fx();
        let b = "b".repeat(40);
        for i in 0..(MAX_REQUESTS + 8) {
            queue_diff_request(f.k.project(), "app", &format!("{i:040x}"), &b).unwrap();
        }
        let reqs = f.requests();
        assert_eq!(reqs.len(), MAX_REQUESTS);
        // The oldest by name went.
        assert!(!reqs.iter().any(|n| n.contains(&format!("{:040x}", 0))));
        assert!(
            reqs.iter()
                .any(|n| n.contains(&format!("{:040x}", MAX_REQUESTS + 7)))
        );
    }
}
