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
}
