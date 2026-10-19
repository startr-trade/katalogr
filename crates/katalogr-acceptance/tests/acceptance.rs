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
}
