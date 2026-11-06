//! FEAT-032 R-13 benchmarks for `render`. Both are `#[ignore]` tests: run them in release, one
//! at a time, under the shared heavy-build lock:
//! `flock ~/.cache/heavy-build.lock cargo test --release -p katalogr --test bench_render -- --ignored --nocapture --test-threads=1`
//! Each prints its numbers with the load average; record them in the component doc.

mod bench {
    use katalogr_dist::render::{ViewSpec, to_dot, to_graphml, to_mermaid, to_structurizr, to_svg};
    use katalogr_dist::{Katalogr, Options, Outcome, Overrides, kindex_init, project_init};
    use std::time::{Duration, Instant};

    /// The one-minute load average, for the line every timing prints.
    fn load() -> String {
        std::fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|s| s.split_whitespace().next().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// 100 files of 100 symbols (10,000 in all). Each file exports its first symbol and its
    /// second symbol calls the first symbol of the next three files, so the file graph is a ring
    /// with chords.
    fn corpus(dir: &std::path::Path) -> Katalogr {
        let kx = dir.join("kx");
        let repo = dir.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        for i in 0..100 {
            let mut text = String::new();
            for j in 0..100 {
                text.push_str(&format!("sym fn s{j}\n"));
            }
            text.push_str(&format!("export x:m{i} s0\n"));
            for step in [1, 2, 3] {
                text.push_str(&format!("ref s1 calls x:m{}\n", (i + step) % 100));
            }
            std::fs::write(repo.join(format!("m{i}.tx")), text).unwrap();
        }
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), repo.clone())], true).unwrap();
        let k = Katalogr::resolve(
            &repo,
            &Overrides {
                kindex: Some(kx.display().to_string()),
                project: None,
                context: None,
                snapshot_dirs: Vec::new(),
            },
        )
        .unwrap();
        k.index(None, true, None, &Options::default()).unwrap();
        k
    }

    fn best_of<T>(runs: usize, mut f: impl FnMut() -> T) -> (Duration, T) {
        let mut best = Duration::MAX;
        let mut last = None;
        for _ in 0..runs {
            let t = Instant::now();
            let v = f();
            best = best.min(t.elapsed());
            last = Some(v);
        }
        (best, last.unwrap())
    }

    /// R-13: on 10,000 symbols, drawing a structure view costs at most 200 ms on top of the
    /// read that builds it. Times the read (`view_graph`) and each writer separately.
    #[test]
    #[ignore = "benchmark: run in release with --ignored"]
    fn render_structure_10k_symbols() {
        let tmp = tempfile::tempdir().unwrap();
        let k = corpus(tmp.path());
        let opts = Options::default();
        for line in [
            "structure --level file",
            "structure --level file --max-nodes 100",
        ] {
            let spec = ViewSpec::parse(line).unwrap();
            let (read, reply) = best_of(5, || k.view_graph(&spec, &opts).unwrap());
            let Outcome::Found(g) = reply.outcome else {
                panic!("{line}: no graph")
            };
            let (mermaid, _) = best_of(5, || to_mermaid(&g));
            let (dot, _) = best_of(5, || to_dot(&g));
            let (graphml, _) = best_of(5, || to_graphml(&g));
            let (structurizr, _) = best_of(5, || to_structurizr(&g));
            let (svg, _) = best_of(3, || to_svg(&g).unwrap());
            eprintln!(
                "BENCH render {line:?}: {} nodes of {}, {} edges; read {read:?}; mermaid {mermaid:?}, dot {dot:?}, graphml {graphml:?}, structurizr {structurizr:?}, svg {svg:?}; load {}",
                g.shown,
                g.total,
                g.edges.len(),
                load()
            );
            let over = mermaid.max(dot).max(graphml).max(structurizr);
            assert!(
                over <= Duration::from_millis(200),
                "{line}: a text writer took {over:?} (budget 200 ms over the read)"
            );
        }
    }

    /// R-13: the binary grows by at most the size of `pele.global.js` plus 300 KiB. The growth
    /// needs a binary built without this item: give its path in `KATALOGR_BASELINE_BIN`.
    /// Without it, the sizes are printed and nothing is asserted.
    #[test]
    #[ignore = "benchmark: run in release with --ignored"]
    fn render_binary_size() {
        let bin = std::path::Path::new(env!("CARGO_BIN_EXE_katalogr"));
        let size = std::fs::metadata(bin).unwrap().len();
        let pele = katalogr_dist::render::PELE_JS.len() as u64;
        let assets = pele
            + (katalogr_dist::render::PELE_INIT_JS.len()
                + katalogr_dist::render::PELE_THEME_CSS.len()) as u64;
        eprintln!(
            "BENCH binary {size} bytes ({:.2} MiB); pele.global.js {pele} bytes, all assets {assets} bytes; load {}",
            size as f64 / 1_048_576.0,
            load()
        );
        let Some(base) = std::env::var_os("KATALOGR_BASELINE_BIN") else {
            eprintln!("BENCH binary: no KATALOGR_BASELINE_BIN, growth not measured");
            return;
        };
        let base_size = std::fs::metadata(&base).unwrap().len();
        let growth = size.saturating_sub(base_size);
        let budget = pele + 300 * 1024;
        eprintln!(
            "BENCH binary growth {growth} bytes ({:.2} MiB) over a {base_size}-byte baseline; budget {budget} bytes, style budget {} bytes",
            growth as f64 / 1_048_576.0,
            3.5 * 1_048_576.0
        );
        assert!(
            growth <= budget,
            "growth {growth} is over the budget {budget}"
        );
    }
}
