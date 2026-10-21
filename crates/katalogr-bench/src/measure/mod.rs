//! Measuring one corpus: fetch, project, then the phases index, discovery, replay, query and
//! concurrent, each behind an idle check.

pub mod concurrent;
pub mod discovery;
pub mod index;
pub mod query;
pub mod replay;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

use crate::disk::{self, DiskPolicy, Keep};
use crate::error::{BenchError, Result};
use crate::fetch::{self, Fetched};
use crate::git::Git;
use crate::idle::{IdleOutcome, IdlePolicy, LoadSource, ProcLoadavg, wait_idle};
use crate::katalogr::Katalogr;
use crate::manifest::{Corpus, Tier};
use crate::proc;
use crate::stats::Summary;
use crate::workspace::Workspace;

/// Longest a cold or concurrent full index may run.
pub const INDEX_TIMEOUT: Duration = Duration::from_secs(6 * 3600);
/// Longest a single query or replay step may run.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(120);
pub const REPLAY_TIMEOUT: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Phases {
    pub index: bool,
    pub replay: bool,
    pub query: bool,
    pub concurrent: bool,
}

impl Phases {
    pub const ALL: Phases = Phases {
        index: true,
        replay: true,
        query: true,
        concurrent: true,
    };
    pub const NONE: Phases = Phases {
        index: false,
        replay: false,
        query: false,
        concurrent: false,
    };

    /// `index,replay,query,concurrent`, `all` or `none`.
    pub fn parse(s: &str) -> Result<Phases> {
        let mut p = Phases::NONE;
        for part in s.split(',').map(str::trim).filter(|x| !x.is_empty()) {
            match part {
                "all" => p = Phases::ALL,
                "none" => {}
                "index" => p.index = true,
                "replay" => p.replay = true,
                "query" => p.query = true,
                "concurrent" => p.concurrent = true,
                other => {
                    return Err(BenchError::Manifest(format!(
                        "unknown phase `{other}` (index, replay, query, concurrent, all, none)"
                    )));
                }
            }
        }
        Ok(p)
    }

