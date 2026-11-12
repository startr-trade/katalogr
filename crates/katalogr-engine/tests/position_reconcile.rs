//! FEAT-084 T2: reconcile the index with the checkout's position before a read.

mod support;

mod position {
    use super::support::{full_index, init_project, open_engine};
    use camino::{Utf8Path, Utf8PathBuf};
    use katalogr_engine::{Answer, Consistency, Engine, QueryOptions, Scope, canonical_graph};
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

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

    struct Setup {
        _tmp: tempfile::TempDir,
        work: Utf8PathBuf,
        engine: Engine,
    }

    /// A committed git repo of `.tx` files, indexed.
    fn setup() -> Setup {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let kx = root.join("kx");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("a.tx"), "sym fn A\nexport k:a A\n").unwrap();
        std::fs::write(work.join("b.tx"), "sym fn B\nref B calls k:a\n").unwrap();
        let w = work.as_std_path();
        git(w, &["init", "-q"]);
        git(w, &["config", "user.name", "Test"]);
        git(w, &["config", "user.email", "test@example.com"]);
        git(w, &["config", "commit.gpgsign", "false"]);
        git(w, &["add", "."]);
        git(w, &["commit", "-q", "-m", "c1"]);
        init_project(&kx);
        let engine = open_engine(&kx, &work);
        full_index(&engine);
        Setup {
            _tmp: tmp,
            work,
            engine,
        }
    }

    fn opts() -> QueryOptions {
        QueryOptions {
            consistency: Consistency::Wait,
            timeout: Duration::from_secs(30),
            scope: Scope::Project,
        }
    }

    fn read(e: &Engine) -> Answer<String> {
        e.with_consistency(&opts(), |tx| canonical_graph(tx))
            .unwrap()
    }

    fn write(work: &Utf8Path, name: &str, text: &str) {
        std::fs::write(work.join(name), text).unwrap();
    }

    /// FEAT-096 R-9: the state `f` is told equals the answer's, clean and dirty.
    #[test]
    fn state_seen_before_compute() {
        let s = setup();
        let seen = |e: &Engine| {
            e.with_consistency_state(&opts(), |_, st| st.clone())
                .unwrap()
        };
        let clean = seen(&s.engine);
        assert!(clean.exact);
        assert_eq!(clean.value.positions, clean.positions);
        assert_eq!(clean.value.exact, clean.exact);
        assert_eq!(clean.positions[0].dirty, None);

        write(&s.work, "a.tx", "sym fn A\nsym fn A2\nexport k:a A2\n");
        let dirty = seen(&s.engine);
        assert!(dirty.exact);
        assert!(dirty.positions[0].dirty.is_some());
        assert_eq!(dirty.value.positions, dirty.positions);
        assert_eq!(dirty.value.exact, dirty.exact);

        // A read that does not reconcile is told what was last recorded, and not exact.
        let stale = QueryOptions {
            consistency: Consistency::StaleOk,
            ..opts()
        };
        let a = s
            .engine
            .with_consistency_state(&stale, |_, st| st.clone())
            .unwrap();
        assert!(!a.exact);
        assert_eq!(
            a.value,
            katalogr_engine::ReadState {
                positions: a.positions.clone(),
                exact: false
            }
        );
    }

    #[test]
    fn reconcile_finds_edit_without_hook() {
        let s = setup();
        let first = read(&s.engine);
        assert!(first.exact);
        assert_eq!(first.positions.len(), 1);
        assert_eq!(first.positions[0].dirty, None);
        assert!(!first.value.contains("A2"));

        // An edit and a new file, with no event sent.
        write(&s.work, "a.tx", "sym fn A\nsym fn A2\nexport k:a A2\n");
        write(&s.work, "c.tx", "sym fn C\n");
        let second = read(&s.engine);
        assert!(second.value.contains("A2"), "edit found");
        assert!(
            second.value.contains("fn C") || second.value.contains("C"),
            "new file found"
        );
        assert!(second.exact);
        assert!(second.positions[0].dirty.is_some());

        // Deleting a tracked file is found as well, then a commit of everything.
        std::fs::remove_file(s.work.join("b.tx")).unwrap();
        let third = read(&s.engine);
        assert!(!third.value.contains("fn B"), "deleted file left the index");
        let w = s.work.as_std_path();
        git(w, &["add", "-A"]);
        git(w, &["commit", "-q", "-m", "c2"]);
        let fourth = read(&s.engine);
        assert!(fourth.exact);
        assert_eq!(fourth.positions[0].dirty, None);
        assert_eq!(fourth.value, third.value);
    }

    #[test]
    fn reconcile_clean_queues_nothing() {
        let s = setup();
        let first = read(&s.engine);
        assert!(first.exact);
        let again = read(&s.engine);
        assert!(again.exact);
        assert_eq!(again.positions, first.positions);
        // Nothing was queued or processed, so the watermark did not move.
        assert_eq!(again.watermark, first.watermark);
        assert_eq!(again.pending, 0);
        assert!(s.engine.inbox().list().unwrap().is_empty());
    }

    #[test]
    fn edit_during_read_is_not_exact() {
        let s = setup();
        let first = read(&s.engine);
        assert!(first.exact);
        let work = s.work.clone();
        let during = s
            .engine
            .with_consistency(&opts(), |tx| {
                // The checkout changes while the read is under way.
                write(&work, "a.tx", "sym fn A\nsym fn Edited\nexport k:a A\n");
                canonical_graph(tx)
            })
            .unwrap();
        assert!(!during.exact);
        // It answers with the last position reached exactly.
        assert_eq!(during.positions, first.positions);
        // The next read sees the edit and is exact.
        let next = read(&s.engine);
        assert!(next.exact);
        assert!(next.value.contains("Edited"));
        assert_ne!(next.positions, first.positions);
    }

    #[test]
    fn status_failure_warns_and_answers() {
        let s = setup();
        let first = read(&s.engine);
        assert!(first.exact);
        std::fs::write(s.work.join(".git/index"), b"not an index").unwrap();
        let a = read(&s.engine);
        assert!(!a.exact);
        assert_eq!(a.positions, first.positions);
        assert!(
            a.warnings.iter().any(|w| w.contains("unreadable")),
            "warning: {:?}",
            a.warnings
        );
        assert_eq!(a.value, first.value);
    }

    #[test]
    fn stale_ok_never_reconciles() {
        let s = setup();
        let first = read(&s.engine);
        write(&s.work, "a.tx", "sym fn A\nsym fn Hidden\nexport k:a A\n");
        let mut o = opts();
        o.consistency = Consistency::StaleOk;
        let a = s
            .engine
            .with_consistency(&o, |tx| canonical_graph(tx))
            .unwrap();
        assert!(!a.value.contains("Hidden"));
        assert!(!a.exact);
        assert_eq!(a.positions, first.positions);
    }

    #[test]
    fn unrecorded_position_not_exact() {
        // An index fed by events alone, with no position ever recorded.
        let s = setup();
        // A fresh cache: the same project opened without the recorded position.
        let mut tx = s.engine.cache().write().unwrap();
        tx.set_meta("position.r", b"null").unwrap();
        tx.commit().unwrap();
        write(&s.work, "a.tx", "sym fn A\nsym fn A3\nexport k:a A3\n");
        s.engine
            .inbox()
            .append(&katalogr_engine::NewEvent::file(
                "r",
                &katalogr_model::RelPath::new("a.tx").unwrap(),
                katalogr_engine::EventKind::Changed,
            ))
            .unwrap();
        let first = read(&s.engine);
        assert!(first.value.contains("A3"));
        assert!(!first.exact, "no recorded position, so not exact");
        assert!(first.positions.is_empty());
        // The next read finds nothing queued, rescans, and then knows its position.
        let second = read(&s.engine);
        assert!(second.exact);
        assert_eq!(second.positions.len(), 1);
    }

    fn head(root: &Utf8Path) -> String {
        let out = Command::new("git")
            .current_dir(root)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// The repo is pinned (FEAT-118) to a second commit, with an uncommitted file beside it.
    fn pinned_setup() -> (Setup, String) {
        let mut s = setup();
        let w = s.work.clone();
        write(&w, "c.tx", "sym fn Committed\n");
        git(w.as_std_path(), &["add", "."]);
        git(w.as_std_path(), &["commit", "-q", "-m", "c2"]);
        write(&w, "d.tx", "sym fn Uncommitted\n");
        let pin = head(&w);
        s.engine.set_source(
            "r",
            katalogr_engine::FileSource::Commit {
                repo_root: w.clone(),
                commit: pin.clone(),
            },
        );
        s.engine.set_pinned_position("r", Some(pin.clone()));
        s.engine.set_pin_rows_follow(true);
        (s, pin)
    }

    fn stored_commit(e: &Engine) -> Option<String> {
        let snap = e.cache().read().unwrap();
        katalogr_engine::consistency::recorded_position(&snap, "r")
            .unwrap()
            .and_then(|p| p.commit)
    }

    /// FEAT-118 R-1: the rows follow the pin, read from git and not from the disk.
    #[test]
    fn pinned_repo_catches_up_to_pin() {
        let (s, pin) = pinned_setup();
        let a = read(&s.engine);
        assert!(a.value.contains("Committed"), "{}", a.value);
        assert!(!a.value.contains("Uncommitted"));
        assert!(a.exact);
        assert_eq!(a.positions.len(), 1);
        assert_eq!(a.positions[0].commit.as_deref(), Some(pin.as_str()));
    }

    /// FEAT-118 R-3: the pin is recorded once its events are applied, and not before.
    #[test]
    fn records_pinned_position() {
        let (s, pin) = pinned_setup();
        assert_ne!(stored_commit(&s.engine).as_deref(), Some(pin.as_str()));
        read(&s.engine);
        assert_eq!(stored_commit(&s.engine).as_deref(), Some(pin.as_str()));
    }
}
