//! `katalogr refs <text>`.

use katalogr_dist::{DistError, Options};

use crate::cli::Cli;
use crate::cmd::{options_for_diag, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, text: &str, diagnostic: bool, expand: bool) -> Result<ExitCode, DistError> {
    let opts = Options {
        expand,
        ..options_for_diag(
            cli,
            "refs",
            &[("text", text.to_string()), ("expand", expand.to_string())],
            diagnostic,
        )
    };
    let reply = resolve(cli)?.refs(text, &opts)?;
    Ok(render::list_folded(cli, reply, render::ref_line, "refs"))
}
