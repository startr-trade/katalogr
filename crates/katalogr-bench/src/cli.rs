//! The `katalogr-bench` command line.

use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, Parser, Subcommand};
use std::process::ExitCode;
use std::time::Duration;

use crate::baseline;
use crate::disk::{self, DiskPolicy, GIB, Keep};
use crate::error::{BenchError, Result};
use crate::fetch;
use crate::git::Git;
use crate::idle::IdlePolicy;
use crate::katalogr::Katalogr;
use crate::machine::MachineProfile;
use crate::manifest::{Manifest, Selection, Tier};
use crate::measure::{self, CorpusResult, Phases, Settings};
use crate::report::{self, RUN_VERSION, RunResult, SettingsRecord};
use crate::workspace::Workspace;

#[derive(Debug, Parser)]
#[command(
    name = "katalogr-bench",
    version,
    about = "Load-test harness for the katalogr binary"
)]
pub struct Cli {
    /// Workspace directory (default: $KATALOGR_LOADTEST_HOME, else ~/.katalogr-loadtest).
    #[arg(long, global = true, value_name = "PATH")]
    pub workspace: Option<Utf8PathBuf>,
    /// The katalogr binary to measure (default: target/release/katalogr, else `katalogr` on PATH).
    #[arg(long, global = true, value_name = "PATH")]
    pub katalogr: Option<Utf8PathBuf>,
    /// Machine-readable output where a command prints a result.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args, Clone, Default)]
pub struct Select {
    /// Tiers to select, comma separated (S, M, L).
    #[arg(long, value_delimiter = ',', value_enum)]
    pub tier: Vec<Tier>,
    /// Corpus names to select; repeatable.
    #[arg(long = "corpus", value_name = "NAME")]
    pub corpus: Vec<String>,
}

#[derive(Debug, Args, Clone)]
pub struct DiskArgs {
    /// Workspace disk budget in GiB.
    #[arg(long, default_value_t = 30.0)]
    pub budget_gib: f64,
    /// Free space that must remain, in GiB.
    #[arg(long, default_value_t = 10.0)]
    pub min_free_gib: f64,
}

impl DiskArgs {
    fn policy(&self) -> DiskPolicy {
        DiskPolicy {
            budget_bytes: (self.budget_gib * GIB as f64) as u64,
            min_free_bytes: (self.min_free_gib * GIB as f64) as u64,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create the workspace layout and an empty corpora.yaml when it is missing.
    Init,
    /// Print each selected corpus with its estimated disk and whether it fits.
    Plan {
        #[command(flatten)]
        select: Select,
        #[command(flatten)]
        disk: DiskArgs,
    },
    /// Shallow-clone each selected corpus at its pinned commits.
    Fetch {
        #[command(flatten)]
        select: Select,
        #[command(flatten)]
        disk: DiskArgs,
    },
    /// Measure the selected corpora and write the results.
    Run {
        #[command(flatten)]
        select: Select,
        #[command(flatten)]
        disk: DiskArgs,
        /// Cold index runs (default 3 for tiers S and M, 1 for L).
        #[arg(long)]
        runs: Option<u32>,
        /// Repetitions per query after one warm-up.
        #[arg(long, default_value_t = 10)]
        reps: u32,
        /// Query targets per corpus.
        #[arg(long, default_value_t = 5)]
        targets: usize,
        /// Concurrent query workers.
        #[arg(long, default_value_t = 4)]
        concurrency: u32,
        /// Phases: index, replay, query, concurrent, all or none (comma separated).
        #[arg(long, default_value = "all")]
        phases: String,
        /// What to keep after measuring a corpus.
        #[arg(long, value_enum, default_value = "all")]
        keep: Keep,
        /// Highest 1-minute load average that counts as idle.
        #[arg(long, default_value_t = 1.0)]
        max_load: f64,
        /// Seconds to wait for an idle machine.
        #[arg(long, default_value_t = 600)]
        idle_wait: u64,
        /// Measure on a busy machine and mark the result loaded.
        #[arg(long)]
        allow_loaded: bool,
        /// Derive the query workload again instead of using the frozen one.
        #[arg(long)]
        refresh_workload: bool,
        #[arg(long)]
        run_id: Option<String>,
    },
    /// Idle re-measure of the ignored cargo benches (not built yet).
    Micro {
        #[arg(long)]
        repo: Option<Utf8PathBuf>,
        #[arg(long, default_value_t = 5)]
        runs: u32,
        #[arg(long)]
        run_id: Option<String>,
    },
    /// Regenerate report.md of a run.
    Report {
        #[arg(long)]
        run_id: String,
    },
    /// Copy a run's samples to a baseline.
    Bless {
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        name: String,
    },
    /// Compare a run with a baseline; exit 1 on any regression.
    Compare {
        #[arg(long)]
        baseline: String,
        #[arg(long)]
        run_id: String,
    },
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    match execute(&cli) {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("katalogr-bench: {e}");
            ExitCode::from(e.exit_code() as u8)
        }
    }
}

/// Resolve the katalogr binary to an absolute path.
pub fn resolve_katalogr(explicit: Option<&Utf8Path>) -> Result<Utf8PathBuf> {
    let candidate = match explicit {
        Some(p) => Some(p.to_path_buf()),
        None => {
            let local = Utf8PathBuf::from("target/release/katalogr");
            if local.is_file() {
                Some(local)
            } else {
                std::env::var_os("PATH").and_then(|paths| {
                    std::env::split_paths(&paths)
                        .map(|d| d.join("katalogr"))
                        .find(|p| p.is_file())
                        .and_then(|p| Utf8PathBuf::from_path_buf(p).ok())
                })
            }
        }
    };
    let Some(p) = candidate else {
        return Err(BenchError::Katalogr(
            "no katalogr binary: pass --katalogr or build target/release/katalogr".into(),
        ));
    };
    let abs = p
        .as_std_path()
        .canonicalize()
        .map_err(|e| BenchError::Katalogr(format!("{p}: {e}")))?;
    Utf8PathBuf::from_path_buf(abs)
        .map_err(|p| BenchError::Katalogr(format!("non-UTF-8 path {}", p.display())))
}

fn load_manifest(ws: &Workspace) -> Result<Manifest> {
    Manifest::load(&ws.manifest())
}

fn selection(s: &Select) -> Selection {
    Selection {
        tiers: s.tier.clone(),
        names: s.corpus.clone(),
    }
}

/// `YYYY-MM-DD` of the current UTC day.
pub fn utc_date() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 to a civil date (proleptic Gregorian).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn default_run_id(ws: &Workspace, sel: &Select) -> String {
    let tiers = if !sel.tier.is_empty() {
        sel.tier
            .iter()
            .map(|t| format!("{t:?}"))
            .collect::<String>()
    } else if !sel.corpus.is_empty() {
        "N".to_string()
    } else {
        "ALL".to_string()
    };
    let date = utc_date();
    (1..)
        .map(|n| format!("{date}-{tiers}-{n}"))
        .find(|id| !ws.results(id).exists())
        .expect("an unused run id")
}

fn execute(cli: &Cli) -> Result<i32> {
    let ws = Workspace::open(cli.workspace.as_deref())?;
    match &cli.command {
        Command::Init => {
            ws.init_layout()?;
            if !ws.manifest().exists() {
                std::fs::write(ws.manifest(), "version: 1\ncorpora: []\n")?;
            }
            println!("workspace ready");
            Ok(0)
        }
        Command::Plan { select, disk } => plan(&ws, select, disk, cli.json),
        Command::Fetch { select, disk } => {
            let _lock = ws.lock()?;
            let m = load_manifest(&ws)?;
            let git = Git::default();
            let policy = disk.policy();
            for c in m.select(&selection(select)) {
                let used = disk::usage_bytes(&ws.clones(&c.name))?
                    + disk::usage_bytes(&ws.project_dir(&c.name))?;
                let need = disk::need_bytes(c.approx.disk_gib, used);
                disk::check(
                    &policy,
                    disk::usage_bytes(ws.root())?,
                    disk::free_bytes(ws.root())?,
                    need,
                )
                .map_err(|e| BenchError::Disk(format!("corpus {}: {e}", c.name)))?;
                let f = fetch::fetch(&ws, c, &git)?;
                println!(
                    "{}: {} repositor{} pinned",
                    c.name,
                    f.len(),
                    if f.len() == 1 { "y" } else { "ies" }
                );
            }
            Ok(0)
        }
        Command::Run {
            select,
            disk,
            runs,
            reps,
            targets,
            concurrency,
            phases,
            keep,
            max_load,
            idle_wait,
            allow_loaded,
            refresh_workload,
            run_id,
        } => {
            let _lock = ws.lock()?;
            let m = load_manifest(&ws)?;
            let bin = resolve_katalogr(cli.katalogr.as_deref())?;
            let phases = Phases::parse(phases)?;
            let run_id = run_id
                .clone()
                .unwrap_or_else(|| default_run_id(&ws, select));
            if ws.results(&run_id).join("run.json").exists() {
                return Err(BenchError::Workspace(format!(
                    "run `{run_id}` already exists"
                )));
            }
            let idle = IdlePolicy {
                max_load: *max_load,
                wait: Duration::from_secs(*idle_wait),
                poll: Duration::from_secs(10)
                    .min(Duration::from_secs(*idle_wait).max(Duration::from_secs(1))),
                allow_loaded: *allow_loaded,
            };
            let base = Settings {
                katalogr: bin.clone(),
                runs: runs.unwrap_or(3),
                reps: *reps,
                targets: *targets,
                concurrency: *concurrency,
                idle,
                disk: disk.policy(),
                keep: *keep,
                refresh_workload: *refresh_workload,
                phases,
            };
            let git = Git::default();
            let mut results: Vec<CorpusResult> = Vec::new();
            let mut failure: Option<BenchError> = None;
            let mut refused = false;
            for c in m.select(&selection(select)) {
                let mut s = base.clone();
                if runs.is_none() && c.tier == Tier::L {
                    s.runs = 1;
                }
                eprintln!("measuring {} ({:?})", c.name, c.tier);
                match measure::run_corpus(&ws, c, &s, &git) {
                    Ok(r) => results.push(r),
                    Err(e @ BenchError::Disk(_)) => {
                        eprintln!("katalogr-bench: {e}");
                        refused = true;
                    }
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                }
            }
            if !results.is_empty() {
                let k = Katalogr {
                    bin,
                    kindex: ws.kindex(),
                    project: "-".into(),
                };
                let mut run = RunResult {
                    version: RUN_VERSION,
                    run_id: run_id.clone(),
                    date: utc_date(),
                    machine: MachineProfile::detect(&ws, &k),
                    settings: SettingsRecord {
                        runs: runs.unwrap_or(3),
                        reps: *reps,
                        targets: *targets,
                        concurrency: *concurrency,
                        max_load: *max_load,
                        idle_wait_s: *idle_wait,
                        allow_loaded: *allow_loaded,
                        keep: format!("{keep:?}").to_lowercase(),
                        phases: phases.names(),
                        refresh_workload: *refresh_workload,
                        budget_gib: disk.budget_gib,
                        min_free_gib: disk.min_free_gib,
                    },
                    corpora: results,
                    micro: vec![],
                    samples: vec![],
                };
                run.refresh_samples();
                report::write(&ws, &run)?;
                println!("run {run_id} written");
            }
            if let Some(e) = failure {
                return Err(e);
            }
            Ok(if refused { 3 } else { 0 })
        }
        Command::Micro { .. } => Err(BenchError::Workspace("micro: not yet built".into())),
        Command::Report { run_id } => {
            let r = RunResult::load(&ws, run_id)?;
            report::atomic(
                &ws.results(run_id).join("report.md"),
                report::markdown(&r).as_bytes(),
            )?;
            println!("report for {run_id} regenerated");
            Ok(0)
        }
        Command::Bless { run_id, name } => {
            let r = RunResult::load(&ws, run_id)?;
            baseline::bless(&ws, &r, name)?;
            println!("baseline {name} written from {run_id}");
            Ok(0)
        }
        Command::Compare {
            baseline: name,
            run_id,
        } => {
            let r = RunResult::load(&ws, run_id)?;
            let b = baseline::load(&ws, name)?;
            let diff = baseline::compare(&b.samples, &r.samples);
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&diff).expect("diff serializes")
                );
            } else {
                if b.machine.cpu_model != r.machine.cpu_model
                    || b.machine.logical_cpus != r.machine.logical_cpus
                {
                    println!(
                        "note: the baseline was measured on a different machine; timings are not comparable"
                    );
                }
                print!("{}", baseline::render(&diff));
            }
            Ok(baseline::exit_code(&diff))
        }
    }
}

