//! `katalogr render mdbook` and `katalogr render filter`: the doc-tool modes of `render`.

use katalogr_dist::DistError;

use crate::cli::Cli;
use crate::exit::ExitCode;

pub fn mdbook(_cli: &Cli, _args: &[String]) -> Result<ExitCode, DistError> {
    Err(DistError::Usage(
        "render mdbook is not built yet".to_string(),
    ))
}

pub fn filter(_cli: &Cli, _assets: &str, _strict: bool) -> Result<ExitCode, DistError> {
    Err(DistError::Usage(
        "render filter is not built yet".to_string(),
    ))
}
