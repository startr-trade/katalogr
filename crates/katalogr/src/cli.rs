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

/// Granularity of the insight graph.
#[derive(Debug, Clone, Copy, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum LevelArg {
    Symbol,
    File,
    Package,
}

/// How sure a dead-code candidate is.
#[derive(Debug, Clone, Copy, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ConfidenceArg {
    High,
    Medium,
    Low,
}

/// Lowest severity of `doctor` finding that makes it exit 1.
#[derive(Debug, Clone, Copy, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum FailOn {
    Error,
    Warning,
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

    /// Limit query output to about this many tokens (estimated as characters / 4).
    #[arg(long, global = true, value_name = "TOKENS")]
    pub budget: Option<u64>,

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
        /// Queue what a git hook event changed (`post-commit`, `post-checkout`, `post-merge`);
        /// the paths are then the hook's own arguments. Never fails, never blocks git.
        #[arg(long, value_name = "HOOK")]
        from_git: Option<String>,
        /// Paths relative to the repository root.
        #[arg(required_unless_present = "from_git")]
        paths: Vec<String>,
    },
    /// Claude Code hook entry points (read the hook JSON on stdin).
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },
    /// Install, remove or show the Claude Code settings entries and git hooks.
    Hooks {
        #[command(subcommand)]
        action: HooksAction,
    },
    /// List symbols whose name matches the text.
    Find {
        /// Canonical name, or part of a plain name.
        text: String,
    },
    /// List what references a symbol, including through re-exports.
    Refs {
        /// Canonical name or plain name.
        text: String,
        /// Also explain the answer: a bounded trace in the JSON `diagnostic` field, or on stderr.
        #[arg(long)]
        diagnostic: bool,
    },
    /// List the callers of a symbol.
    Callers {
        /// Canonical name or plain name.
        text: String,
        /// How many call hops to follow.
        #[arg(long, default_value_t = 1, value_name = "N")]
        depth: u32,
    },
    /// Show the symbol at a line and the rationale linked to it.
    Why {
        /// `FILE:LINE`, with FILE relative to the repository root.
        location: String,
    },
    /// Outline the project: symbols by PageRank over the reference graph.
    Map {
        /// Also explain the answer: a bounded trace in the JSON `diagnostic` field, or on stderr.
        #[arg(long)]
        diagnostic: bool,
    },
    /// Rank symbols by BM25 over names, docs, sections and contract keys.
    Search {
        /// Words to look for; identifiers are split into words.
        #[arg(required = true)]
        terms: Vec<String>,
        /// How many hits to return.
        #[arg(long, default_value_t = 20, value_name = "N")]
        limit: usize,
        /// Also explain the answer: a bounded trace in the JSON `diagnostic` field, or on stderr.
        #[arg(long)]
        diagnostic: bool,
    },
    /// Print a symbol's lines (or, with --signature, its signature and doc).
    Show {
        /// Canonical name or plain name.
        symbol: String,
        /// Print only the signature and doc facts.
        #[arg(long)]
        signature: bool,
    },
    /// Find the shortest chain of links between two symbols.
    Path {
        /// Canonical name or plain name of the start.
        from: String,
        /// Canonical name or plain name of the end.
        to: String,
        /// Longest chain to look for.
        #[arg(long, default_value_t = 6, value_name = "N")]
        max_len: u32,
    },
    /// Describe a symbol: signature, doc, links and rationale.
    Explain {
        /// Canonical name or plain name.
        symbol: String,
        /// How many links to list on each side.
        #[arg(long, default_value_t = 10, value_name = "N")]
        per_side: usize,
    },
    /// Replace a symbol's lines with the content of a file, then reindex it.
    Replace {
        /// Canonical name or plain name.
        symbol: String,
        /// File holding the new text for exactly the symbol's lines.
        #[arg(long, value_name = "PATH")]
        file: String,
    },
    /// Check the index and print a recommended fix for each problem; exits 1 on findings.
    Doctor {
        /// Lowest severity that makes the command exit 1.
        #[arg(long, value_enum, default_value = "warning", value_name = "LEVEL")]
        fail_on: FailOn,
        /// Do not compare the shard files with the kindex git repository.
        #[arg(long)]
        skip_git: bool,
        /// List up to a few repo-relative keys for each finding.
        #[arg(long)]
        verbose: bool,
    },
    /// Claude Code project wiring.
    Claude {
        #[command(subcommand)]
        action: ClaudeAction,
    },
    /// Manage the katalogr skill for Claude Code.
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },
    /// Print a compact project map for Claude's SessionStart hook; never fails.
    SessionStart,
    /// Queue changes and bring the index up to date.
    Index {
        /// Index changes since this revision (up to HEAD). With `--history`, only the
        /// commits after this revision are indexed.
        #[arg(long, value_name = "REV", conflicts_with_all = ["range", "full"])]
        since: Option<String>,
        /// Index changes in a revision range `a..b`.
        #[arg(long, value_name = "A..B", conflicts_with_all = ["full", "history"])]
        range: Option<String>,
        /// Rescan every repository of the project.
        #[arg(long)]
        full: bool,
        /// Index the commits that carry item trailers (`Refs:`).
        #[arg(long)]
        history: bool,
    },
    /// Show the index watermark, pending changes and staleness.
    Status,
    /// List the hub nodes: those with the highest weighted degree (links in plus out).
    Hubs {
        /// Group nodes as symbols, files or packages.
        #[arg(long, value_enum, default_value_t = LevelArg::Symbol)]
        level: LevelArg,
        /// Smallest weighted degree listed (default: the project's `insights.hub_min_degree`).
        #[arg(long, value_name = "DEGREE")]
        min_degree: Option<f64>,
        /// How many hubs to list.
        #[arg(long, default_value_t = 20, value_name = "N")]
        limit: usize,
        /// Leave out weak links (inferred, or resolved to several targets).
        #[arg(long)]
        strict: bool,
    },
    /// Report how complete the index is: resolved, inferred and unresolved references, dangling
    /// contracts, parse errors, and files no extension claims.
    Coverage {
        /// Only this repository of the project.
        #[arg(long, value_name = "NAME")]
        repo: Option<String>,
        /// How many file suffixes to list per repository for unclaimed files.
        #[arg(long, default_value_t = 20, value_name = "N")]
        suffixes: usize,
    },
    /// Find communities: groups of nodes that use each other more than the rest.
    Communities {
        /// Group nodes as symbols, files or packages.
        #[arg(long, value_enum, default_value_t = LevelArg::File)]
        level: LevelArg,
        /// Seed of the search; the same seed gives the same communities.
        #[arg(long, value_name = "SEED")]
        seed: Option<u64>,
        /// Modularity resolution: higher gives smaller communities.
        #[arg(long, value_name = "GAMMA")]
        resolution: Option<f64>,
        /// Members listed per community.
        #[arg(long, default_value_t = 5, value_name = "N")]
        members: usize,
        /// How many communities to list.
        #[arg(long, default_value_t = 20, value_name = "N")]
        limit: usize,
        /// Only the community of this node (canonical name at the chosen level).
        #[arg(long, value_name = "NAME")]
        of: Option<String>,
        /// Leave out weak links (inferred, or resolved to several targets).
        #[arg(long)]
        strict: bool,
    },
    /// List dependency cycles, each with one witness; exits 1 when any exist.
    Cycles {
        /// Group nodes as symbols, files or packages.
        #[arg(long, value_enum, default_value_t = LevelArg::File)]
        level: LevelArg,
        /// Also follow inferred links (each cycle then shows its weakest confidence).
        #[arg(long)]
        include_inferred: bool,
        /// Members listed per cycle.
        #[arg(long, default_value_t = 10, value_name = "N")]
        members: usize,
        /// How many cycles to list.
        #[arg(long, default_value_t = 20, value_name = "N")]
        limit: usize,
    },
    /// List dead-code candidates: code no entry point reaches. `high` means no link of any
    /// confidence reaches it, not that it is certainly dead: string-keyed registries that no
    /// profile models stay invisible.
    DeadCode {
        /// Do not count public-API roots as entry points.
        #[arg(long)]
        internal: bool,
        /// Least confidence listed.
        #[arg(long, value_enum, default_value_t = ConfidenceArg::Medium)]
        min_confidence: ConfidenceArg,
        /// Only symbols under this repository-relative path prefix.
        #[arg(long, value_name = "PREFIX")]
        path: Option<String>,
        /// How many candidates to list.
        #[arg(long, default_value_t = 50, value_name = "N")]
        limit: usize,
        /// Exit 1 when any candidate is listed.
        #[arg(long)]
        check: bool,
    },
    /// List the rationale of a symbol, a path or the top hubs: comment markers, ADR citations,
    /// board items and documents that explain it.
    Rationale {
        /// Canonical name or plain name of a symbol (its members are included).
        symbol: Option<String>,
        /// Only files under this repository-relative path prefix.
        #[arg(long, value_name = "PREFIX", conflicts_with_all = ["symbol", "hubs"])]
        path: Option<String>,
        /// The top N hubs; those with no rationale are named.
        #[arg(long, value_name = "N", conflicts_with = "symbol")]
        hubs: Option<usize>,
        /// How many entries to list.
        #[arg(long, default_value_t = 100, value_name = "N")]
        limit: usize,
    },
    /// List references that name something that does not exist; exits 1 when any dangle.
    Dangling {
        /// Only this link kind, or `contract` for one-sided contracts; repeatable (default:
        /// links-to, annotated-by and refs-item).
        #[arg(long, value_name = "KIND")]
        kind: Vec<String>,
    },
    /// Compare mirrored copies of a structure: contracts with two or more declarers. Exits 1
    /// when a field is missing on one side or differs in type, required or many.
    Mirrors {
        /// Only contracts of this kind (for example `schema_type`); repeatable.
        #[arg(long, value_name = "KIND")]
        kind: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ClaudeAction {
    /// Write or refresh the katalogr block in the repository's CLAUDE.md.
    Sync {
        /// Print the block without writing.
        #[arg(long)]
        show: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum SkillAction {
    /// Install the katalogr skill (installed, updated or unchanged).
    Install {
        /// Where to install: `user` (~/.claude, the default) or `repo` (this repository's .claude).
        #[arg(long, value_enum, default_value_t = SkillScope::User)]
        scope: SkillScope,
    },
}

/// Where `skill install` writes the skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SkillScope {
    User,
    Repo,
}

#[derive(Debug, Subcommand)]
pub enum HookAction {
    /// PreToolUse for Read: block a whole-file read of a large indexed file.
    PreRead,
    /// PostToolUse for Grep and Glob: add index facts about the names searched for.
    PostSearch,
}

#[derive(Debug, Subcommand)]
pub enum HooksAction {
    /// Add katalogr's entries (idempotent) and the git hooks of the current repository.
    Install {
        /// Write `~/.claude/settings.json` instead of the project's.
        #[arg(long)]
        global: bool,
    },
    /// Remove only katalogr's entries and git hook lines.
    Uninstall {
        /// Use `~/.claude/settings.json` instead of the project's.
        #[arg(long)]
        global: bool,
    },
    /// Show what is installed where.
    Status,
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