    pub fn names(&self) -> String {
        let mut v = Vec::new();
        for (on, n) in [
            (self.index, "index"),
            (self.replay, "replay"),
            (self.query, "query"),
            (self.concurrent, "concurrent"),
        ] {
            if on {
                v.push(n);
            }
        }
        if v.is_empty() {
            "none".into()
        } else {
            v.join(",")
        }
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub katalogr: Utf8PathBuf,
    pub runs: u32,
    pub reps: u32,
    pub targets: usize,
    pub concurrency: u32,
    pub idle: IdlePolicy,
    pub disk: DiskPolicy,
    pub keep: Keep,
    pub refresh_workload: bool,
    pub phases: Phases,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoResult {
    pub name: String,
    pub url: String,
    pub commit: String,
    pub files_in_tree: u64,
    pub tree_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexResult {
    pub runs: u32,
    pub wall_s: Summary,
    pub files_indexed: u64,
    pub files_per_s: f64,
    pub peak_rss_mib: Summary,
    pub shard_files: u64,
    pub shard_bytes: u64,
    pub cache_bytes: u64,
    pub digests_equal: bool,
    /// Digest of the shard tree of the last run.
    pub digest: String,
    /// Coefficient of variation of the wall times.
    pub wall_cv: f64,
    /// More than 2 runs and the coefficient of variation is above 10%.
    pub noisy: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayResult {
    pub commits: u32,
    pub files_changed: Summary,
    pub wall_ms: Summary,
    pub peak_rss_mib: Summary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpResult {
    /// `QueryOp::label`: the command, `doctor-skip-git` or `session-start`.
    pub command: String,
    pub samples: u32,
    pub ms: Summary,
    pub peak_rss_mib: Summary,
    pub errors: u32,
    pub no_match: u32,
    /// Up to 3 redacted error messages, 200 bytes each.
    #[serde(default)]
    pub error_examples: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConcurrentResult {
    pub workers: u32,
    pub index_wall_s: f64,
    pub index_slowdown: f64,
    pub queries: u32,
    pub errors: u32,
    pub ms: Summary,
    pub digest_equal: bool,
    pub doctor_clean: bool,
    #[serde(default)]
    pub error_examples: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryResult {
    pub files_seen: u64,
    pub files_indexed: u64,
    pub files_unclaimed: u64,
    /// `files_seen / median cold wall`, for corpora whose only claimed files are `text` or `md`.
    pub unclaimed_per_s: Option<f64>,
    pub text_files: u64,
    /// Chunk symbols of text-fallback shards.
    pub text_chunks: Option<u64>,
    pub by_suffix: Vec<(String, u64)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiskResult {
    pub clone_bytes: u64,
    pub index_bytes: u64,
    pub free_before: u64,
    pub free_after: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorpusResult {
    pub corpus: String,
    pub tier: Tier,
    pub category: String,
    pub languages: Vec<String>,
    pub repos: Vec<RepoResult>,
    /// Any phase ran with `IdleOutcome::Loaded`.
    pub loaded: bool,
    pub load_before: Vec<f64>,
    pub load_after: Vec<f64>,
    pub index: Option<IndexResult>,
    pub replay: Option<ReplayResult>,
    pub queries: Vec<OpResult>,
    pub concurrent: Option<ConcurrentResult>,
    pub discovery: Option<DiscoveryResult>,
    pub disk: DiskResult,
    /// The query workload was re-derived, so it is not comparable with a baseline.
    #[serde(default)]
    pub workload_refreshed: bool,
    /// A phase failed and the corpus was abandoned (what failed).
    #[serde(default)]
    pub aborted: Option<String>,
}

/// Everything a phase needs.
pub struct Ctx<'a> {
    pub ws: &'a Workspace,
    pub c: &'a Corpus,
    pub s: &'a Settings,
    pub git: &'a Git,
    pub k: Katalogr,
    pub fetched: Vec<Fetched>,
    pub load: &'a dyn LoadSource,
    pub sleep: fn(Duration),
    pub loaded: bool,
    pub load_before: Vec<f64>,
    pub load_after: Vec<f64>,
}

impl Ctx<'_> {
    /// Wait for an idle machine before a measured step and record the load.
    pub fn gate(&mut self) -> Result<IdleOutcome> {
        let mut sleep = self.sleep;
        let out = wait_idle(self.load, &self.s.idle, &mut sleep)?;
        self.load_before.push(out.load());
        if out.is_loaded() {
            self.loaded = true;
        }
        Ok(out)
    }

    /// Record the load after a measured step.
    pub fn after(&mut self) {
        if let Ok(l) = self.load.load1() {
            self.load_after.push(l);
        }
    }

    pub fn primary(&self) -> &Fetched {
        self.fetched
            .iter()
            .find(|f| f.repo == self.c.primary)
            .expect("primary repository is fetched")
    }

    /// Check out `base` in every repository (the state a cold index measures).
    pub fn checkout_base(&self) -> Result<()> {
        for f in &self.fetched {
            fetch::checkout(self.git, &self.ws.clone_dir(&self.c.name, &f.repo), &f.base)?;
        }
        Ok(())
    }

    pub fn checkout_head(&self) -> Result<()> {
        for f in &self.fetched {
            fetch::checkout(self.git, &self.ws.clone_dir(&self.c.name, &f.repo), &f.head)?;
        }
        Ok(())
    }

    pub fn project_dir(&self) -> Utf8PathBuf {
        self.ws.project_dir(&self.c.name)
    }

    /// Run a katalogr command to completion and parse its JSON stdout.
    pub fn run_json(&self, args: &[&str]) -> Result<Value> {
        let r = proc::run_with_timeout(&mut self.k.cmd(args), QUERY_TIMEOUT)?;
        if r.status != Some(0) {
            return Err(BenchError::Katalogr(format!(
                "{}: exit {:?}: {}",
                args.first().copied().unwrap_or(""),
                r.status,
                first_line(&r.stderr)
            )));
        }
        serde_json::from_slice(&r.stdout).map_err(|e| {
            BenchError::Katalogr(format!(
                "{}: bad JSON: {e}",
                args.first().copied().unwrap_or("")
            ))
        })
    }
}

pub fn first_line(b: &[u8]) -> String {
    String::from_utf8_lossy(b)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Files and bytes of the checked-out tree, `.git` and the `.katalogr` marker left out.
pub fn tree_stats(dir: &Utf8Path) -> (u64, u64) {
    fn go(p: &std::path::Path, top: bool, files: &mut u64, bytes: &mut u64) {
        let Ok(rd) = std::fs::read_dir(p) else { return };
        for e in rd.flatten() {
            let name = e.file_name();
            if top && (name == ".git" || name == ".katalogr") {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                go(&e.path(), false, files, bytes);
            } else if ft.is_file() {
                *files += 1;
                *bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    let (mut f, mut b) = (0, 0);
    go(dir.as_std_path(), true, &mut f, &mut b);
    (f, b)
}

pub fn mib(kib: Option<u64>) -> f64 {
    kib.map(|k| k as f64 / 1024.0).unwrap_or(0.0)
}

/// Fetch, ensure the project, run the enabled phases in order index, discovery, replay, query,
/// concurrent; then evict per `keep`. Each phase calls `wait_idle` first.
pub fn run_corpus(ws: &Workspace, c: &Corpus, s: &Settings, git: &Git) -> Result<CorpusResult> {
    let load = ProcLoadavg;
    run_corpus_with(ws, c, s, git, &load, std::thread::sleep)
}

/// As [`run_corpus`] with an injected load source and sleep (tests).
pub fn run_corpus_with(
    ws: &Workspace,
    c: &Corpus,
    s: &Settings,
    git: &Git,
    load: &dyn LoadSource,
    sleep: fn(Duration),
) -> Result<CorpusResult> {
    // Disk guard first: nothing is cloned or indexed when it refuses.
    let used =
        disk::usage_bytes(&ws.clones(&c.name))? + disk::usage_bytes(&ws.project_dir(&c.name))?;
    let need = disk::need_bytes(c.approx.disk_gib, used);
    let free_before = disk::free_bytes(ws.root())?;
    disk::check(&s.disk, disk::usage_bytes(ws.root())?, free_before, need)
        .map_err(|e| BenchError::Disk(format!("corpus {}: {e}", c.name)))?;

    let fetched = fetch::fetch(ws, c, git)?;
    let k = Katalogr {
        bin: s.katalogr.clone(),
        kindex: ws.kindex(),
        project: c.name.clone(),
    };
    k.ensure_project(ws, c)?;
    let mut cx = Ctx {
        ws,
        c,
        s,
        git,
        k,
        fetched,
        load,
        sleep,
        loaded: false,
        load_before: Vec::new(),
        load_after: Vec::new(),
    };
    let repos = c
        .repos
        .iter()
        .map(|r| {
            let (files, bytes) = tree_stats(&ws.clone_dir(&c.name, &r.name));
            RepoResult {
                name: r.name.clone(),
                url: r.url.clone(),
                commit: r.commit.clone(),
                files_in_tree: files,
                tree_bytes: bytes,
            }
        })
        .collect();

    let mut res = CorpusResult {
        corpus: c.name.clone(),
        tier: c.tier,
        category: c.category.clone(),
        languages: c.languages.clone(),
        repos,
        loaded: false,
        load_before: vec![],
        load_after: vec![],
        index: None,
        replay: None,
        queries: vec![],
        concurrent: None,
        discovery: None,
        disk: DiskResult {
            clone_bytes: 0,
            index_bytes: 0,
            free_before,
            free_after: free_before,
        },
        workload_refreshed: false,
        aborted: None,
    };

    let outcome = phases(&mut cx, &mut res);
    if let Err(e) = outcome {
        match e {
            // A refusal to measure on a busy machine ends the whole run.
            BenchError::NotIdle { .. } | BenchError::Disk(_) | BenchError::Busy(_) => {
                return Err(e);
            }
            other => res.aborted = Some(other.to_string()),
        }
    }
    res.loaded = cx.loaded;
    res.load_before = cx.load_before.clone();
    res.load_after = cx.load_after.clone();
    res.disk.clone_bytes = disk::usage_bytes(&ws.clones(&c.name))?;
    res.disk.index_bytes = disk::usage_bytes(&ws.project_dir(&c.name))?;
    res.disk.free_after = disk::free_bytes(ws.root())?;
    disk::evict(ws, &c.name, s.keep)?;
    Ok(res)
}

fn phases(cx: &mut Ctx, res: &mut CorpusResult) -> Result<()> {
    let p = cx.s.phases;
    let mut cold_digest: Option<String> = None;
    let mut cold_wall_s: Option<f64> = None;
    if p.index {
        let r = index::run(cx)?;
        cold_digest = Some(r.digest.clone());
        cold_wall_s = Some(r.wall_s.p50);
        res.index = Some(r);
        // Discovery reads the coverage report of the freshly indexed project.
        res.discovery = Some(discovery::run(cx, cold_wall_s)?);
    }
    if p.replay && cx.c.replay > 0 {
        res.replay = Some(replay::run(cx)?);
    }
    if p.query {
        let (ops, refreshed) = query::run(cx)?;
        res.queries = ops;
        res.workload_refreshed = refreshed;
    }
    if p.concurrent {
        res.concurrent = Some(concurrent::run(cx, cold_digest.as_deref(), cold_wall_s)?);
    }
    Ok(())
}
