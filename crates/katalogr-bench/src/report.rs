//! The versioned run result and the deterministic Markdown report.

use camino::Utf8PathBuf;
use katalogr_dist::analytics::metrics::MetricSample;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

use crate::error::{BenchError, Result};
use crate::machine::MachineProfile;
use crate::measure::CorpusResult;
use crate::micro::{MicroResult, Verdict};
use crate::workspace::Workspace;

pub const RUN_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsRecord {
    pub runs: u32,
    pub reps: u32,
    pub targets: usize,
    pub concurrency: u32,
    pub max_load: f64,
    pub idle_wait_s: u64,
    pub allow_loaded: bool,
    pub keep: String,
    pub phases: String,
    pub refresh_workload: bool,
    pub budget_gib: f64,
    pub min_free_gib: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    pub version: u32,
    pub run_id: String,
    pub date: String,
    pub machine: MachineProfile,
    pub settings: SettingsRecord,
    pub corpora: Vec<CorpusResult>,
    pub micro: Vec<MicroResult>,
    pub samples: Vec<MetricSample>,
}

impl RunResult {
    /// Any corpus was measured on a machine that never became idle.
    pub fn loaded(&self) -> bool {
        self.corpora.iter().any(|c| c.loaded)
    }

    /// Recompute `samples` from the corpora and the micro results.
    pub fn refresh_samples(&mut self) {
        let mut v = Vec::new();
        for c in &self.corpora {
            v.extend(crate::samples::corpus_samples(c));
        }
        v.extend(crate::samples::micro_samples(&self.micro));
        self.samples = v;
    }

    pub fn load(ws: &Workspace, run_id: &str) -> Result<RunResult> {
        let path = ws.results(run_id).join("run.json");
        let text = std::fs::read_to_string(&path).map_err(|_| {
            BenchError::Workspace(format!(
                "run `{run_id}` has no run.json (partial or unknown run)"
            ))
        })?;
        let r: RunResult = serde_json::from_str(&text)
            .map_err(|e| BenchError::Workspace(format!("run `{run_id}`: {e}")))?;
        if r.version > RUN_VERSION {
            return Err(BenchError::Workspace(format!(
                "run `{run_id}` has version {} but this harness reads {RUN_VERSION}",
                r.version
            )));
        }
        Ok(r)
    }
}

/// Replace the workspace root by `<ws>`.
pub fn redact(text: &str, ws_root: &str) -> String {
    if ws_root.is_empty() {
        return text.to_string();
    }
    text.replace(ws_root, "<ws>")
}

