//! The katalogr load-test harness (FEAT-068, ADR-0026).
//!
//! It drives the release `katalogr` binary as child processes over pinned corpora kept in a
//! workspace outside any katalogr checkout, and turns the measurements into
//! `katalogr_analytics::metrics` samples, a deterministic report and baseline comparisons.
//! The crate is not published and not shipped (`archtest::bench_not_shipped`).

pub mod baseline;
pub mod cli;
pub mod disk;
pub mod error;
pub mod fetch;
pub mod git;
pub mod idle;
pub mod katalogr;
pub mod machine;
pub mod manifest;
pub mod measure;
pub mod micro;
pub mod proc;
pub mod report;
pub mod samples;
pub mod sizes;
pub mod stats;
pub mod workload;
pub mod workspace;

pub use error::BenchError;

/// Tests that the requirements name by module path (`discovery::`, `query::`); the code they
/// test lives under `measure::`.
#[cfg(test)]
mod discovery {
    use crate::measure::discovery::from_coverage;

    #[test]
    fn baseline_from_coverage() {
        let cov = serde_json::json!({"schema": 1, "result": {
            "totals": {"files_indexed": 7, "files_unclaimed": 5},
            "by_extension": [
                {"repo": null, "extension": "text", "language": null, "counts": {"files_indexed": 4}},
                {"repo": null, "extension": "md", "language": null, "counts": {"files_indexed": 3}}
            ],
            "unclaimed": [
                {"repo": "a", "files": 3, "suffixes": [{"suffix": ".c", "files": 2}, {"suffix": ".h", "files": 1}]},
                {"repo": "b", "files": 2, "suffixes": [{"suffix": ".c", "files": 1}, {"suffix": ".cc", "files": 1}]}
            ]
        }});
        let d = from_coverage(&cov, Some(2.0));
        assert_eq!(
            (d.files_indexed, d.files_unclaimed, d.files_seen),
            (7, 5, 12)
        );
        assert_eq!(d.text_files, 4);
        assert_eq!(d.unclaimed_per_s, Some(6.0));
        assert_eq!(
            d.by_suffix,
            vec![
                (".c".to_string(), 3),
                (".cc".to_string(), 1),
                (".h".to_string(), 1)
            ]
        );
        // A claimed non-text extension takes the unclaimed-throughput figure away.
        let mut other = cov.clone();
        other["result"]["by_extension"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"extension": "rust", "counts": {"files_indexed": 2}}));
        assert_eq!(from_coverage(&other, Some(2.0)).unclaimed_per_s, None);
        assert_eq!(from_coverage(&cov, None).unclaimed_per_s, None);
    }
}

#[cfg(test)]
mod query {
    use crate::measure::query::{Outcome, classify};

    #[test]
    fn outcome_classification() {
        assert_eq!(classify(Some(0), b"{}"), Outcome::Ok);
        assert_eq!(
            classify(Some(0), b"plain text of session-start"),
            Outcome::Ok
        );
        assert_eq!(classify(Some(3), b"{}"), Outcome::Ok);
        assert_eq!(classify(Some(4), b"{}"), Outcome::NoMatch);
        // Gate commands exit 1 on findings.
        assert_eq!(
            classify(Some(1), br#"{"schema":1,"healthy":false}"#),
            Outcome::Ok
        );
        match classify(
            Some(1),
            br#"{"message":"symbols[7]: absolute path refused"}"#,
        ) {
            Outcome::Error(m) => assert!(m.contains("absolute path refused"), "{m}"),
            o => panic!("{o:?}"),
        }
        assert!(matches!(classify(Some(1), b"not json"), Outcome::Error(_)));
        assert!(matches!(
            classify(Some(2), br#"{"message":"usage"}"#),
            Outcome::Error(_)
        ));
        assert!(matches!(classify(Some(5), b""), Outcome::Error(_)));
        assert!(matches!(classify(None, b"{}"), Outcome::Error(_)));
        // Messages are cut at 200 bytes.
        let long = format!(r#"{{"message":"{}"}}"#, "x".repeat(500));
        match classify(Some(1), long.as_bytes()) {
            Outcome::Error(m) => assert_eq!(m.len(), 200),
            o => panic!("{o:?}"),
        }
    }
}
