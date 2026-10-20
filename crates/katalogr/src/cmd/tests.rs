//! `katalogr tests <text> [--depth N] [--cases]`.

use katalogr_dist::{DistError, query::TestsOptions};

use crate::cli::Cli;
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, text: &str, depth: u32, cases: bool) -> Result<ExitCode, DistError> {
    let topts = TestsOptions {
        depth,
        cases_only: cases,
    };
    let reply = resolve(cli)?.tests(text, topts, &options(cli))?;
    Ok(render::list(cli, reply, render::test_line, "tests"))
}
