//! FEAT-086 T3: which documents are linked to code that changed. Needs the `.tx` test
//! extractor: `cargo test -p katalogr-dist --features test-extractor`.
#![cfg(feature = "test-extractor")]

mod docs_affected {
    use katalogr_dist::{
        DistError, DocsAffectedParams, Katalogr, Options, Outcome, Overrides, ViewQuery,
        kindex_init, project_init,
    };
    use katalogr_query::DocImpact;
    use std::path::{Path, PathBuf};

    fn git(dir: &Path, args: &[&str]) -> String {
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
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    const CODE: &str = "sym fn A\nsym fn B\nexport ns:A A\nexport ns:B B\n";
    const DOC: &str = "sym fn Da\nsym fn Db\nref Da links-to ns:A\nref Db links-to ns:B\n";

    struct World {
        _tmp: tempfile::TempDir,
        app: PathBuf,
        docs: Option<PathBuf>,
    }

    /// `app` (git, branch `branch`) with `code.tx` and, when `docs_in_app`, `doc.tx`; with
    /// `docs_in_app` false the document lives in a second repo `docs`.
    fn world(branch: &str, docs_in_app: bool) -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let app = root.join("app");
        std::fs::create_dir_all(&app).unwrap();
        git(&app, &["init", "-q", "-b", branch]);
        std::fs::write(app.join("code.tx"), CODE).unwrap();
        std::fs::write(app.join(".gitignore"), ".katalogr\n").unwrap();
        let mut repos = vec![("app".to_string(), app.clone())];
        let mut docs = None;
        if docs_in_app {
            std::fs::write(app.join("doc.tx"), DOC).unwrap();
        } else {
            let d = root.join("docs");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("doc.tx"), DOC).unwrap();
            repos.push(("docs".to_string(), d.clone()));
            docs = Some(d);
        }
        git(&app, &["add", "."]);
        git(&app, &["commit", "-q", "-m", "base"]);
        let kx = root.join("kx");
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", repos, true).unwrap();
        World {
            _tmp: tmp,
            app,
            docs,
        }
    }

    fn open(w: &World) -> Katalogr {
        let k = Katalogr::resolve(&w.app, &Overrides::default()).unwrap();
        k.index(None, true, None, &Options::default()).unwrap();
        k
    }

    fn reindex(k: &Katalogr) {
        k.index(None, true, None, &Options::default()).unwrap();
    }

    fn touch(dir: &Path, file: &str, from: &str, to: &str) {
        let t = std::fs::read_to_string(dir.join(file)).unwrap();
        std::fs::write(dir.join(file), t.replace(from, to)).unwrap();
    }

    fn found(
        k: &Katalogr,
        since: Option<&str>,
        opts: &Options,
    ) -> Result<(DocImpact, katalogr_dist::Report), DistError> {
        let r = k.docs_affected(since, &DocsAffectedParams::default(), opts)?;
        match r.outcome {
            Outcome::Found(d) => Ok((d, r.report)),
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    #[test]
    fn since_defaults_to_merge_base() {
        let w = world("main", true);
        git(&w.app, &["checkout", "-q", "-b", "feat"]);
        touch(&w.app, "code.tx", "sym fn A", "sym  fn A");
        git(&w.app, &["commit", "-q", "-am", "edit a"]);
        git(&w.app, &["checkout", "-q", "main"]);
        touch(&w.app, "code.tx", "sym fn B", "sym  fn B");
        git(&w.app, &["commit", "-q", "-am", "edit b on main"]);
        git(&w.app, &["checkout", "-q", "feat"]);
        let k = open(&w);
        let (d, _) = found(&k, None, &Options::default()).unwrap();
        assert_eq!(d.changed_files, 1);
        assert_eq!(d.docs.len(), 1, "{d:?}");
        let links = &d.docs[0].links;
        assert_eq!(links.len(), 1, "{links:?}");
        assert!(links[0].target.contains('A') && !links[0].target.contains('B'));
        assert_eq!(links[0].precision, "symbol");
        // an explicit --since of the base sees the same change
        let base = git(&w.app, &["merge-base", "feat", "main"]);
        let (e, _) = found(&k, Some(&base), &Options::default()).unwrap();
        assert_eq!(e.docs, d.docs);
    }

    #[test]
    fn dirty_since_is_usage_error() {
        let w = world("main", true);
        let k = open(&w);
        let head = git(&w.app, &["rev-parse", "HEAD"]);
        let dirty = format!("{head}+{}", "0".repeat(64));
        let e = found(&k, Some(&dirty), &Options::default()).unwrap_err();
        assert!(e.is_usage(), "{e}");
        let e = found(&k, Some("no-such-rev"), &Options::default()).unwrap_err();
        assert!(e.is_usage(), "{e}");
    }

    #[test]
    fn no_default_branch_needs_since() {
        let w = world("trunk", true);
        let k = open(&w);
        let e = found(&k, None, &Options::default()).unwrap_err();
        assert!(e.is_usage() && e.to_string().contains("--since"), "{e}");
        let (d, _) = found(&k, Some("HEAD"), &Options::default()).unwrap();
        assert!(d.docs.is_empty());
    }

    #[test]
    fn doc_in_other_repo_found() {
        let w = world("main", false);
        let base = git(&w.app, &["rev-parse", "HEAD"]);
        touch(&w.app, "code.tx", "sym fn A", "sym  fn A");
        git(&w.app, &["commit", "-q", "-am", "edit a"]);
        let k = open(&w);
        let (d, _) = found(&k, Some(&base), &Options::default()).unwrap();
        assert_eq!(d.docs.len(), 1, "{d:?}");
        assert!(
            d.docs[0].doc.name.starts_with("docs:"),
            "{:?}",
            d.docs[0].doc
        );
        assert!(!d.docs[0].doc_changed);
        assert!(w.docs.is_some());
    }

    fn view() -> Options {
        Options {
            view: Some(ViewQuery {
                op: "docs-affected".into(),
                args: Default::default(),
            }),
            ..Options::default()
        }
    }

    #[test]
    fn answer_carries_position_and_id() {
        let w = world("main", true);
        touch(&w.app, "code.tx", "sym fn A", "sym  fn A");
        git(&w.app, &["commit", "-q", "-am", "edit a"]);
        let k = open(&w);
        let (_, report) = found(&k, Some("HEAD~1"), &view()).unwrap();
        assert!(!report.positions.is_empty());
        assert!(report.exact, "{report:?}");
        assert!(
            report
                .view_id
                .as_deref()
                .is_some_and(|v| v.starts_with("v1-"))
        );
    }

    #[test]
    fn repeats_at_same_position() {
        let w = world("main", true);
        touch(&w.app, "code.tx", "sym fn A", "sym  fn A");
        git(&w.app, &["commit", "-q", "-am", "edit a"]);
        let k = open(&w);
        let (a, ra) = found(&k, Some("HEAD~1"), &view()).unwrap();
        reindex(&k);
        let (b, rb) = found(&k, Some("HEAD~1"), &view()).unwrap();
        assert_eq!(a, b);
        assert_eq!(ra.view_id, rb.view_id);
        assert!(ra.view_id.is_some());
    }

    /// FEAT-141 acceptance: a checkout whose files git wrote with CRLF (`core.autocrlf=true`, or
    /// `.gitattributes` `* text=auto eol=crlf`) has no change against `HEAD`, so no document is
    /// affected; a real edit still affects the linked document.
    #[test]
    fn crlf_checkout_affects_nothing() {
        for attributes in [false, true] {
            let w = world("main", true);
            if attributes {
                std::fs::write(w.app.join(".gitattributes"), "* text=auto eol=crlf\n").unwrap();
                git(&w.app, &["add", ".gitattributes"]);
                git(&w.app, &["commit", "-q", "-m", "attributes"]);
            } else {
                git(&w.app, &["config", "core.autocrlf", "true"]);
            }
            for f in ["code.tx", "doc.tx", ".gitattributes"] {
                let _ = std::fs::remove_file(w.app.join(f));
            }
            git(&w.app, &["checkout", "--", "."]);
            let code = std::fs::read_to_string(w.app.join("code.tx")).unwrap();
            assert_eq!(code, CODE.replace('\n', "\r\n"), "checked out with CRLF");
            let k = open(&w);
            let (d, _) = found(&k, Some("HEAD"), &Options::default()).unwrap();
            assert_eq!(d.changed_files, 0, "attributes {attributes}: {d:?}");
            assert!(d.docs.is_empty(), "{d:?}");
            touch(&w.app, "code.tx", "sym fn A", "sym  fn A");
            reindex(&k);
            let (d, _) = found(&k, Some("HEAD"), &Options::default()).unwrap();
            assert_eq!(d.changed_files, 1, "attributes {attributes}: {d:?}");
            assert_eq!(d.docs.len(), 1, "{d:?}");
        }
    }
}