/// Replace every absolute path (a token that starts with `/` and has a second `/`) by `<path>`.
/// URLs are left alone, because their slashes do not follow whitespace or a bracket.
pub fn scrub(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let start_of_token = i == 0
            || matches!(
                chars[i - 1],
                ' ' | '\t' | '\n' | '(' | '[' | '\'' | '"' | '`' | '=' | ','
            );
        if chars[i] == '/' && start_of_token {
            let mut j = i;
            while j < chars.len()
                && !matches!(
                    chars[j],
                    ' ' | '\t' | '\n' | ')' | ']' | '\'' | '"' | '`' | ','
                )
            {
                j += 1;
            }
            let token: String = chars[i..j].iter().collect();
            if token[1..].contains('/') {
                out.push_str("<path>");
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Write `results/<run_id>/run.json` and `report.md`, each by temp file and rename.
pub fn write(ws: &Workspace, r: &RunResult) -> Result<Utf8PathBuf> {
    let dir = ws.results(&r.run_id);
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(r).expect("run result serializes");
    atomic(&dir.join("report.md"), markdown(r).as_bytes())?;
    atomic(&dir.join("run.json"), json.as_bytes())?;
    Ok(dir)
}

pub fn atomic(path: &camino::Utf8Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn ms(x: f64) -> String {
    format!("{x:.1}")
}
fn sec(x: f64) -> String {
    format!("{x:.2}")
}
fn mib(x: f64) -> String {
    format!("{x:.0}")
}
fn ratio(x: f64) -> String {
    format!("{x:.2}")
}
fn bytes_mib(b: u64) -> String {
    mib(b as f64 / (1024.0 * 1024.0))
}
fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}
fn cell(s: &str) -> String {
    scrub(s).replace('|', "\\|").replace('\n', " ")
}

/// Deterministic Markdown: a pure function of the run result. Tables are in corpus order, then
/// op name. It never contains the workspace path, `$HOME`, the user name or the host name;
/// corpora are named by their manifest name and URL, and any absolute path in free text is
/// replaced.
pub fn markdown(r: &RunResult) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "# Load test {}\n", cell(&r.date));
    let _ = writeln!(
        o,
        "Run `{}`, harness format {}.\n",
        cell(&r.run_id),
        r.version
    );

    let m = &r.machine;
    let _ = writeln!(o, "## 1. Machine profile\n");
    let _ = writeln!(o, "| Field | Value |\n|---|---|");
    for (k, v) in [
        ("os", m.os.clone()),
        ("arch", m.arch.clone()),
        ("kernel", m.kernel.clone()),
        ("cpu", m.cpu_model.clone()),
        ("logical cpus", m.logical_cpus.to_string()),
        ("memory GiB", m.mem_gib.to_string()),
        ("filesystem", m.fs.clone()),
        ("katalogr", m.katalogr_version.clone()),
        ("harness", m.harness_version.clone()),
        ("load source", m.load_source.clone()),
    ] {
        let _ = writeln!(o, "| {k} | {} |", cell(&v));
    }

    let s = &r.settings;
    let _ = writeln!(o, "\n## 2. Protocol\n");
    let _ = writeln!(o, "| Setting | Value |\n|---|---|");
    for (k, v) in [
        ("idle threshold (1-minute load)", ratio(s.max_load)),
        ("idle wait (s)", s.idle_wait_s.to_string()),
        ("loaded runs allowed", yes(s.allow_loaded).to_string()),
        ("cold runs per corpus", s.runs.to_string()),
        ("repetitions per query", s.reps.to_string()),
        ("targets per corpus", s.targets.to_string()),
        ("concurrent workers", s.concurrency.to_string()),
        ("phases", s.phases.clone()),
        ("keep", s.keep.clone()),
        ("workload refreshed", yes(s.refresh_workload).to_string()),
        ("disk budget GiB", ratio(s.budget_gib)),
        ("free-space floor GiB", ratio(s.min_free_gib)),
    ] {
        let _ = writeln!(o, "| {k} | {} |", cell(&v));
    }
    let _ = writeln!(
        o,
        "\n| Corpus | Loaded | Load before | Load after |\n|---|---|---|---|"
    );
    for c in &r.corpora {
        let fmt = |v: &[f64]| v.iter().map(|x| ratio(*x)).collect::<Vec<_>>().join(" ");
        let _ = writeln!(
            o,
            "| {} | {} | {} | {} |",
            cell(&c.corpus),
            yes(c.loaded),
            fmt(&c.load_before),
            fmt(&c.load_after)
        );
    }

    let _ = writeln!(o, "\n## 3. Corpora\n");
    let _ = writeln!(
        o,
        "| Corpus | Tier | Category | Repository | Commit | Files | Tree MiB | Claimed files | Unclaimed files | Clone MiB | Index MiB |\n|---|---|---|---|---|---|---|---|---|---|---|"
    );
    for c in &r.corpora {
        let (claimed, unclaimed) = c
            .discovery
            .as_ref()
            .map(|d| (d.files_indexed.to_string(), d.files_unclaimed.to_string()))
            .unwrap_or_else(|| ("-".into(), "-".into()));
        for (i, repo) in c.repos.iter().enumerate() {
            let first = i == 0;
            let _ = writeln!(
                o,
                "| {} | {} | {} | {} | `{}` | {} | {} | {} | {} | {} | {} |",
                cell(&c.corpus),
                c.tier.label(),
                cell(&c.category),
                cell(&repo.url),
                cell(&repo.commit),
                repo.files_in_tree,
                bytes_mib(repo.tree_bytes),
                if first {
                    claimed.clone()
                } else {
                    String::new()
                },
                if first {
                    unclaimed.clone()
                } else {
                    String::new()
                },
                if first {
                    bytes_mib(c.disk.clone_bytes)
                } else {
                    String::new()
                },
                if first {
                    bytes_mib(c.disk.index_bytes)
                } else {
                    String::new()
                },
            );
        }
    }

    let _ = writeln!(o, "\n## 4. Index\n");
    let _ = writeln!(
        o,
        "| Corpus | Runs | Wall s (p50) | Min s | Max s | CV | Files indexed | Files/s | Peak RSS MiB (p50) | Shard files | Shard MiB | Cache MiB | Deterministic | Noisy |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"
    );
    for c in &r.corpora {
        if let Some(i) = &c.index {
            let _ = writeln!(
                o,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                cell(&c.corpus),
                i.runs,
                sec(i.wall_s.p50),
                sec(i.wall_s.min),
                sec(i.wall_s.max),
                ratio(i.wall_cv),
                i.files_indexed,
                ratio(i.files_per_s),
                mib(i.peak_rss_mib.p50),
                i.shard_files,
                bytes_mib(i.shard_bytes),
                bytes_mib(i.cache_bytes),
                yes(i.digests_equal),
                yes(i.noisy),
            );
        }
    }

    let _ = writeln!(o, "\n## 5. Replay\n");
    let _ = writeln!(
        o,
        "| Corpus | Commits | Files changed p50 | p95 | max | Wall ms p50 | p95 | max | Peak RSS MiB p50 |\n|---|---|---|---|---|---|---|---|---|"
    );
    for c in &r.corpora {
        if let Some(p) = &c.replay {
            let _ = writeln!(
                o,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                cell(&c.corpus),
                p.commits,
                ms(p.files_changed.p50),
                ms(p.files_changed.p95),
                ms(p.files_changed.max),
                ms(p.wall_ms.p50),
                ms(p.wall_ms.p95),
                ms(p.wall_ms.max),
                mib(p.peak_rss_mib.p50),
            );
        }
    }

    let _ = writeln!(o, "\n## 6. Query latency\n");
    let _ = writeln!(
        o,
        "| Corpus | Command | Samples | p50 ms | p95 ms | Max ms | Errors | No match |\n|---|---|---|---|---|---|---|---|"
    );
    for c in &r.corpora {
        let mut qs: Vec<_> = c
            .queries
            .iter()
            .filter(|q| q.command != "session-start")
            .collect();
        qs.sort_by(|a, b| a.command.cmp(&b.command));
        for q in qs {
            let _ = writeln!(
                o,
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                cell(&c.corpus),
                cell(&q.command),
                q.samples,
                ms(q.ms.p50),
                ms(q.ms.p95),
                ms(q.ms.max),
                q.errors,
                q.no_match
            );
        }
    }

    let _ = writeln!(o, "\n## 7. Session start\n");
    let _ = writeln!(
        o,
        "| Corpus | Samples | p50 ms | p95 ms | Max ms | Errors |\n|---|---|---|---|---|---|"
    );
    for c in &r.corpora {
        if let Some(q) = c.queries.iter().find(|q| q.command == "session-start") {
            let _ = writeln!(
                o,
                "| {} | {} | {} | {} | {} | {} |",
                cell(&c.corpus),
                q.samples,
                ms(q.ms.p50),
                ms(q.ms.p95),
                ms(q.ms.max),
                q.errors
            );
        }
    }

    let _ = writeln!(o, "\n## 8. Concurrent queries while indexing\n");
    let _ = writeln!(
        o,
        "| Corpus | Workers | Index wall s | Slowdown | Queries | Errors | p50 ms | p95 ms | Shards identical | Doctor clean |\n|---|---|---|---|---|---|---|---|---|---|"
    );
    for c in &r.corpora {
        if let Some(k) = &c.concurrent {
            let _ = writeln!(
                o,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                cell(&c.corpus),
                k.workers,
                sec(k.index_wall_s),
                ratio(k.index_slowdown),
                k.queries,
                k.errors,
                ms(k.ms.p50),
                ms(k.ms.p95),
                yes(k.digest_equal),
                yes(k.doctor_clean)
            );
        }
    }

    let _ = writeln!(o, "\n## 9. Unsupported-language baselines\n");
    let _ = writeln!(
        o,
        "| Corpus | Files seen | Indexed | Unclaimed | Unclaimed files/s | Text files | Text chunks | Unclaimed by suffix |\n|---|---|---|---|---|---|---|---|"
    );
    for c in &r.corpora {
        if let Some(d) = &c.discovery {
            if d.files_unclaimed == 0 && d.text_files == 0 {
                continue;
            }
            let suffixes = d
                .by_suffix
                .iter()
                .map(|(s, n)| format!("{} {n}", if s.is_empty() { "(none)" } else { s }))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                o,
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                cell(&c.corpus),
                d.files_seen,
                d.files_indexed,
                d.files_unclaimed,
                d.unclaimed_per_s.map(ratio).unwrap_or_else(|| "-".into()),
                d.text_files,
                d.text_chunks
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "-".into()),
                cell(&suffixes)
            );
        }
    }

    let _ = writeln!(o, "\n## 10. Idle re-measure\n");
    let _ = writeln!(
        o,
        "| Id | Item | Requirement | Budget | Median | Runs | Verdict | Proposed budget |\n|---|---|---|---|---|---|---|---|"
    );
    for m in &r.micro {
        let _ = writeln!(
            o,
            "| {} | {} | {} | {} {} | {} | {} | {} | {} |",
            cell(&m.id),
            cell(&m.item),
            cell(&m.req),
            ratio(m.budget),
            cell(&m.unit),
            ratio(m.median),
            m.runs.len(),
            match m.verdict {
                Verdict::Met => "met",
                Verdict::MissedSetFromMeasurement => "missed, set from measurement",
            },
            m.proposed_budget.map(ratio).unwrap_or_else(|| "-".into())
        );
    }

    let _ = writeln!(o, "\n## 11. Findings\n");
    let findings = findings(r);
    if findings.is_empty() {
        let _ = writeln!(o, "None.");
    }
    for f in findings {
        let _ = writeln!(o, "- {}", cell(&f));
    }
    o
}

