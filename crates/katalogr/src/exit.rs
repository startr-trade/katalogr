//! Process exit codes (the stable contract for scripts and agents).

/// Exit codes returned by the katalogr CLI.
#[allow(dead_code)] // the non-zero variants are produced by later commands
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// Success.
    Ok = 0,
    /// Any other error.
    Error = 1,
    /// Bad command line or usage.
    Usage = 2,
    /// A query matched more than one item.
    Ambiguous = 3,
    /// A query matched nothing.
    NoMatch = 4,
    /// The index was still stale when `--timeout` elapsed.
    Stale = 5,
}

impl ExitCode {
    /// The numeric process exit status.
    pub fn code(self) -> i32 {
        self as i32
    }
}
