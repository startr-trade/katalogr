//! `katalogr api`: the stateless HTTP/JSON API server (FEAT-049).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use katalogr_dist::{DistError, Katalogr};

use crate::api::guard::Localhost;
use crate::api::{ApiConfig, DEFAULT_QUEUE, default_workers};
use crate::cli::Cli;
use crate::cmd::overrides;
use crate::exit::ExitCode;

/// Serve until the process is stopped. A project that cannot be resolved is a usage error
/// before anything binds.
pub fn run(
    cli: &Cli,
    listen: SocketAddr,
    workers: Option<usize>,
    queue: Option<usize>,
    no_process: bool,
    cors_origin: Option<String>,
) -> Result<ExitCode, DistError> {
    let start_dir =
        std::env::current_dir().map_err(|e| DistError::Usage(format!("current directory: {e}")))?;
    let overrides = overrides(cli);
    Katalogr::resolve(&start_dir, &overrides).map_err(|e| DistError::Usage(e.to_string()))?;
    let cfg = ApiConfig {
        listen,
        workers: workers.unwrap_or_else(default_workers),
        queue: queue.unwrap_or(DEFAULT_QUEUE),
        process: !no_process,
        cors_origin,
        overrides,
        start_dir,
    };
    crate::api::run_announcing(cfg, Arc::new(Localhost), &AtomicBool::new(false))?;
    Ok(ExitCode::Ok)
}