fn plan(ws: &Workspace, select: &Select, disk_args: &DiskArgs, json: bool) -> Result<i32> {
    let m = load_manifest(ws)?;
    let policy = disk_args.policy();
    let free = disk::free_bytes(ws.root())?;
    let usage = disk::usage_bytes(ws.root())?;
    let mut planned = 0u64;
    let mut fits_all = true;
    let mut rows = Vec::new();
    for c in m.select(&selection(select)) {
        let used =
            disk::usage_bytes(&ws.clones(&c.name))? + disk::usage_bytes(&ws.project_dir(&c.name))?;
        let need = disk::need_bytes(c.approx.disk_gib, used);
        let verdict = disk::check(&policy, usage + planned, free.saturating_sub(planned), need);
        if verdict.is_err() {
            fits_all = false;
        }
        planned += need;
        rows.push((c.name.clone(), c.tier, c.approx.disk_gib, verdict));
    }
    if json {
        let v: Vec<_> = rows
            .iter()
            .map(|(n, t, g, v)| {
                serde_json::json!({"corpus": n, "tier": format!("{t:?}"), "disk_gib": g, "fits": v.is_ok(),
                    "reason": v.as_ref().err().map(|e| e.to_string())})
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&v).expect("plan serializes")
        );
    } else {
        println!(
            "workspace uses {:.1} GiB, {:.1} GiB free, budget {:.1} GiB, floor {:.1} GiB",
            disk::usage_bytes(ws.root())? as f64 / GIB as f64,
            free as f64 / GIB as f64,
            disk_args.budget_gib,
            disk_args.min_free_gib
        );
        for (n, t, g, v) in &rows {
            match v {
                Ok(()) => println!("{n} ({t:?}): {g:.2} GiB, fits"),
                Err(e) => println!("{n} ({t:?}): {g:.2} GiB, {e}"),
            }
        }
    }
    Ok(if fits_all { 0 } else { 3 })
}
