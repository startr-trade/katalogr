//! The idle re-measure of the ignored cargo benches (FEAT-068 R-11): each listed bench prints
//! `katalogr-bench: <id> <value> <unit>` lines, the runner takes the median of several runs and
//! proposes a budget from the measurement where the budget was missed.

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::error::{BenchError, Result};
use crate::idle::{IdlePolicy, ProcLoadavg, wait_idle};
use crate::stats::median;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    Met,
    MissedSetFromMeasurement,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MicroResult {
    pub id: String,
    /// `ms` or `ratio`.
    pub unit: String,
    pub budget: f64,
    pub runs: Vec<f64>,
    pub median: f64,
    pub verdict: Verdict,
    pub proposed_budget: Option<f64>,
    pub item: String,
    pub req: String,
}

/// One measured id of an ignored cargo bench.
#[derive(Debug, Clone, Copy)]
pub struct MicroBench {
    pub id: &'static str,
    pub package: &'static str,
    pub test_target: &'static str,
    /// The path given to `--exact`, as `cargo test -- --list` prints it.
    pub test: &'static str,
    pub budget: f64,
    pub unit: &'static str,
    pub item: &'static str,
    pub req: &'static str,
}

#[allow(clippy::too_many_arguments)]
const fn mb(
    id: &'static str,
    package: &'static str,
    test_target: &'static str,
    test: &'static str,
    budget: f64,
    unit: &'static str,
    item: &'static str,
    req: &'static str,
) -> MicroBench {
    MicroBench {
        id,
        package,
        test_target,
        test,
        budget,
        unit,
        item,
        req,
    }
}

/// The fixed list of the idle re-measure, one entry per printed line id. Entries that share a
/// `test` come from one test that prints several lines.
pub const MICRO: &[MicroBench] = &[
    mb(
        "xsd_extract_1mib",
        "katalogr-ext-schema",
        "bench",
        "bench::xsd_extract_1mib",
        121.0,
        "ms",
        "FEAT-009",
        "R-13",
    ),
    mb(
        "jsonschema_extract_1mib_json",
        "katalogr-ext-schema",
        "bench",
        "bench::jsonschema_extract_1mib",
        107.0,
        "ms",
        "FEAT-009",
        "R-13",
    ),
    mb(
        "jsonschema_extract_1mib_yaml",
        "katalogr-ext-schema",
        "bench",
        "bench::jsonschema_extract_1mib",
        153.0,
        "ms",
        "FEAT-009",
        "R-13",
    ),
    mb(
        "data_doc_20k_keys",
        "katalogr-ext-data",
        "bench",
        "bench::data_doc_20k_keys",
        73.0,
        "ms",
        "FEAT-009",
        "R-13",
    ),
    mb(
        "profile_scope_20k_symbols",
        "katalogr-engine",
        "bench_profile",
        "bench::profile_scope_20k_symbols",
        105.0,
        "ms",
        "FEAT-009",
        "R-13",
    ),
    mb(
        "string_calls_time_ratio",
        "katalogr-acceptance",
        "bench",
        "bench::string_calls_overhead",
        1.41,
        "ratio",
        "FEAT-014",
        "R-15",
    ),
    mb(
        "string_calls_bytes_ratio",
        "katalogr-acceptance",
        "bench",
        "bench::string_calls_overhead",
        1.15,
        "ratio",
        "FEAT-014",
        "R-15",
    ),
    mb(
        "coverage_100k",
        "katalogr-analytics",
        "bench",
        "bench::coverage_100k",
        1500.0,
        "ms",
        "FEAT-062",
        "R-12",
    ),
    mb(
        "tests_100k",
        "katalogr-query",
        "bench",
        "tests_100k",
        500.0,
        "ms",
        "FEAT-063",
        "R-13",
    ),
    mb(
        "doctor_100k_git",
        "katalogr-engine",
        "bench",
        "bench::doctor_100k",
        5000.0,
        "ms",
        "FEAT-064",
        "R-15",
    ),
    mb(
        "doctor_100k_skip_git",
        "katalogr-engine",
        "bench",
        "bench::doctor_100k",
        1500.0,
        "ms",
        "FEAT-064",
        "R-15",
    ),
];

