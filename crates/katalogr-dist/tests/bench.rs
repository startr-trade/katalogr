//! Evaluation benchmarks (FEAT-061 R-11). Ignored; run in release:
//! `cargo test -p katalogr-dist --release --test bench -- --ignored --nocapture`.

mod bench {
    use katalogr_dist::eval::{EvalRequest, parse_sweep, run_fixtures, sweep};
    use std::path::Path;
    use std::time::Instant;

    fn request() -> EvalRequest {
        EvalRequest {
            sets: vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/eval")],
            ..EvalRequest::default()
        }
    }

    #[test]
    #[ignore]
    fn eval_fixture_suite() {
        let t = Instant::now();
        let r = run_fixtures(&request()).unwrap();
        let ms = t.elapsed().as_millis();
        println!(
            "eval_fixture_suite: {ms} ms for {} queries",
            r.queries.len()
        );
        assert!(ms < 5000, "{ms} ms");
    }

    #[test]
    #[ignore]
    fn eval_sweep_16() {
        let grid = [
            parse_sweep("search.k1=1.0,1.2,1.5,2.0").unwrap(),
            parse_sweep("search.b=0.5,0.6,0.75,0.9").unwrap(),
        ];
        let t = Instant::now();
        let r = sweep(&request(), &grid, "retrieval.ndcg@10").unwrap();
        let ms = t.elapsed().as_millis();
        println!("eval_sweep_16: {ms} ms for {} combinations", r.rows.len());
        assert_eq!(r.rows.len(), 16);
        assert!(ms < 10000, "{ms} ms");
    }
}
