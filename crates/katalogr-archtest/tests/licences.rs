//! Licence declarations: every workspace package and the licence files (R-5).

mod ci {
    use std::path::Path;

    use cargo_metadata::MetadataCommand;

    const LICENCE: &str = "Apache-2.0 OR MIT";

    #[test]
    fn licences_declared() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let md = MetadataCommand::new()
            .current_dir(&root)
            .no_deps()
            .exec()
            .expect("cargo metadata");

        let mut failures = Vec::new();
        for p in md.workspace_packages() {
            if p.license.as_deref() != Some(LICENCE) {
                failures.push(format!("{} has license {:?}", p.name, p.license));
            }
        }
        assert!(
            failures.is_empty(),
            "workspace packages without `{LICENCE}`:\n{}",
            failures.join("\n")
        );

        for file in ["LICENSE-APACHE", "LICENSE-MIT"] {
            assert!(
                root.join(file).is_file(),
                "missing {file} at workspace root"
            );
        }
    }

    /// Runner labels that GitHub has retired must not appear in the release workflow (R-11).
    #[test]
    fn release_runners_current() {
        const RETIRED: [&str; 4] = ["macos-11", "macos-12", "macos-13", "macos-14"];
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/release.yml");
        let text = std::fs::read_to_string(&path).expect("read release.yml");
        let mut bad = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let t = line.trim_start().trim_start_matches("- ");
            if let Some(v) = t.strip_prefix("os:") {
                let v = v.split('#').next().unwrap_or("").trim().trim_matches('"');
                if RETIRED.contains(&v) {
                    bad.push(format!("release.yml:{}: retired runner `{v}`", n + 1));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}
