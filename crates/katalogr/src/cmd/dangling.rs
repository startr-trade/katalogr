//! `katalogr dangling [--kind links-to|annotated-by|refs-item|contract]`.

use katalogr_dist::DistError;

use crate::cli::{Cli, DanglingKind};
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, kind: Option<DanglingKind>) -> Result<ExitCode, DistError> {
    let kinds: Vec<&str> = kind.map(|k| k.as_str()).into_iter().collect();
    let reply = resolve(cli)?.dangling(&kinds, &options(cli))?;
    Ok(render::dangling(cli, reply))
}
