mod metrics {
    use katalogr_analytics::metrics::{Better, MetricSample, Tolerance, compare, metric_key};

    fn s(name: &str, v: f64, b: Better) -> MetricSample {
        MetricSample::new(name, v, b)
    }

    #[test]
    fn compare_directions() {
        let zero = |_: &str| Tolerance { abs: 0.0, rel: 0.0 };
        let d = compare(
            &[s("h", 0.8, Better::Higher), s("h2", 0.8, Better::Higher)],
            &[s("h", 0.7, Better::Higher), s("h2", 0.9, Better::Higher)],
            &zero,
        );
        assert_eq!(d.regressions.len(), 1);
        assert_eq!(d.regressions[0].key, "h");
        assert_eq!(d.improvements.len(), 1);
        assert_eq!(d.improvements[0].key, "h2");

        let rel = |_: &str| Tolerance {
            abs: 0.0,
            rel: 0.10,
        };
        let d = compare(
            &[s("t", 100.0, Better::Lower), s("u", 100.0, Better::Lower)],
            &[s("t", 109.0, Better::Lower), s("u", 111.0, Better::Lower)],
            &rel,
        );
        assert_eq!(d.unchanged, 1);
        assert_eq!(d.regressions.len(), 1);
        assert_eq!(d.regressions[0].key, "u");

        let d = compare(
            &[s("i", 1.0, Better::Info), s("gone", 1.0, Better::Higher)],
            &[s("i", 5.0, Better::Info), s("new", 1.0, Better::Higher)],
            &zero,
        );
        assert_eq!(d.unchanged, 1);
        assert!(d.regressions.is_empty() && d.improvements.is_empty());
        assert_eq!(d.added, vec!["new".to_string()]);
        assert_eq!(d.removed, vec!["gone".to_string()]);

        let k = s("x", 1.0, Better::Info).label("b", "2").label("a", "1");
        assert_eq!(metric_key(&k), "x{a=1,b=2}");
        assert_eq!(metric_key(&s("x", 1.0, Better::Info)), "x");
        assert_eq!(s("x", 0.123_456_789, Better::Info).value, 0.123457);
    }
}
