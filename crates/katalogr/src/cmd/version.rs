//! The version string shown by `katalogr --version`.

/// `katalogr <semver> (<sha12> <yyyy-mm-dd>)`, with `unknown` for a missing sha or date.
pub fn version_string() -> String {
    format!(
        "katalogr {} ({} {})",
        env!("CARGO_PKG_VERSION"),
        env!("KATALOGR_BUILD_SHA"),
        env!("KATALOGR_BUILD_DATE"),
    )
}
