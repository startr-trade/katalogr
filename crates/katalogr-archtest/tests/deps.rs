//! Dependency floors set by security advisories (FEAT-111).

mod ci {
    use std::path::Path;

    /// The lowest version of each crate that has no known advisory against the code katalogr ships.
    /// gix 0.90.0 fixes GHSA-q54r-jf2p-rfr2 (a repository's own config could turn off the `.git`
    /// path-confusion checks).
    const FLOORS: &[(&str, (u64, u64, u64))] = &[("gix", (0, 90, 0))];

    fn parse(v: &str) -> (u64, u64, u64) {
        let core = v.split(['-', '+']).next().unwrap_or(v);
        let mut it = core.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
        (
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
        )
    }

    /// (name, version) of every package in Cargo.lock.
    fn locked() -> Vec<(String, String)> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let lock = std::fs::read_to_string(root.join("Cargo.lock")).expect("read Cargo.lock");
        let mut out = Vec::new();
        let mut name: Option<String> = None;
        for line in lock.lines() {
            let line = line.trim();
            if line == "[[package]]" {
                name = None;
            } else if let Some(n) = line.strip_prefix("name = ") {
                name = Some(n.trim_matches('"').to_string());
            } else if let Some(v) = line.strip_prefix("version = ")
                && let Some(n) = name.take()
            {
                out.push((n, v.trim_matches('"').to_string()));
            }
        }
        out
    }

    // Test ci::gix_not_vulnerable (FEAT-111 R-1)
    #[test]
    fn gix_not_vulnerable() {
        let pkgs = locked();
        let mut failures = Vec::new();
        for (crate_name, floor) in FLOORS {
            let found: Vec<_> = pkgs.iter().filter(|(n, _)| n == crate_name).collect();
            assert!(!found.is_empty(), "{crate_name} is not in Cargo.lock");
            for (_, v) in found {
                if parse(v) < *floor {
                    failures.push(format!(
                        "{crate_name} {v} is below the advisory floor {}.{}.{}",
                        floor.0, floor.1, floor.2
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
