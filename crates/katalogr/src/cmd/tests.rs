//! `katalogr tests <text> [--depth N] [--cases]`.

use katalogr_dist::{DistError, query::TestsOptions};

use crate::cli::Cli;
use crate::cmd::{options_for, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, text: &str, depth: u32, cases: bool) -> Result<ExitCode, DistError> {
    let topts = TestsOptions {
        depth,
        cases_only: cases,
    };
    let reply = resolve(cli)?.tests(
        text,
        topts,
        &options_for(
            cli,
            "tests",
            &[
                ("text", text.to_string()),
                ("depth", depth.to_string()),
                ("cases", cases.to_string()),
            ],
        ),
    )?;
    Ok(render::list(cli, reply, render::test_line, "tests"))
}