/// Lines `katalogr-bench: <id> <value> <unit>` from a test's output. Other lines, and lines
/// with a missing or non-numeric value, are skipped.
pub fn parse_lines(out: &str) -> Vec<(String, f64, String)> {
    out.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("katalogr-bench:")?;
            let mut it = rest.split_whitespace();
            let id = it.next()?;
            let value: f64 = it.next()?.parse().ok()?;
            let unit = it.next()?;
            if it.next().is_some() || !value.is_finite() || !matches!(unit, "ms" | "ratio") {
                return None;
            }
            Some((id.to_string(), value, unit.to_string()))
        })
        .collect()
}

/// `ceil(median * 1.15)` to the step: 5 for ms below 1000, 50 for ms from 1000, 0.01 for
/// `ratio`.
pub fn budget_from_measurement(median: f64, unit: &str) -> f64 {
    let padded = median * 1.15;
    let (step, decimals) = if unit == "ratio" {
        (0.01, 2)
    } else if median < 1000.0 {
        (5.0, 0)
    } else {
        (50.0, 0)
    };
    // The small subtraction keeps a product that is a whole step apart from float noise.
    let steps = (padded / step - 1e-9).ceil();
    let v = steps * step;
    let scale = 10f64.powi(decimals);
    (v * scale).round() / scale
}

/// The result of one id from its runs: the median, the verdict against the budget, and the
/// proposed budget when the budget was missed.
pub fn result_from_runs(b: &MicroBench, runs: Vec<f64>) -> MicroResult {
    let med = median(&runs);
    let met = med <= b.budget;
    MicroResult {
        id: b.id.to_string(),
        unit: b.unit.to_string(),
        budget: b.budget,
        median: med,
        verdict: if met {
            Verdict::Met
        } else {
            Verdict::MissedSetFromMeasurement
        },
        proposed_budget: (!met).then(|| budget_from_measurement(med, b.unit)),
        runs,
        item: b.item.to_string(),
        req: b.req.to_string(),
    }
}

fn run_once(repo: &Utf8Path, b: &MicroBench) -> Result<Vec<(String, f64, String)>> {
    let out = Command::new("cargo")
        .current_dir(repo)
        .args([
            "test",
            "--release",
            "--locked",
            "-p",
            b.package,
            "--test",
            b.test_target,
        ])
        .args(["--", "--ignored", "--exact", "--nocapture", b.test])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()?;
    // The exit status is ignored when the lines were printed: a budget assert fails after them.
    Ok(parse_lines(&String::from_utf8_lossy(&out.stdout)))
}

/// For each MICRO test, `runs` times: wait for an idle machine, then run it in release in
/// `repo`. A missing line is an error.
pub fn run_micro(repo: &Utf8Path, runs: u32, idle: &IdlePolicy) -> Result<Vec<MicroResult>> {
    let mut tests: Vec<&MicroBench> = Vec::new();
    for b in MICRO {
        if !tests
            .iter()
            .any(|t| (t.package, t.test_target, t.test) == (b.package, b.test_target, b.test))
        {
            tests.push(b);
        }
    }
    let mut values: Vec<Vec<f64>> = vec![Vec::new(); MICRO.len()];
    for t in tests {
        for _ in 0..runs {
            wait_idle(&ProcLoadavg, idle, &mut |d: Duration| std::thread::sleep(d))?;
            let lines = run_once(repo, t)?;
            for (i, b) in MICRO.iter().enumerate() {
                if (b.package, b.test_target, b.test) != (t.package, t.test_target, t.test) {
                    continue;
                }
                let v = lines.iter().find(|(id, _, _)| id == b.id).ok_or_else(|| {
                    BenchError::Katalogr(format!(
                        "micro: bench {} printed no `katalogr-bench: {}` line",
                        b.test, b.id
                    ))
                })?;
                values[i].push(v.1);
            }
        }
    }
    Ok(MICRO
        .iter()
        .zip(values)
        .filter(|(_, v)| !v.is_empty())
        .map(|(b, v)| result_from_runs(b, v))
        .collect())
}

#[cfg(test)]
#[test]
fn micro_list_is_consistent() {
    let mut ids: Vec<_> = MICRO.iter().map(|b| b.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), MICRO.len());
    assert!(MICRO.iter().all(|b| matches!(b.unit, "ms" | "ratio")));
}
