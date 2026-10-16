//! Test-only crash injection (feature `failpoints`; FEAT-047 section 8).

/// Abort the process when `KATALOGR_FAILPOINT` equals `name`.
pub fn hit(name: &str) {
    if std::env::var("KATALOGR_FAILPOINT").is_ok_and(|v| v == name) {
        std::process::abort();
    }
}
