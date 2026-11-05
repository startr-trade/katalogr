//! `katalogr position [--repo R] [--json]`: the checkout's position, and what its context
//! reflects. Works without a board.

use katalogr_dist::{DistError, PositionAnswer};

use crate::cli::Cli;
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;

pub fn run(cli: &Cli, repo: Option<&str>) -> Result<ExitCode, DistError> {
    let answer = match resolve(cli) {
        Ok(k) => k.position(repo, &options(cli))?,
        Err(_) => {
            let cwd = std::env::current_dir()
                .map_err(|e| DistError::Usage(format!("current directory: {e}")))?;
            katalogr_dist::position_without_board(&cwd)?
        }
    };
    print(cli, &answer);
    Ok(ExitCode::Ok)
}

fn print(cli: &Cli, a: &PositionAnswer) {
    if cli.json {
        println!("{}", serde_json::to_string(a).expect("serialise position"));
        return;
    }
    println!("{}", a.text);
    if a.reflected.is_empty() {
        println!("reflects: nothing recorded");
    }
    for p in &a.reflected {
        println!(
            "reflects: {}{}",
            p.text(),
            if p.repo == a.live.repo {
                ""
            } else {
                " (other repo)"
            }
        );
    }
    println!("exact: {}", a.exact);
}
