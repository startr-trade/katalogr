//! Command-line definitions (clap derive).

use std::sync::LazyLock;

use clap::{Parser, Subcommand, ValueEnum};

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
    pub timeout: Option<f64>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage the kindex repository.
    Kindex {
        #[command(subcommand)]
        action: KindexAction,
    },
    /// Manage projects.
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    /// Queue changed files (called by hooks).
    Notify {
        /// The paths were deleted.
        #[arg(long)]
        deleted: bool,
        /// Paths relative to the repository root.
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Queue changes and bring the index up to date.
    Index {
        /// Index changes since this revision (up to HEAD).
        #[arg(long, value_name = "REV", conflicts_with_all = ["range", "full"])]
        since: Option<String>,
        /// Index changes in a revision range `a..b`.
        #[arg(long, value_name = "A..B", conflicts_with = "full")]
        range: Option<String>,
        /// Rescan every repository of the project.
        #[arg(long)]
        full: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum KindexAction {
    /// Create a kindex repository (idempotent).
    Init {
        /// Directory of the kindex.
        dir: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProjectAction {
    /// Create a project in the kindex given by `--kindex`.
    Init {
        /// Project name.
        name: String,
        /// A repository as `name=path`; repeatable.
        #[arg(long = "repo", value_name = "NAME=PATH", value_parser = parse_repo)]
        repos: Vec<(String, String)>,
        /// Write a `.katalogr` marker into each repository.
        #[arg(long)]
        mark: bool,
    },
    /// Record where a repository lives on this machine.
    AddRoot {
        /// Repository name.
        name: String,
        /// Repository root.
        path: String,
    },
}

fn parse_repo(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((n, p)) if !n.is_empty() && !p.is_empty() => Ok((n.to_string(), p.to_string())),
        _ => Err("expected NAME=PATH".to_string()),
    }
}
