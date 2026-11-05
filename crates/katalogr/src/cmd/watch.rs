//! `katalogr watch [--interval SECONDS]`: poll the checkout's position and reindex on change.

use std::sync::atomic::AtomicBool;

use katalogr_dist::DistError;
use katalogr_dist::watch::{WatchTick, default_interval, interval_of, watch};

use crate::cli::Cli;
use crate::cmd::resolve;
use crate::exit::ExitCode;

pub fn run(cli: &Cli, interval: Option<f64>) -> Result<ExitCode, DistError> {
    // A bad interval is a usage error before anything else is looked at.
    let explicit = interval.map(interval_of).transpose()?;
    let k = resolve(cli)?;
    let interval = match explicit {
        Some(i) => i,
        None => k
            .setting_f64("watch.interval")
            .map(interval_of)
            .transpose()?
            .unwrap_or_else(default_interval),
    };
    let stop = AtomicBool::new(false);
    watch(&k, interval, &stop, &mut |tick| match tick {
        WatchTick::Idle => {}
        WatchTick::Processed { pending } => println!("watch: indexed, {pending} pending"),
        WatchTick::Failed(e) => eprintln!("katalogr: warning: watch: {e}"),
    })?;
    Ok(ExitCode::Ok)
}
