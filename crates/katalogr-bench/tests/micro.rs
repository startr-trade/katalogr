//! R-11 (FEAT-068): parsing the bench lines, the median of runs and the proposed budget. No
//! corpus and no release build are needed.

mod micro {
    use katalogr_bench::micro::{
        MicroBench, Verdict, budget_from_measurement as propose, parse_lines, result_from_runs,
    };

    fn bench(id: &'static str, budget: f64, unit: &'static str) -> MicroBench {
        MicroBench {
            id,
            package: "p",
            test_target: "bench",
            test: "bench::t",
            budget,
            unit,
            item: "FEAT-000",
            req: "R-1",
        }
    }

    #[test]
    fn parses_bench_lines() {
        let out = "running 1 test\n\
            bench::x: 12 bytes in 3ms\n\
            katalogr-bench: a_ms 46.250 ms\n\
            katalogr-bench: b_ratio 1.2200 ratio\n\
            katalogr-bench: bad nan ms\n\
            katalogr-bench: bad_unit 3 s\n\
            katalogr-bench: short 3\n\
            katalogr-bench: extra 3 ms more\n\
            test result: ok\n";
        assert_eq!(
            parse_lines(out),
            vec![
                ("a_ms".to_string(), 46.25, "ms".to_string()),
                ("b_ratio".to_string(), 1.22, "ratio".to_string()),
            ]
        );
        assert!(parse_lines("").is_empty());
    }

    #[test]
    fn median_of_runs() {
        let b = bench("x", 60.0, "ms");
        let r = result_from_runs(&b, vec![70.0, 50.0, 55.0, 90.0, 52.0]);
        assert_eq!(r.median, 55.0);
        assert_eq!(r.verdict, Verdict::Met);
        assert_eq!(r.proposed_budget, None);
        assert_eq!(r.runs.len(), 5);
        // An even count takes the mean of the middle two.
        let r = result_from_runs(&b, vec![80.0, 60.0, 70.0, 100.0]);
        assert_eq!(r.median, 75.0);
        assert_eq!(r.verdict, Verdict::MissedSetFromMeasurement);
        assert_eq!(r.proposed_budget, Some(90.0));
    }

    #[test]
    fn budget_from_measurement() {
        assert_eq!(propose(46.0, "ms"), 55.0);
        assert_eq!(propose(1.22, "ratio"), 1.41);
        assert_eq!(propose(4120.0, "ms"), 4750.0);
        assert_eq!(propose(420.0, "ms"), 485.0);
        let r = result_from_runs(&bench("p", 30.0, "ms"), vec![46.0; 5]);
        assert_eq!(r.proposed_budget, Some(55.0));
        let r = result_from_runs(&bench("q", 1.10, "ratio"), vec![1.22; 3]);
        assert_eq!(r.proposed_budget, Some(1.41));
    }
}
