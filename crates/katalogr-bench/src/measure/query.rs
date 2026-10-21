//! Query latency per command, and session start.

use camino::Utf8Path;

use super::{Ctx, OpResult, QUERY_TIMEOUT, mib};
use crate::error::Result;
use crate::proc::{self, RunStats};
use crate::report::redact;
use crate::stats::summary;
use crate::workload::{self, QueryOp, Workload};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    /// Exit 4: the query legitimately matched nothing.
    NoMatch,
    Error(String),
}

/// Classify a `--json` invocation.
/// - Exit 0, 3 (ambiguous) is ok, and exit 4 (no match) is ok and counted separately.
/// - Exit 1 with no top-level `message` string in the JSON is ok: gate commands such as
///   `cycles` and `doctor` exit 1 on findings.
/// - Anything else (exit 1 with a `message`, exit 2, exit 5, a signal, unparsable stdout on exit
///   1) is an error carrying the first 200 bytes of the message.
pub fn classify(status: Option<i32>, stdout: &[u8]) -> Outcome {
    let message = || -> String {
        let text = serde_json::from_slice::<serde_json::Value>(stdout)
            .ok()
            .and_then(|v| {
                v.get("message")
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| {
                String::from_utf8_lossy(stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string()
            });
        truncate(&text, 200)
    };
    match status {
        Some(0) | Some(3) => Outcome::Ok,
        Some(4) => Outcome::NoMatch,
        Some(1) => match serde_json::from_slice::<serde_json::Value>(stdout) {
            Ok(v) if v.get("message").and_then(|m| m.as_str()).is_none() => Outcome::Ok,
            Ok(_) => Outcome::Error(message()),
            Err(_) => Outcome::Error(format!("unparsable output: {}", message())),
        },
        Some(n) => Outcome::Error(format!("exit {n}: {}", message())),
        None => Outcome::Error("killed by a signal".into()),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// The classification of a finished child, with its stderr used when stdout says nothing.
pub fn classify_run(r: &RunStats) -> Outcome {
    if r.timed_out {
        return Outcome::Error("timed out".into());
    }
    match classify(r.status, &r.stdout) {
        Outcome::Error(m) if m.is_empty() || m.ends_with(": ") => {
            let e = super::first_line(&r.stderr);
            Outcome::Error(format!("{m}{}", truncate(&e, 200)))
        }
        o => o,
    }
}

#[derive(Default)]
struct Acc {
    ms: Vec<f64>,
    rss: Vec<f64>,
    errors: u32,
    no_match: u32,
    examples: Vec<String>,
}

/// Load the frozen workload, or derive it from the index's symbol hubs and save it. The hubs
/// are asked for with `--min-degree 0`, so a small corpus below the project's hub threshold
/// still yields targets; on a large corpus the top 50 by degree are the same.
/// Returns the workload and whether an existing one was re-derived.
pub fn workload_for(cx: &Ctx) -> Result<(Workload, bool)> {
    let path = cx.ws.workload(&cx.c.name);
    if path.is_file() && !cx.s.refresh_workload {
        let text = std::fs::read_to_string(&path)?;
        let w: Workload = serde_json::from_str(&text)
            .map_err(|e| crate::error::BenchError::Manifest(format!("{path}: {e}")))?;
        if w.version > workload::WORKLOAD_VERSION {
            return Err(crate::error::BenchError::Manifest(format!(
                "{path}: workload version {} is newer than this harness",
                w.version
            )));
        }
        return Ok((w, false));
    }
    let refreshed = path.is_file();
    let hubs = cx.run_json(&[
        "--consistency",
        "stale-ok",
        "hubs",
        "--level",
        "symbol",
        "--limit",
        "50",
        "--min-degree",
        "0",
    ])?;
    let w = workload::derive(cx.c, &hubs, cx.s.targets);
    save_workload(&path, &w)?;
    Ok((w, refreshed))
}

fn save_workload(path: &Utf8Path, w: &Workload) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(w).expect("workload serializes"),
    )?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Run every op of the workload `reps` times after one discarded warm-up, with
/// `--consistency stale-ok`, and pool the samples per command label.
pub fn run(cx: &mut Ctx) -> Result<(Vec<OpResult>, bool)> {
    let (w, refreshed) = workload_for(cx)?;
    let all = workload::ops(&w);
    cx.gate()?;
    let reps = cx.s.reps.max(1);
    let ws_root = cx.ws.root().to_string();
    let mut order: Vec<&'static str> = Vec::new();
    let mut accs: std::collections::BTreeMap<&'static str, Acc> = Default::default();
    for op in &all {
        let label = op.label();
        if !order.contains(&label) {
            order.push(label);
        }
        let acc = accs.entry(label).or_default();
        for rep in 0..=reps {
            let r = one(cx, op)?;
            if rep == 0 {
                continue; // warm-up
            }
            match classify_run(&r) {
                Outcome::Ok | Outcome::NoMatch => {
                    if classify_run(&r) == Outcome::NoMatch {
                        acc.no_match += 1;
                    }
                    acc.ms.push(r.wall.as_secs_f64() * 1000.0);
                    acc.rss.push(mib(r.peak_rss_kib));
                }
                Outcome::Error(m) => {
                    acc.errors += 1;
                    if acc.examples.len() < 3 {
                        acc.examples.push(redact(&m, &ws_root));
                    }
                }
            }
        }
    }
    cx.after();
    let out = order
        .into_iter()
        .map(|label| {
            let a = accs.remove(label).unwrap_or_default();
            OpResult {
                command: label.to_string(),
                samples: a.ms.len() as u32,
                ms: summary(&a.ms),
                peak_rss_mib: summary(&a.rss),
                errors: a.errors,
                no_match: a.no_match,
                error_examples: a.examples,
            }
        })
        .collect();
    Ok((out, refreshed))
}

/// One invocation of an op through `proc::run`.
pub fn one(cx: &Ctx, op: &QueryOp) -> Result<RunStats> {
    let args = op.args();
    let mut full: Vec<&str> = vec!["--consistency", "stale-ok"];
    full.extend(args.iter().map(String::as_str));
    proc::run_with_timeout(&mut cx.k.cmd(&full), QUERY_TIMEOUT)
}
