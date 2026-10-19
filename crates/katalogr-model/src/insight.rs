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
