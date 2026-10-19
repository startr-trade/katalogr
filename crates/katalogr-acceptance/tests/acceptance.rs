mod acceptance {
    use camino::Utf8Path;
    use katalogr_acceptance::{full_registry, load_manifest, run_case, testfmt};

    #[test]
    fn expected_vs_actual_report() {
        let fx = Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let manifest = load_manifest(&fx).unwrap();
        let mut failed = Vec::new();
        for case in manifest
            .technologies
            .values()
            .flat_map(|p| p.values().flatten())
        {
            let report = run_case(&full_registry, &fx.join(case)).unwrap();
            if !report.passed() {
                failed.push(report.render());
            }
        }
        assert!(failed.is_empty(), "{}", failed.join("\n"));
    }

    #[test]
    fn mismatch_fails() {
        let dir = Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/mismatch");
        let report = run_case(&testfmt::registry, &dir).unwrap();
        assert!(!report.passed());
        assert_eq!(report.missing_symbols, ["fx:a.tx#fn:absent"]);
        assert_eq!(report.unexpected_symbols, ["fx:a.tx#fn:present"]);
        let text = report.render();
        assert!(text.contains("missing symbol: fx:a.tx#fn:absent"), "{text}");
        assert!(
            text.contains("unexpected symbol: fx:a.tx#fn:present"),
            "{text}"
        );
    }

    fn case(name: &str) -> camino::Utf8PathBuf {
        Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/test")
            .join(name)
    }

    /// Copy a case into a temp dir, replacing `from` by `to` in its `expected.yaml`.
    fn flipped(name: &str, from: &str, to: &str) -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dst = camino::Utf8PathBuf::from_path_buf(tmp.path().join(name)).unwrap();
        let src = case(name);
        std::fs::create_dir_all(dst.join("_profiles")).unwrap();
        for e in walk(&src) {
            let rel = e.strip_prefix(&src).unwrap();
            std::fs::copy(&e, dst.join(rel)).unwrap();
        }
        let p = dst.join("expected.yaml");
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains(from), "{text}");
        std::fs::write(&p, text.replace(from, to)).unwrap();
        (tmp, dst)
    }

    fn walk(dir: &Utf8Path) -> Vec<camino::Utf8PathBuf> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = camino::Utf8PathBuf::from_path_buf(e.path()).unwrap();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
        out
    }

    #[test]
    fn contracts_asserted() {
        let report = run_case(&testfmt::registry, &case("contracts-bind")).unwrap();
        assert!(report.passed(), "{}", report.render());
        let (_t, dir) = flipped(
            "contracts-bind",
            "  - \"contract:channel:orders\"\nlinks",
            "  - \"contract:channel:x\"\nlinks",
        );
        let report = run_case(&testfmt::registry, &dir).unwrap();
        assert!(!report.passed());
        assert_eq!(report.missing_contracts, ["contract:channel:x"]);
    }

    #[test]
    fn produced_by_asserted() {
        let report = run_case(&testfmt::registry, &case("contracts-profile")).unwrap();
        assert!(report.passed(), "{}", report.render());
        let (_t, dir) = flipped(
            "contracts-profile",
            "profile:example#bind-channel",
            "profile:example#other",
        );
        let report = run_case(&testfmt::registry, &dir).unwrap();
        assert!(!report.passed());
        assert!(
            report
                .render()
                .contains("produced_by=profile:example#other")
        );
    }

    #[test]
    fn contract_mismatch_reported() {
        let dir = Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/contract-mismatch");
        let report = run_case(&testfmt::registry, &dir).unwrap();
        assert!(!report.passed());
        assert_eq!(report.missing_contracts, ["contract:channel:other"]);
        assert_eq!(report.unexpected_contracts, ["contract:channel:orders"]);
        let text = report.render();
        assert!(
            text.contains("missing contract: contract:channel:other"),
            "{text}"
        );
        assert!(
            text.contains("unexpected contract: contract:channel:orders"),
            "{text}"
        );
    }
}
