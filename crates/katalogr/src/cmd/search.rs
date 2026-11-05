//! `katalogr search <words...>`.

use katalogr_dist::{DistError, Options};

use crate::cli::Cli;
use crate::cmd::{options_for_diag, ranking, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(
    cli: &Cli,
    terms: &[String],
    limit: usize,
    diagnostic: bool,
    neutral: bool,
) -> Result<ExitCode, DistError> {
    let reply = resolve(cli)?.search(
        &terms.join(" "),
        limit,
        &Options {
            ranking: ranking(neutral),
            ..options_for_diag(
                cli,
                "search",
                &[("terms", terms.join(" ")), ("limit", limit.to_string())],
                diagnostic,
            )
        },
    )?;
    Ok(render::list(cli, reply, render::search_line, "search"))
}
