//! Machine-local baselines of metric samples, and the comparison with a run.

use katalogr_dist::analytics::metrics::{self, Better, MetricDiff, MetricSample};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

use crate::error::{BenchError, Result};
use crate::machine::MachineProfile;
use crate::report::{RunResult, atomic};
use crate::samples::tolerance;
use crate::workspace::Workspace;

pub const BASELINE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineFile {
    pub version: u32,
    pub run_id: String,
    pub machine: MachineProfile,
    pub samples: Vec<MetricSample>,
}

/// Copy a run's samples to `baselines/<name>.json`. Refuses a run marked loaded.
pub fn bless(ws: &Workspace, run: &RunResult, name: &str) -> Result<()> {
    if run.loaded() {
        return Err(BenchError::Workspace(format!(
            "run `{}` was measured on a loaded machine and cannot become a baseline",
            run.run_id
        )));
    }
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(BenchError::Workspace(format!(
            "baseline name `{name}` must match [A-Za-z0-9_-]+"
        )));
    }
    let file = BaselineFile {
        version: BASELINE_VERSION,
        run_id: run.run_id.clone(),
        machine: run.machine.clone(),
        samples: run.samples.clone(),
    };
    let path = ws.baseline(name);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    atomic(
        &path,
        serde_json::to_string_pretty(&file)
            .expect("baseline serializes")
            .as_bytes(),
    )
}

pub fn load(ws: &Workspace, name: &str) -> Result<BaselineFile> {
    let path = ws.baseline(name);
    let text = std::fs::read_to_string(&path)
        .map_err(|_| BenchError::Workspace(format!("no baseline named `{name}`")))?;
    let f: BaselineFile = serde_json::from_str(&text)
        .map_err(|e| BenchError::Workspace(format!("baseline `{name}`: {e}")))?;
    if f.version > BASELINE_VERSION {
        return Err(BenchError::Workspace(format!(
            "baseline `{name}` has version {} but this harness reads {BASELINE_VERSION}",
            f.version
        )));
    }
    Ok(f)
}

/// `metrics::compare` with the tolerance of each sample's family. For higher-is-better samples
/// the family tolerance is relative: `current < baseline * (1 - rel) - abs`.
pub fn compare(baseline: &[MetricSample], current: &[MetricSample]) -> MetricDiff {
    // `compare` applies `abs` only to higher-is-better samples, so they are filtered again below.
    let mut diff = metrics::compare(baseline, current, &|name| {
        let t = tolerance(name);
        metrics::Tolerance {
            abs: if is_higher(baseline, current, name) {
                0.0
            } else {
                t.abs
            },
            rel: t.rel,
        }
    });
    let mut kept = Vec::new();
    for change in std::mem::take(&mut diff.regressions) {
        if change.better == Better::Higher {
            let name = change.key.split('{').next().unwrap_or(&change.key);
            let t = tolerance(name);
            if change.current >= change.baseline * (1.0 - t.rel) - t.abs {
                diff.unchanged += 1;
                continue;
            }
        }
        kept.push(change);
    }
    diff.regressions = kept;
    diff
}

fn is_higher(baseline: &[MetricSample], current: &[MetricSample], name: &str) -> bool {
    baseline
        .iter()
        .chain(current)
        .find(|s| s.name == name)
        .is_some_and(|s| s.better == Better::Higher)
}

/// 1 on any regression, else 0.
pub fn exit_code(diff: &MetricDiff) -> i32 {
    if diff.regressions.is_empty() { 0 } else { 1 }
}

/// A short listing of the regressions and improvements.
pub fn render(diff: &MetricDiff) -> String {
    let mut o = String::new();
    let line = |o: &mut String, tag: &str, c: &metrics::MetricChange| {
        let pct = if c.baseline != 0.0 {
            (c.current - c.baseline) / c.baseline * 100.0
        } else {
            0.0
        };
        let _ = writeln!(
            o,
            "{tag} {}: {} -> {} ({pct:+.1}%)",
            c.key, c.baseline, c.current
        );
    };
    for c in &diff.regressions {
        line(&mut o, "regression ", c);
    }
    for c in &diff.improvements {
        line(&mut o, "improvement", c);
    }
    let _ = writeln!(
        o,
        "{} regression(s), {} improvement(s), {} unchanged, {} added, {} removed",
        diff.regressions.len(),
        diff.improvements.len(),
        diff.unchanged,
        diff.added.len(),
        diff.removed.len()
    );
    o
}

#[cfg(test)]
fn p95(v: f64) -> MetricSample {
    MetricSample::new("load.query.p95_ms", v, Better::Lower)
        .label("corpus", "demo")
        .label("op", "find")
}

#[cfg(test)]
#[test]
fn regression_exits_one() {
    let base = vec![p95(1000.0)];
    // +20% is over the 15% + 5 ms tolerance; +10% is inside it.
    let worse = compare(&base, &[p95(1200.0)]);
    assert_eq!(worse.regressions.len(), 1);
    assert_eq!(exit_code(&worse), 1);
    assert!(render(&worse).contains("regression"));
    let ok = compare(&base, &[p95(1100.0)]);
    assert!(ok.regressions.is_empty());
    assert_eq!(exit_code(&ok), 0);
    // Faster is an improvement.
    let better = compare(&base, &[p95(500.0)]);
    assert_eq!((better.improvements.len(), exit_code(&better)), (1, 0));
    // Higher-is-better samples use the relative tolerance: -20% regresses, -10% does not.
    let fps = |v: f64| {
        MetricSample::new("load.index.files_per_s", v, Better::Higher).label("corpus", "demo")
    };
    assert_eq!(compare(&[fps(100.0)], &[fps(80.0)]).regressions.len(), 1);
    assert!(compare(&[fps(100.0)], &[fps(90.0)]).regressions.is_empty());
    // Exact samples tolerate nothing.
    let errs =
        |v: f64| MetricSample::new("load.query.errors", v, Better::Lower).label("op", "find");
    assert_eq!(compare(&[errs(0.0)], &[errs(1.0)]).regressions.len(), 1);
    let det = |v: f64| MetricSample::new("load.index.deterministic", v, Better::Higher);
    assert_eq!(compare(&[det(1.0)], &[det(0.0)]).regressions.len(), 1);
}

#[cfg(test)]
#[test]
fn loaded_results_never_bless() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = Workspace::open(Some(camino::Utf8Path::from_path(tmp.path()).unwrap())).unwrap();
    let mut run = crate::report::sample_run();
    bless(&ws, &run, "S").unwrap();
    let back = load(&ws, "S").unwrap();
    assert_eq!(back.samples, run.samples);
    assert_eq!(back.run_id, run.run_id);
    run.corpora[0].loaded = true;
    let e = bless(&ws, &run, "loaded").unwrap_err();
    assert!(e.to_string().contains("loaded machine"), "{e}");
    assert!(!ws.baseline("loaded").exists());
    assert!(load(&ws, "loaded").is_err());
}
