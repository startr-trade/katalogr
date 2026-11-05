//! The build identity stored answers are valid for (FEAT-096 R-5, ADR-0037).
//!
//! A binary built from a clean checkout is named by its commit. A dirty or unstamped build has no
//! identity, so it never stores or serves an answer. `KATALOGR_ANSWER_BUILD` overrides this for
//! tests: non-empty is the build, empty is none.

/// The rule, with its inputs spelled out. `env` is `KATALOGR_ANSWER_BUILD`; `sha` and `dirty` are
/// the build script's `KATALOGR_BUILD_SHA` and `KATALOGR_BUILD_DIRTY`.
pub fn answer_build_from(env: Option<&str>, sha: &str, dirty: &str) -> Option<String> {
    match env {
        Some("") => None,
        Some(v) => Some(v.to_string()),
        None => (sha != "unknown" && dirty == "0").then(|| sha.to_string()),
    }
}

/// This binary's build identity, or `None` to turn stored answers off.
// Called by the command layer once FEAT-096 T13 lands.
#[allow(dead_code)]
pub fn current() -> Option<String> {
    answer_build_from(
        std::env::var("KATALOGR_ANSWER_BUILD").ok().as_deref(),
        env!("KATALOGR_BUILD_SHA"),
        env!("KATALOGR_BUILD_DIRTY"),
    )
}

#[cfg(test)]
mod tests {
    use super::answer_build_from;

    #[test]
    fn rules() {
        // The variable set: it is the build, whatever the stamp says.
        assert_eq!(
            answer_build_from(Some("test"), "unknown", "1"),
            Some("test".into())
        );
        // The variable empty: no build.
        assert_eq!(answer_build_from(Some(""), "abc123", "0"), None);
        // Unset, unknown sha: none.
        assert_eq!(answer_build_from(None, "unknown", "0"), None);
        // Unset, dirty: none.
        assert_eq!(answer_build_from(None, "abc123", "1"), None);
        // Unset, clean: the sha.
        assert_eq!(
            answer_build_from(None, "abc123", "0"),
            Some("abc123".into())
        );
    }
}
