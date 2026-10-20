//! The optional MCP wrapper for non-Claude agent hosts (ADR-0023, FEAT-067).
//!
//! `katalogr mcp` serves the core queries over stdio as MCP tools. It holds no query logic:
//! each tool makes one `katalogr-dist` call and renders it with the CLI's JSON renderer.

pub mod server;
pub mod tools;
pub mod transport;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use katalogr_dist::{Options, Overrides};
use rmcp::ServiceExt;

use self::server::KatalogrMcp;
use self::transport::{CappedLines, MAX_LINE_BYTES};

/// Everything a session needs; shared by the handler behind an `Arc`.
#[derive(Debug, Clone)]
pub struct ServeConfig {
    /// Directory the project is resolved from (the `.katalogr` marker walk).
    pub root: PathBuf,
    /// `--kindex` / `--project` passed through.
    pub overrides: Overrides,
    /// Server-wide `--consistency` / `--timeout`.
    pub options: Options,
    /// Server-wide `--budget`; a tool's `budget` argument overrides it.
    pub budget: Option<u64>,
    /// Include `outputSchema` (the `schema/v1` file) in `tools/list`. Default false.
    pub output_schemas: bool,
    /// Progress heartbeat interval. Default 1000 ms.
    pub progress: Duration,
}

/// Build the runtime, serve MCP on stdin/stdout until the session ends, wait for a running
/// facade call to finish, and shut the runtime down (1 s timeout, for a stdin read that is
/// still blocked).
pub fn serve_stdio(cfg: ServeConfig) -> io::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .max_blocking_threads(4)
        .thread_name("katalogr-mcp")
        .build()?;
    let result = rt.block_on(serve(cfg, tokio::io::stdin(), tokio::io::stdout()));
    rt.shutdown_timeout(Duration::from_secs(1));
    result
}

/// Serve over any async reader and writer: the testable core of [`serve_stdio`].
pub async fn serve<R, W>(cfg: ServeConfig, input: R, output: W) -> io::Result<()>
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
    W: tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    let server = KatalogrMcp::new(cfg);
    let running = server
        .clone()
        .serve((CappedLines::new(input, MAX_LINE_BYTES), output))
        .await
        .map_err(io::Error::other)?;
    let ended = running.waiting().await;
    server.drain().await;
    match ended {
        Ok(_) => Ok(()),
        Err(e) => Err(io::Error::other(e)),
    }
}
