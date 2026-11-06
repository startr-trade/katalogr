//! `katalogr snapshot export|import|show` (FEAT-097).

use katalogr_dist::{DistError, SnapshotManifest, Utf8Path};

use crate::cli::{Cli, SnapshotAction};
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;

pub fn run(cli: &Cli, action: &SnapshotAction) -> Result<ExitCode, DistError> {
    match action {
        SnapshotAction::Export { out } => {
            let k = resolve(cli)?;
            let r = k.snapshot_export(Utf8Path::new(out), &options(cli))?;
            if cli.json {
                println!("{}", serde_json::to_string(&r).expect("serialise export"));
            } else {
                println!(
                    "wrote {out}: {} repos, {} bytes in the file, digest {}",
                    r.manifest.positions.len(),
                    r.file_bytes,
                    short(&r.manifest.digest)
                );
                for w in &r.warnings {
                    eprintln!("warning: {w}");
                }
            }
            Ok(ExitCode::Ok)
        }
        SnapshotAction::Import {
            file,
            replace,
            digest,
        } => {
            if cli.context.is_some() {
                return Err(DistError::Usage(
                    "snapshot import always installs into the base: --context is not accepted"
                        .into(),
                ));
            }
            let k = resolve(cli)?;
            let r = k.snapshot_import(
                Utf8Path::new(file),
                *replace,
                digest.as_deref(),
                &options(cli),
            )?;
            if cli.json {
                println!("{}", serde_json::to_string(&r).expect("serialise import"));
            } else {
                println!(
                    "installed {file}: {} repos, digest {}",
                    r.manifest.positions.len(),
                    short(&r.manifest.digest)
                );
                if !r.facts_match {
                    println!(
                        "facts version differs from this binary's: rescans queued for {}",
                        r.rescans.join(", ")
                    );
                }
                for repo in &r.no_root {
                    println!("repo {repo} has no checkout here: it answers from the snapshot");
                }
                for w in &r.warnings {
                    eprintln!("warning: {w}");
                }
            }
            Ok(ExitCode::Ok)
        }
        SnapshotAction::Show { file, verify } => {
            let path = Utf8Path::new(file);
            // Without --verify the file alone is read: no project is needed.
            let m = if *verify {
                resolve(cli)?.snapshot_show(path, true)?
            } else {
                katalogr_dist::read_snapshot_manifest(path)?
            };
            if cli.json {
                println!("{}", serde_json::to_string(&m).expect("serialise manifest"));
            } else {
                print_manifest(&m, *verify);
            }
            Ok(ExitCode::Ok)
        }
    }
}

fn short(hex: &str) -> &str {
    &hex[..hex.len().min(12)]
}

fn print_manifest(m: &SnapshotManifest, verified: bool) {
    println!("format: {}", m.format);
    println!("cache schema: {}", m.cache_schema);
    println!("facts version: {}", m.facts_version);
    println!("database bytes: {}", m.bytes);
    println!("digest: {}", m.digest);
    for p in &m.positions {
        println!("position {}: {}", p.repo, p.commit);
    }
    if verified {
        println!("payload verified");
    }
}