/// Everything the report must call out: aborted corpora, loaded or noisy runs, errors, queries
/// with a p95 over 1 s, concurrent runs slower than 3 times, shards that differ between runs.
pub fn findings(r: &RunResult) -> Vec<String> {
    let mut f = Vec::new();
    for c in &r.corpora {
        let n = &c.corpus;
        if let Some(a) = &c.aborted {
            f.push(format!("{n}: aborted: {a}"));
        }
        if c.loaded {
            f.push(format!(
                "{n}: measured on a loaded machine; never a baseline or a budget"
            ));
        }
        if c.workload_refreshed {
            f.push(format!(
                "{n}: workload re-derived; not comparable with a baseline"
            ));
        }
        if let Some(i) = &c.index {
            if i.noisy {
                f.push(format!(
                    "{n}: noisy, cold index wall time CV {}",
                    ratio(i.wall_cv)
                ));
            }
            if !i.digests_equal {
                f.push(format!("{n}: shards differ between cold runs"));
            }
        }
        for q in &c.queries {
            if q.errors > 0 {
                let ex = q
                    .error_examples
                    .first()
                    .map(|e| format!(": {e}"))
                    .unwrap_or_default();
                f.push(format!(
                    "{n}: `{}` failed {} time(s){ex}",
                    q.command, q.errors
                ));
            }
            if q.samples > 0 && q.ms.p95 > 1000.0 {
                f.push(format!(
                    "{n}: `{}` p95 {} ms is over 1 s",
                    q.command,
                    ms(q.ms.p95)
                ));
            }
        }
        if let Some(k) = &c.concurrent {
            if k.errors > 0 {
                let ex = k
                    .error_examples
                    .first()
                    .map(|e| format!(": {e}"))
                    .unwrap_or_default();
                f.push(format!("{n}: {} concurrent quer(ies) failed{ex}", k.errors));
            }
            if k.index_slowdown > 3.0 {
                f.push(format!(
                    "{n}: indexing was {}x slower with concurrent queries",
                    ratio(k.index_slowdown)
                ));
            }
            if !k.digest_equal {
                f.push(format!(
                    "{n}: concurrent index shards differ from the isolated run"
                ));
            }
            if !k.doctor_clean {
                f.push(format!("{n}: doctor is not clean after the concurrent run"));
            }
        }
    }
    f
}

