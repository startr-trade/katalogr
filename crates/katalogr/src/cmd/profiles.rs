//! `katalogr profiles list` and `katalogr profiles show <name>`.

use katalogr_dist::{DistError, Katalogr, ProfileListing};

use crate::cli::{Cli, ProfilesAction};
use crate::cmd::resolve;
use crate::exit::ExitCode;
use crate::render;

pub fn run(cli: &Cli, action: &ProfilesAction) -> Result<ExitCode, DistError> {
    match action {
        ProfilesAction::List => {
            // Outside a project only the built-ins are known.
            let listing = match resolve(cli) {
                Ok(k) => k.profile_listing(),
                Err(_) => Katalogr::builtin_listing(),
            };
            print_listing(cli, &listing);
            Ok(ExitCode::Ok)
        }
        ProfilesAction::Show { name } => match Katalogr::builtin_profile(name) {
            Some(text) => {
                if cli.json {
                    println!("{}", render::json::profile_text(name, text));
                } else {
                    print!("{text}");
                }
                Ok(ExitCode::Ok)
            }
            None => {
                eprintln!(
                    "katalogr: no built-in profile {name:?} (built-ins: {})",
                    Katalogr::builtin_profile_names().join(", ")
                );
                Ok(ExitCode::NoMatch)
            }
        },
    }
}

fn print_listing(cli: &Cli, l: &ProfileListing) {
    if cli.json {
        println!("{}", render::json::profile_listing(l));
        return;
    }
    for b in &l.builtins {
        println!("{b}");
    }
    for p in &l.project {
        match &p.replaces {
            Some(r) => println!("{} (replaces {r})", p.file),
            None => println!("{}", p.file),
        }
    }
    for e in &l.refused {
        println!("refused: {e}");
    }
}
