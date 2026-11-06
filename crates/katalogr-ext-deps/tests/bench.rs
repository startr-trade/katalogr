//! Timing benchmark, run on demand (FEAT-007 R-10):
//! `flock ~/.cache/heavy-build.lock cargo test -p katalogr-ext-deps --release --test bench -- --ignored --nocapture`.

use katalogr_ext_deps::DepsExtractor;
use katalogr_model::{ExtractInput, Extractor, RelPath};
use std::time::{Duration, Instant};

const N: usize = 20_000;

fn load() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn cargo_lock() -> String {
    let mut s = String::from("version = 3\n");
    for i in 0..N {
        s.push_str(&format!(
            "\n[[package]]\nname = \"pkg-{i}\"\nversion = \"1.{}.0\"\nsource = \"registry+https://registry.example/index\"\ndependencies = [\n \"pkg-{}\",\n]\n",
            i % 7,
            (i + 1) % N
        ));
    }
    s
}

fn npm_lock() -> String {
    let mut s = String::from(
        "{\n  \"lockfileVersion\": 3,\n  \"packages\": {\n    \"\": {\n      \"name\": \"root-app\"\n    }",
    );
    for i in 0..N {
        s.push_str(&format!(
            ",\n    \"node_modules/pkg-{i}\": {{\n      \"version\": \"1.{}.0\",\n      \"resolved\": \"https://registry.example/pkg-{i}.tgz\",\n      \"dependencies\": {{\n        \"pkg-{}\": \"^1\"\n      }}\n    }}",
            i % 7,
            (i + 1) % N
        ));
    }
    s.push_str("\n  }\n}\n");
    s
}

fn pnpm_lock() -> String {
    let mut s = String::from(
        "lockfileVersion: '9.0'\n\nimporters:\n\n  .:\n    dependencies:\n      pkg-0:\n        specifier: ^1\n        version: 1.0.0\n\npackages:\n",
    );
    for i in 0..N {
        s.push_str(&format!(
            "\n  pkg-{i}@1.{}.0:\n    resolution: {{integrity: sha512-x, tarball: https://registry.example/pkg-{i}.tgz}}\n",
            i % 7
        ));
    }
    s.push_str("\nsnapshots:\n");
    for i in 0..N {
        s.push_str(&format!(
            "\n  pkg-{i}@1.{}.0:\n    dependencies:\n      pkg-{}: 1.{}.0\n",
            i % 7,
            (i + 1) % N,
            (i + 1) % N % 7
        ));
    }
    s
}

mod bench {
    use super::*;

    /// R-10. A synthetic lockfile of 20,000 entries, each format, extracted. Budget: 500 ms.
    /// Also reports the graph added per entry (symbols, links and encoded shard bytes).
    #[test]
    #[ignore]
    fn deps_extract_20k_entries() {
        for (file, text) in [
            ("Cargo.lock", cargo_lock()),
            ("package-lock.json", npm_lock()),
            ("pnpm-lock.yaml", pnpm_lock()),
        ] {
            let path = RelPath::new(file).unwrap();
            let input = ExtractInput {
                repo: "r",
                path: &path,
                revision: "ce013625030ba8dba906f756967f9e9ca394464a",
                content: text.as_bytes(),
                context: &[],
            };
            let mut best = Duration::MAX;
            let mut shape = (0, 0, 0);
            for _ in 0..5 {
                let start = Instant::now();
                let (f, _errors) = DepsExtractor.extract_with_errors(&input).unwrap();
                best = best.min(start.elapsed());
                let bytes = katalogr_store::shard::encode(&f).unwrap().len();
                shape = (f.symbols.len(), f.links.len(), bytes);
            }
            println!(
                "bench::deps_extract_20k_entries {file}: {} bytes in {best:?} (load {}); {} symbols, {} links, {} shard bytes: {:.1} symbols, {:.1} links, {:.0} bytes per entry",
                text.len(),
                load(),
                shape.0,
                shape.1,
                shape.2,
                shape.0 as f64 / N as f64,
                shape.1 as f64 / N as f64,
                shape.2 as f64 / N as f64,
            );
            assert!(best < Duration::from_millis(500), "{file} took {best:?}");
        }
    }
}
