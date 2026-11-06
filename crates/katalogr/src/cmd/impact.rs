//! `katalogr impact [SYMBOL] [--file PATH | --range BASE..HEAD | --since REV | --item CODE]
//! [--depth N] [--limit N] [--test-depth N]`.

use katalogr_dist::query::ImpactOptions;
use katalogr_dist::{DistError, ImpactInput};

use crate::cli::Cli;
use crate::cmd::{options_for, resolve};
use crate::exit::ExitCode;
use crate::render;

/// The ways to say where impact starts; at most one may be given.
pub struct Input<'a> {
    pub symbol: Option<&'a str>,
    pub file: Option<&'a str>,
    pub range: Option<&'a str>,
    pub since: Option<&'a str>,
    pub item: Option<&'a str>,
}

impl<'a> Input<'a> {
    /// The facade input, or a usage error when more than one way is given (or `--range` is
    /// combined with `--at`). None given means the changes since the merge-base.
    pub fn parse(&self, at: Option<&str>) -> Result<ImpactInput<'a>, DistError> {
        let given = [
            self.symbol.is_some(),
            self.file.is_some(),
            self.range.is_some(),
            self.since.is_some(),
            self.item.is_some(),
        ]
        .iter()
        .filter(|g| **g)
        .count();
        if given > 1 {
            return Err(DistError::Usage(
                "give at most one of SYMBOL, --file, --range, --since and --item".to_string(),
            ));
        }
        if self.range.is_some() && at.is_some() {
            return Err(DistError::Usage(
                "--range names its own position: it cannot be combined with --at".to_string(),
            ));
        }
        Ok(if let Some(s) = self.symbol {
            ImpactInput::Symbol(s)
        } else if let Some(f) = self.file {
            ImpactInput::File(f)
        } else if let Some(r) = self.range {
            ImpactInput::Range(r)
        } else if let Some(i) = self.item {
            ImpactInput::Item(i)
        } else {
            ImpactInput::Since(self.since)
        })
    }
}

pub fn run(
    cli: &Cli,
    input: &Input<'_>,
    depth: u32,
    limit: usize,
    test_depth: u32,
) -> Result<ExitCode, DistError> {
    let facade_input = input.parse(cli.at.as_deref())?;
    let iopts = ImpactOptions {
        depth,
        limit,
        test_depth,
    };
    let reply = resolve(cli)?.impact(
        &facade_input,
        &iopts,
        &options_for(cli, "impact", &args(input, depth, limit, test_depth)),
    )?;
    Ok(render::impact(cli, reply))
}

/// The view ID arguments of a question; the MCP tool builds the same ones.
pub fn args(
    input: &Input<'_>,
    depth: u32,
    limit: usize,
    test_depth: u32,
) -> Vec<(&'static str, String)> {
    let text = if let Some(s) = input.symbol {
        format!("symbol:{s}")
    } else if let Some(f) = input.file {
        format!("file:{f}")
    } else if let Some(r) = input.range {
        format!("range:{r}")
    } else if let Some(i) = input.item {
        format!("item:{i}")
    } else {
        format!("since:{}", input.since.unwrap_or(""))
    };
    vec![
        ("input", text),
        ("depth", depth.to_string()),
        ("limit", limit.to_string()),
        ("test-depth", test_depth.to_string()),
    ]
}
