mod fixtures {
    use camino::{Utf8Path, Utf8PathBuf};
    use katalogr_acceptance::privacy;
    use katalogr_acceptance::{Expected, load_manifest, testfmt};
    use std::collections::BTreeSet;

    fn workspace() -> Utf8PathBuf {
        Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize_utf8()
            .unwrap()
    }

    fn subdirs(dir: &Utf8Path) -> Vec<Utf8PathBuf> {
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter_map(|e| Utf8PathBuf::from_path_buf(e.path()).ok())
            .filter(|p| p.is_dir())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn coverage_manifest_complete() {
        let fx = workspace().join("fixtures");
        let manifest = load_manifest(&fx).unwrap();
        let mut problems = Vec::new();
        for ext in testfmt::registry().extractors() {
            for pattern in ext.patterns() {
                let cases = manifest
                    .technologies
                    .get(ext.id())
                    .and_then(|t| t.get(*pattern));
                if cases.is_none_or(|c| c.is_empty()) {
                    problems.push(format!("no case for ({}, {pattern})", ext.id()));
                }
            }
        }
        for (tech, patterns) in &manifest.technologies {
            for (pattern, cases) in patterns {
                for c in cases {
                    if !fx.join(c).is_dir() {
                        problems.push(format!("{tech}/{pattern}: case dir {c} does not exist"));
                    }
                }
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn expectations_declared() {
        let fx = workspace().join("fixtures");
        let manifest = load_manifest(&fx).unwrap();
        let mut cases: BTreeSet<Utf8PathBuf> = BTreeSet::new();
        for tech in subdirs(&fx) {
            cases.extend(subdirs(&tech));
        }
        for patterns in manifest.technologies.values() {
            for c in patterns.values().flatten() {
                cases.insert(fx.join(c));
            }
        }
        let mut problems = Vec::new();
        for case in &cases {
            if let Err(e) = Expected::load(&case.join("expected.yaml")) {
                problems.push(e.to_string());
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn privacy_lint() {
        let root = workspace();
        let denied = privacy::load_denied(&root.join("fixtures/privacy.sha256")).unwrap();
        let mut failures = privacy::lint_tree(&root.join("fixtures"), &denied);
        for krate in subdirs(&root.join("crates")) {
            failures.extend(privacy::lint_tree(&krate.join("tests"), &denied));
        }
        assert!(
            failures.is_empty(),
            "denied term at:\n{}",
            failures.join("\n")
        );

        // Self-test: the deny list is empty in the repository, so prove the lint works.
        let term = "qzx-selftest-term";
        let tmp = tempfile::tempdir().unwrap();
        let file = Utf8PathBuf::from_path_buf(tmp.path().join("probe.txt")).unwrap();
        std::fs::write(&file, format!("clean line\nsee {term}/sub here\n")).unwrap();
        let set = BTreeSet::from([privacy::hash(term)]);
        let errs = privacy::lint_file(&file, &set).unwrap_err();
        assert_eq!(errs, vec![format!("{file}:2")]);
        assert!(errs.iter().all(|e| !e.contains(term)));
        assert!(privacy::lint_file(&file, &BTreeSet::new()).is_ok());
    }
}
