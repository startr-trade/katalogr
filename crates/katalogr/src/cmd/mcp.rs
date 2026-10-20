//! `katalogr mcp`: serve the core queries over MCP (stdio) for non-Claude agent hosts.

use std::time::Duration;

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options, overrides};
use crate::exit::ExitCode;
use crate::mcp::{self, ServeConfig};

/// Serve MCP on stdin/stdout until stdin closes. Exit 0 when the session ends normally; exit 1
/// (`katalogr: mcp: <e>` on stderr) if the runtime cannot be built, the handshake fails, or
/// the service task panics. The project is not resolved up front: a misconfigured project is
/// reported per call, so the host still lists the tools.
pub fn run(
    cli: &Cli,
    root: Option<&str>,
    output_schemas: bool,
    progress_ms: u64,
) -> Result<ExitCode, DistError> {
    let cwd =
        std::env::current_dir().map_err(|e| DistError::Usage(format!("current directory: {e}")))?;
    let root = match root {
        Some(r) => cwd.join(r),
        None => cwd,
    };
    let cfg = ServeConfig {
        root,
        overrides: overrides(cli),
        options: options(cli),
        budget: cli.budget,
        output_schemas,
        progress: Duration::from_millis(progress_ms.max(10)),
    };
    match mcp::serve_stdio(cfg) {
        Ok(()) => Ok(ExitCode::Ok),
        Err(e) => {
            eprintln!("katalogr: mcp: {e}");
            Ok(ExitCode::Error)
        }
    }
}
