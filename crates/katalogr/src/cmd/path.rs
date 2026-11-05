//! `katalogr path <from> <to>`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::cmd::{options_for, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, from: &str, to: &str, max_len: u32) -> Result<ExitCode, DistError> {
    let reply = resolve(cli)?.path(
        from,
        to,
        max_len,
        &options_for(
            cli,
            "path",
            &[
                ("from", from.to_string()),
                ("to", to.to_string()),
                ("max_len", max_len.to_string()),
            ],
        ),
    )?;
    Ok(render::list(cli, reply, render::path_line, "path"))
}
