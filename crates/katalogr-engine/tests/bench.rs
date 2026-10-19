//! FEAT-059 R-8: exporting contract nodes costs little.
mod support;

use camino::Utf8PathBuf;
use std::time::{Duration, Instant};
use support::{drained_graph, full_index, init_project, open_engine};

fn median_export(with_contracts: bool) -> (Duration, usize) {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let mut text = String::new();
    for i in 0..1000 {
        text.push_str(&format!("sym fn f{i}\n"));
    }
    for i in 0..1000 {
        if with_contracts {
            text.push_str(&format!("bind f{i} provides channel:c{i}\n"));
        }
    }
    std::fs::write(work.join("big.tx"), text).unwrap();
    let kx = root.join("kx");
    init_project(&kx);
    let e = open_engine(&kx, &work);
    full_index(&e);
    let mut times: Vec<Duration> = (0..9)
        .map(|_| {
            let t = Instant::now();
            let g = drained_graph(&e);
            std::hint::black_box(&g);
            t.elapsed()
        })
        .collect();
    times.sort();
    let contracts = drained_graph(&e).matches(r#""node":"contract""#).count();
    (times[4], contracts)
}

mod bench {
    use super::*;

    /// Both graphs have 1,000 symbols; one adds 1,000 bindings, each to its own contract.
    #[test]
    #[ignore = "benchmark: run in release with --ignored"]
    fn export_1000_contracts() {
        let (base, n1) = median_export(false); // same symbols, no bindings
        let (many, n2) = median_export(true);
        println!("no bindings: {base:?} ({n1} contract), 1000 contracts: {many:?} ({n2})");
        assert_eq!((n1, n2), (0, 1000));
        assert!(
            many <= base + Duration::from_millis(20),
            "{many:?} vs {base:?}"
        );
    }
}
