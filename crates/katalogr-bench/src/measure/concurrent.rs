//! Queries served from worker processes while a cold full index runs.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::query::{Outcome, classify_run, workload_for};
use super::{ConcurrentResult, Ctx, INDEX_TIMEOUT, QUERY_TIMEOUT};
use crate::error::{BenchError, Result};
use crate::katalogr::Katalogr;
use crate::proc;
use crate::report::redact;
use crate::sizes;
use crate::stats::summary;
use crate::workload::{QueryOp, Workload, short_name};

/// The ops the workers loop over: find, refs, callers, explain, search and session-start.
pub fn worker_ops(w: &Workload) -> Vec<QueryOp> {
    let mut v = Vec::new();
    if let Some(t) = w.targets.first() {
        v.push(QueryOp::Find(short_name(t)));
        v.push(QueryOp::Refs(t.clone()));
        v.push(QueryOp::Callers(t.clone()));
        v.push(QueryOp::Explain(t.clone()));
    }
    if let Some(t) = w.terms.first() {
        v.push(QueryOp::Search(t.clone()));
    }
    v.push(QueryOp::SessionStart);
    v
}

/// The doctor run after the concurrent index. Warnings do not fail it: only errors do, so a
/// corpus that parses with warnings (and so exits 1 on an idle index too) is not mistaken for
/// damage done by the concurrent run (FEAT-080 R-9).
pub fn doctor_args() -> [&'static str; 4] {
    ["doctor", "--skip-git", "--fail-on", "error"]
}

/// How long the workers wait for the index to publish its first round before starting anyway.
const FIRST_ROUND_WAIT: Duration = Duration::from_secs(600);

/// Block until the cache file exists and a round has published a shard, so a query that finds
/// an empty cache is not counted as an answer (FEAT-080 R-10).
fn wait_first_round(project_dir: &camino::Utf8Path, limit: Duration) {
    let start = Instant::now();
    while start.elapsed() < limit {
        let cache = project_dir.join(".cache").join("index.redb").exists();
        let shards = sizes::shard_stats(project_dir).map_or(0, |s| s.files);
        if cache && shards > 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[derive(Default)]
struct Shared {
    ms: Vec<f64>,
    errors: u32,
    examples: Vec<String>,
}

fn worker(
    k: &Katalogr,
    ops: &[QueryOp],
    id: usize,
    stop: &AtomicBool,
    shared: &Mutex<Shared>,
    ws_root: &str,
) {
    let mut i = id;
    while !stop.load(Ordering::Relaxed) {
        let op = &ops[i % ops.len()];
        i += 1;
        let args = op.args();
        let mut full: Vec<&str> = vec!["--consistency", "stale-ok"];
        full.extend(args.iter().map(String::as_str));
        let r = proc::run_with_timeout(&mut k.cmd(&full), QUERY_TIMEOUT);
        // A query that started while the index ran is measured even if it finishes after the
        // index did: its latency includes any wait for the index.
        let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
        match r {
            Ok(r) => match classify_run(&r) {
                // Exit 4 counts as ok here: before its file is indexed an op can match nothing.
                Outcome::Ok | Outcome::NoMatch => s.ms.push(r.wall.as_secs_f64() * 1000.0),
                Outcome::Error(m) => {
                    s.errors += 1;
                    if s.examples.len() < 3 {
                        s.examples.push(redact(&m, ws_root));
                    }
                }
            },
            Err(e) => {
                s.errors += 1;
                if s.examples.len() < 3 {
                    s.examples.push(redact(&e.to_string(), ws_root));
                }
            }
        }
    }
}

/// Reset the project, spawn `index --full` and loop `concurrency` workers until it exits. The
/// final shard digest is compared with the cold runs' digest (both at the base commit) and
/// `doctor --skip-git --fail-on error` must exit 0.
pub fn run(
    cx: &mut Ctx,
    cold_digest: Option<&str>,
    cold_wall_s: Option<f64>,
) -> Result<ConcurrentResult> {
    let (w, _) = workload_for(cx)?;
    let ops = worker_ops(&w);
    let workers = cx.s.concurrency.max(1);
    cx.gate()?;
    cx.k.reset_index(cx.ws)?;
    // The cold runs indexed the base commits; the concurrent run must too, to compare shards.
    cx.checkout_base()?;
    let stop = AtomicBool::new(false);
    let shared = Mutex::new(Shared::default());
    let ws_root = cx.ws.root().to_string();
    let k = cx.k.clone();
    let project_dir = cx.project_dir();
    let index = std::thread::scope(|scope| -> Result<proc::RunStats> {
        let running = proc::spawn_with(&mut k.cmd(&["index", "--full"]), Some(INDEX_TIMEOUT))?;
        wait_first_round(&project_dir, FIRST_ROUND_WAIT);
        let handles: Vec<_> = (0..workers as usize)
            .map(|id| {
                let (k, ops, stop, shared, ws_root) = (&k, &ops, &stop, &shared, ws_root.as_str());
                scope.spawn(move || worker(k, ops, id, stop, shared, ws_root))
            })
            .collect();
        let done = running.finish();
        stop.store(true, Ordering::Relaxed);
        for h in handles {
            let _ = h.join();
        }
        done
    })?;
    cx.after();
    if index.status != Some(0) {
        return Err(BenchError::Katalogr(format!(
            "concurrent index --full exited {:?}: {}",
            index.status,
            super::first_line(&index.stderr)
        )));
    }
    let digest = sizes::shard_digest(&cx.project_dir())?;
    let doctor = proc::run_with_timeout(&mut cx.k.cmd(&doctor_args()), QUERY_TIMEOUT)?;
    let s = shared.into_inner().unwrap_or_else(|e| e.into_inner());
    let wall = index.wall.as_secs_f64();
    Ok(ConcurrentResult {
        workers,
        index_wall_s: wall,
        index_slowdown: match cold_wall_s {
            Some(c) if c > 0.0 => wall / c,
            _ => 0.0,
        },
        queries: (s.ms.len() as u32) + s.errors,
        errors: s.errors,
        ms: summary(&s.ms),
        digest_equal: cold_digest.is_some_and(|d| d == digest),
        doctor_clean: doctor.status == Some(0),
        error_examples: s.examples,
    })
}

#[cfg(test)]
#[test]
fn doctor_check_ignores_warnings() {
    let args = doctor_args();
    let at = args
        .iter()
        .position(|a| *a == "--fail-on")
        .expect("--fail-on");
    assert_eq!(args[at + 1], "error", "warnings must not fail the check");
    assert!(args.contains(&"--skip-git"));
    assert_eq!(args[0], "doctor");
}
