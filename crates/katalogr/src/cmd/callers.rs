//! `katalogr callers <text> [--depth N]`.

use katalogr_dist::{DistError, Options};

use crate::cli::Cli;
use crate::cmd::{options_for, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, text: &str, depth: u32, expand: bool) -> Result<ExitCode, DistError> {
    let opts = Options {
        expand,
        ..options_for(
            cli,
            "callers",
            &[
                ("text", text.to_string()),
                ("depth", depth.to_string()),
                ("expand", expand.to_string()),
            ],
        )
    };
    let reply = resolve(cli)?.callers(text, depth, &opts)?;
    Ok(render::list_folded(
        cli,
        reply,
        render::caller_line,
        "callers",
    ))
}