#[cfg(test)]
pub(crate) fn sample_run() -> RunResult {
    let mut r = RunResult {
        version: RUN_VERSION,
        run_id: "2026-10-21-S-1".into(),
        date: "2026-10-21".into(),
        machine: MachineProfile {
            os: "linux".into(),
            arch: "x86_64".into(),
            kernel: "6.8".into(),
            cpu_model: "Some CPU".into(),
            logical_cpus: 12,
            mem_gib: 32,
            fs: "ext4".into(),
            katalogr_version: "katalogr 0.1.0 (abc1234)".into(),
            harness_version: "0.1.0".into(),
            load_source: "proc".into(),
        },
        settings: SettingsRecord {
            runs: 3,
            reps: 10,
            targets: 5,
            concurrency: 4,
            max_load: 1.0,
            idle_wait_s: 600,
            allow_loaded: false,
            keep: "all".into(),
            phases: "index,replay,query,concurrent".into(),
            refresh_workload: false,
            budget_gib: 30.0,
            min_free_gib: 10.0,
        },
        corpora: vec![crate::samples::sample_corpus()],
        micro: vec![],
        samples: vec![],
    };
    r.refresh_samples();
    r
}

#[cfg(test)]
#[test]
fn markdown_is_deterministic() {
    let r = sample_run();
    let a = markdown(&r);
    let b = markdown(&r);
    assert_eq!(a, b);
    // Through a JSON round trip too (no dependence on map order or float formatting).
    let again: RunResult = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(markdown(&again), a);
    for h in [
        "## 1. Machine profile",
        "## 4. Index",
        "## 6. Query latency",
        "## 7. Session start",
        "## 8. Concurrent",
        "## 9. Unsupported-language",
        "## 10. Idle re-measure",
        "## 11. Findings",
    ] {
        assert!(a.contains(h), "missing {h}");
    }
    assert!(a.contains("https://example.invalid/demo.git"));
    assert!(a.contains("| demo | find | 50 | "));
    // Findings: a p95 over 1 s, the failing command, the slow concurrent run.
    assert!(a.contains("`find` p95"), "{a}");
    assert!(a.contains("`hubs` failed 2 time(s)"));
    assert!(a.contains("3.80x slower"));
}

#[cfg(test)]
#[test]
fn no_local_paths() {
    let mut r = sample_run();
    let a = markdown(&r);
    // The example error holds an absolute path: it renders as <path>.
    assert!(!a.contains("/home/someone"), "{a}");
    assert!(a.contains("<path>"));
    // The workspace root is replaced at record time.
    assert_eq!(
        redact("failed in /tmp/ws/clones/a: boom", "/tmp/ws"),
        "failed in <ws>/clones/a: boom"
    );
    r.corpora[0].queries[1].error_examples =
        vec![redact("cannot open /tmp/ws/kindex/x", "/tmp/ws")];
    let a = markdown(&r);
    assert!(a.contains("<ws>/kindex/x") || a.contains("<path>"), "{a}");
    assert!(!a.contains("/tmp/ws"));
    // URLs survive; the run file holds no path either.
    assert_eq!(
        scrub("see https://x.invalid/a/b and /usr/lib/x"),
        "see https://x.invalid/a/b and <path>"
    );
    assert_eq!(scrub("a/b is relative"), "a/b is relative");
    let json = serde_json::to_string(&r).unwrap();
    assert!(!json.contains("/tmp/ws"));
}
