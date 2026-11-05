//! `katalogr contract`: what this binary serves, for a client that links by contract.

use katalogr_dist::DistError;
use katalogr_dist::contract::contract_info;

use crate::cli::Cli;
use crate::exit::ExitCode;

pub fn run(cli: &Cli) -> Result<ExitCode, DistError> {
    let i = contract_info();
    if cli.json {
        println!("{}", serde_json::to_string(&i).expect("serialise contract"));
    } else {
        println!(
            "katalogr {}: api v{}, contract {}, schema {}, position format {}, view format {}",
            i.katalogr, i.api, i.contract, i.schema, i.position_format, i.view_format
        );
        println!("capabilities: {}", i.capabilities.join(", "));
    }
    Ok(ExitCode::Ok)
}
