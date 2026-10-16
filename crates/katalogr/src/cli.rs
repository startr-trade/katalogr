//! Command-line definitions (clap derive).

use std::sync::LazyLock;

use clap::{Parser, ValueEnum};

use crate::cmd::version::version_string;

/// clap prints `<bin> <version>` itself, so the `katalogr ` prefix is dropped here.
static VERSION: LazyLock<String> =
    LazyLock::new(|| version_string().trim_start_matches("katalogr ").to_string());

/// Freshness requirement for a read.
#[derive(Debug, Clone, Copy, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum Consistency {
    /// Wait until the index is fresh for the project.
    Wait,
    /// Wait until every project's index is fresh.
    WaitAll,
    /// Answer from the index as it is, even if stale.
    StaleOk,
}

/// Traceability for polyglot systems.
#[allow(dead_code)] // global flags are consumed by commands added in later items
#[derive(Debug, Parser)]
#[command(name = "katalogr", version = VERSION.as_str(), about, long_about = None)]
pub struct Cli {
    /// Path to the kindex repository.
    #[arg(long, global = true, value_name = "PATH")]
    pub kindex: Option<String>,

    /// Project name.
    #[arg(long, global = true, value_name = "NAME")]
    pub project: Option<String>,

    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    pub json: bool,

    /// Freshness requirement for the read.
    #[arg(long, global = true, value_enum, value_name = "MODE")]
    pub consistency: Option<Consistency>,

    /// Seconds to wait for a fresh index before giving up.
    #[arg(long, global = true, value_name = "SECONDS")]
    pub timeout: Option<u64>,
}
