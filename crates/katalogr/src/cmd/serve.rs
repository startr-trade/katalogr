//! `katalogr serve`: the API server plus the embedded read-only web view (FEAT-033).

use katalogr_dist::DistError;

use crate::cli::{Cli, ServeArgs};
use crate::exit::ExitCode;

/// `--write-assets` writes the bundle and exits, before any project is resolved. Otherwise the
/// same server as `katalogr api`, answering pages too.
pub fn run(cli: &Cli, args: &ServeArgs) -> Result<ExitCode, DistError> {
    if let Some(dir) = &args.write_assets {
        let written = katalogr_dist::web::write_bundle(dir, args.api_base.as_deref().unwrap_or(""))
            .map_err(|e| DistError::Io {
                path: dir.display().to_string(),
                message: e.to_string(),
            })?;
        for path in written {
            println!("{}", path.display());
        }
        return Ok(ExitCode::Ok);
    }
    if args.api_base.is_some() {
        return Err(DistError::Usage(
            "--api-base only goes with --write-assets".to_string(),
        ));
    }
    super::api::serve(
        cli,
        args.listen,
        args.workers,
        args.queue,
        args.no_process,
        args.cors_origin.clone(),
        &args.snapshots,
        true,
        "serve",
    )
}
