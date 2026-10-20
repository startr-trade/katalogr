//! Neutral vocabulary shared by extensions, profiles and queries for structure insights
//! (FEAT-030, ADR-0018). The core names no language; an extension sets these facts.

/// Symbol or artifact fact: why the node is an entry point. A `Fact::Str` holding one of the
/// `ROOT_*` values (profiles may write any string; unknown values count as strong roots).
pub const FACT_ROOT: &str = "root";
/// Artifact fact: the package (build unit) the file belongs to, `Fact::Str`.
pub const FACT_PACKAGE: &str = "package";
/// Symbol or artifact fact: rationale markers, `Fact::List` of
/// `Fact::Map {marker: Str, line: Int, text: Str}`, in source order.
pub const FACT_RATIONALE: &str = "rationale";

/// Program entry.
pub const ROOT_MAIN: &str = "main";
/// Test, bench or test-only code.
pub const ROOT_TEST: &str = "test";
/// Called from outside the language (FFI, exported symbol, macro entry).
pub const ROOT_EXTERNAL: &str = "external";
/// Implementation reached through dispatch.
pub const ROOT_IMPL: &str = "impl";
/// Public API of a library; the only weak reason.
pub const ROOT_API: &str = "api";
/// Conventional name for profile-declared roots (registries).
pub const ROOT_DECLARED: &str = "declared";

/// Symbol or artifact fact: the node is test code, `Fact::Str` holding one of the `TEST_*`
/// roles (ADR-0018 amendment, FEAT-063).
pub const FACT_TEST: &str = "test";
/// A runnable test: one the runner executes and reports by name.
pub const TEST_CASE: &str = "case";
/// A grouping of tests: a test module, a describe block, or a whole test file (on the artifact).
pub const TEST_SUITE: &str = "suite";
/// Other code inside a test container: helpers, fixtures, setup and teardown hooks.
pub const TEST_SUPPORT: &str = "support";
/// Export namespace for test cases: the key is the name the runner prints, or a suffix of it.
pub const TEST_NAME_NS: &str = "test-name";
/// Link kind from a requirement (or any node) to a test case that verifies it.
pub const VERIFIED_BY: &str = "verified-by";

/// The `test-name` keys for a test whose full runner path is `segments`, joined with `::`:
/// the full path, then every proper suffix of at least two segments, longest first.
/// `["a","b","c"]` gives `["a::b::c", "b::c"]`; `["x"]` gives `["x"]`; `[]` gives `[]`.
pub fn test_name_keys(segments: &[&str]) -> Vec<String> {
    if segments.is_empty() {
        return Vec::new();
    }
    let mut keys = vec![segments.join("::")];
    for start in 1..segments.len().saturating_sub(1) {
        keys.push(segments[start..].join("::"));
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_full_path_then_suffixes() {
        assert_eq!(test_name_keys(&["a", "b", "c"]), vec!["a::b::c", "b::c"]);
        assert_eq!(test_name_keys(&["x"]), vec!["x"]);
        assert!(test_name_keys(&[]).is_empty());
        assert_eq!(test_name_keys(&["a", "b"]), vec!["a::b"]);
        assert_eq!(
            test_name_keys(&["a", "b", "c", "d"]),
            vec!["a::b::c::d", "b::c::d", "c::d"]
        );
    }
}
