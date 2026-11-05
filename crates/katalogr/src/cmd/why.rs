//! `katalogr why <file:line>`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options_for, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, location: &str, no_use: bool) -> Result<ExitCode, DistError> {
    let k = resolve(cli)?;
    let limit = (!no_use).then(|| k.why_use_limit());
    let opts = options_for(
        cli,
        "why",
        &[
            ("location", location.to_string()),
            ("use_limit", limit.map_or("none".to_string(), |n| n.to_string())),
        ],
    );
    let reply = k.why(location, limit, &opts)?;
    Ok(render::why(cli, reply))
}
