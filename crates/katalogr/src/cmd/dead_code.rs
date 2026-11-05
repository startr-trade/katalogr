//! `katalogr dead-code [--internal] [--min-confidence C] [--path P] [--limit N] [--check]`.

use std::collections::BTreeMap;

use katalogr_dist::DistError;
use katalogr_dist::analytics::{DeadCodeOptions, DeadConfidence};

use crate::cli::{Cli, ConfidenceArg};
use crate::cmd::{options_for, resolve};
use crate::exit::ExitCode;
use crate::render;

pub fn run(
    cli: &Cli,
    internal: bool,
    min_confidence: ConfidenceArg,
    path: Option<&str>,
    limit: usize,
    check: bool,
) -> Result<ExitCode, DistError> {
    let k = resolve(cli)?;
    let settings = k.insight_settings();
    let opts = DeadCodeOptions {
        // The facade fills the code kinds from the registered extensions.
        code_kinds: BTreeMap::new(),
        exclude_kinds: settings.exclude_kinds,
        internal,
        min_confidence: match min_confidence {
            ConfidenceArg::High => DeadConfidence::High,
            ConfidenceArg::Medium => DeadConfidence::Medium,
            ConfidenceArg::Low => DeadConfidence::Low,
        },
        path_prefix: path.map(str::to_string),
        limit,
    };
    let o = options_for(cli, "dead-code", &[("options", format!("{opts:?}"))]);
    let reply = k.dead_code(opts, &o)?;
    let shape = if check {
        &render::DEAD_CODE_CHECK
    } else {
        &render::DEAD_CODE
    };
    Ok(render::insight(cli, reply, "dead-code", shape))
}
