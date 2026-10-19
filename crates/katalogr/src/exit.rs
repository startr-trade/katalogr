//! Process exit codes (the stable contract for scripts and agents).

use katalogr_dist::DistError;

/// Exit codes returned by the katalogr CLI.
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

    /// Print the error and pick its status.
    pub fn from_error(e: &DistError) -> ExitCode {
        eprintln!("katalogr: {e}");
        if e.is_usage() {
            ExitCode::Usage
        } else {
            ExitCode::Error
        }
    }
}
