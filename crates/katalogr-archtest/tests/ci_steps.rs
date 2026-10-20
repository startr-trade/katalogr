//! The CI workflow keeps the checks that guard the optional MCP wrapper (FEAT-067).

mod ci {
    use std::path::Path;

    /// R-14. The `clippy` job lints the `katalogr` crate without default features, so a build
    /// without the `mcp` feature (no tokio, no rmcp) cannot rot.
    #[test]
    fn builds_without_mcp_feature() {
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/ci.yml"),
        )
        .expect("ci.yml");
        let step =
            "cargo clippy -p katalogr --no-default-features --all-targets --locked -- -D warnings";
        assert!(
            text.lines().any(|l| l.trim() == format!("run: {step}")),
            "ci.yml has no step `{step}`"
        );
    }
}
