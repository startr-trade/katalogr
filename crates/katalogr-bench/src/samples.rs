//! Measures as `katalogr_analytics::metrics` samples, and the tolerance family of each name.

use katalogr_dist::analytics::metrics::{Better, MetricSample, Tolerance};

use crate::measure::CorpusResult;
use crate::micro::MicroResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Time,
    Memory,
    Size,
    Exact,
    None,
}

/// The family of a sample name (ADR-0026).
pub fn family(name: &str) -> Family {
    match name {
        "load.index.wall_s"
        | "load.index.files_per_s"
        | "load.replay.p50_ms"
        | "load.replay.p95_ms"
        | "load.query.p50_ms"
        | "load.query.p95_ms"
        | "load.session_start.p50_ms"
        | "load.session_start.p95_ms"
        | "load.concurrent.p95_ms"
        | "load.concurrent.index_slowdown"
        | "load.discovery.unclaimed_per_s"
        | "bench.micro_ms"
        | "bench.micro_ratio" => Family::Time,
        "load.index.peak_rss_mib" => Family::Memory,
        "load.index.shard_bytes" | "load.index.cache_bytes" => Family::Size,
        "load.index.deterministic" | "load.query.errors" | "load.concurrent.errors" => {
            Family::Exact
        }
        _ => Family::None,
    }
}

/// Relative 0.15 and 5 ms (0.005 s) for time, relative 0.10 for memory, relative 0.02 for
/// size, nothing for exact samples.
pub fn tolerance(name: &str) -> Tolerance {
    match family(name) {
        Family::Time => {
            let abs = if name.ends_with("_ms") {
                5.0
            } else if name.ends_with("wall_s") {
                0.005
            } else {
                0.0
            };
            Tolerance { abs, rel: 0.15 }
        }
        Family::Memory => Tolerance {
            abs: 0.0,
            rel: 0.10,
        },
        Family::Size => Tolerance {
            abs: 0.0,
            rel: 0.02,
        },
        Family::Exact | Family::None => Tolerance { abs: 0.0, rel: 0.0 },
    }
}

fn s(name: &str, value: f64, better: Better, r: &CorpusResult) -> MetricSample {
    MetricSample::new(name, value, better)
        .label("corpus", r.corpus.clone())
        .label("tier", format!("{:?}", r.tier))
}

/// The `load.*` samples of one corpus.
pub fn corpus_samples(r: &CorpusResult) -> Vec<MetricSample> {
    let mut v = Vec::new();
    if let Some(i) = &r.index {
        v.push(s("load.index.wall_s", i.wall_s.p50, Better::Lower, r));
        v.push(s(
            "load.index.files_per_s",
            i.files_per_s,
            Better::Higher,
            r,
        ));
        v.push(s(
            "load.index.peak_rss_mib",
            i.peak_rss_mib.p50,
            Better::Lower,
            r,
        ));
        v.push(s(
            "load.index.shard_bytes",
            i.shard_bytes as f64,
            Better::Lower,
            r,
        ));
        v.push(s(
            "load.index.cache_bytes",
            i.cache_bytes as f64,
            Better::Lower,
            r,
        ));
        v.push(s(
            "load.index.deterministic",
            if i.digests_equal { 1.0 } else { 0.0 },
            Better::Higher,
            r,
        ));
    }
    if let Some(p) = &r.replay {
        v.push(s("load.replay.p50_ms", p.wall_ms.p50, Better::Lower, r));
        v.push(s("load.replay.p95_ms", p.wall_ms.p95, Better::Lower, r));
    }
    for q in &r.queries {
        if q.command == "session-start" {
            if q.samples > 0 {
                v.push(s("load.session_start.p50_ms", q.ms.p50, Better::Lower, r));
                v.push(s("load.session_start.p95_ms", q.ms.p95, Better::Lower, r));
            }
            continue;
        }
        if q.samples > 0 {
            v.push(
                s("load.query.p50_ms", q.ms.p50, Better::Lower, r).label("op", q.command.clone()),
            );
            v.push(
                s("load.query.p95_ms", q.ms.p95, Better::Lower, r).label("op", q.command.clone()),
            );
        }
        v.push(
            s("load.query.errors", f64::from(q.errors), Better::Lower, r)
                .label("op", q.command.clone()),
        );
    }
    if let Some(c) = &r.concurrent {
        v.push(s("load.concurrent.p95_ms", c.ms.p95, Better::Lower, r));
        v.push(s(
            "load.concurrent.index_slowdown",
            c.index_slowdown,
            Better::Lower,
            r,
        ));
        v.push(s(
            "load.concurrent.errors",
            f64::from(c.errors),
            Better::Lower,
            r,
        ));
    }
    if let Some(d) = &r.discovery {
        if let Some(p) = d.unclaimed_per_s {
            v.push(s("load.discovery.unclaimed_per_s", p, Better::Higher, r));
        }
        v.push(s(
            "load.discovery.files_unclaimed",
            d.files_unclaimed as f64,
            Better::Info,
            r,
        ));
        if let Some(c) = d.text_chunks {
            v.push(s("load.discovery.text_chunks", c as f64, Better::Info, r));
        }
    }
    v
}

/// `bench.micro_ms{id}` or `bench.micro_ratio{id}` (the median), lower is better.
pub fn micro_samples(r: &[MicroResult]) -> Vec<MetricSample> {
    r.iter()
        .map(|m| {
            let name = if m.unit == "ratio" {
                "bench.micro_ratio"
            } else {
                "bench.micro_ms"
            };
            MetricSample::new(name, m.median, Better::Lower).label("id", m.id.clone())
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn sample_corpus() -> CorpusResult {
    use crate::manifest::Tier;
    use crate::measure::*;
    use crate::stats::summary;
    let ms = |xs: &[f64]| summary(xs);
    CorpusResult {
        corpus: "demo".into(),
        tier: Tier::S,
        category: "rust".into(),
        languages: vec!["rust".into()],
        repos: vec![RepoResult {
            name: "main".into(),
            url: "https://example.invalid/demo.git".into(),
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            files_in_tree: 42,
            tree_bytes: 4 << 20,
        }],
        loaded: false,
        load_before: vec![0.2, 0.3],
        load_after: vec![0.3, 0.4],
        index: Some(IndexResult {
            runs: 3,
            wall_s: ms(&[10.0, 10.5, 11.0]),
            files_indexed: 100,
            files_per_s: 9.52,
            peak_rss_mib: ms(&[200.0, 210.0, 205.0]),
            shard_files: 100,
            shard_bytes: 2_000_000,
            cache_bytes: 9_000_000,
            digests_equal: true,
            digest: "ab".repeat(32),
            wall_cv: 0.04,
            noisy: false,
        }),
        replay: Some(ReplayResult {
            commits: 3,
            files_changed: ms(&[1.0, 2.0, 3.0]),
            wall_ms: ms(&[100.0, 120.0, 400.0]),
            peak_rss_mib: ms(&[50.0, 51.0, 52.0]),
        }),
        queries: vec![
            OpResult {
                command: "find".into(),
                samples: 50,
                ms: ms(&[10.0, 12.0, 20.0, 2500.0]),
                peak_rss_mib: ms(&[30.0]),
                errors: 0,
                no_match: 1,
                error_examples: vec![],
            },
            OpResult {
                command: "hubs".into(),
                samples: 10,
                ms: ms(&[100.0, 110.0]),
                peak_rss_mib: ms(&[60.0]),
                errors: 2,
                no_match: 0,
                error_examples: vec![
                    "symbols[7]: absolute path refused at /home/someone/ws/clones/x/a.ts".into(),
                ],
            },
            OpResult {
                command: "session-start".into(),
                samples: 10,
                ms: ms(&[300.0, 320.0]),
                peak_rss_mib: ms(&[60.0]),
                errors: 0,
                no_match: 0,
                error_examples: vec![],
            },
        ],
        concurrent: Some(ConcurrentResult {
            workers: 4,
            index_wall_s: 40.0,
            index_slowdown: 3.8,
            queries: 20,
            errors: 0,
            ms: ms(&[50.0, 400.0]),
            digest_equal: true,
            doctor_clean: true,
            error_examples: vec![],
        }),
        discovery: Some(DiscoveryResult {
            files_seen: 120,
            files_indexed: 100,
            files_unclaimed: 20,
            unclaimed_per_s: Some(12.0),
            text_files: 7,
            text_chunks: Some(33),
            by_suffix: vec![(".go".into(), 15), (".c".into(), 5)],
        }),
        disk: DiskResult {
            clone_bytes: 5 << 20,
            index_bytes: 11 << 20,
            free_before: 100 << 30,
            free_after: 99 << 30,
        },
        workload_refreshed: false,
        aborted: None,
    }
}

#[cfg(test)]
#[test]
fn names_and_directions() {
    use katalogr_dist::analytics::metrics::metric_key;
    let v = corpus_samples(&sample_corpus());
    let get = |key: &str| {
        v.iter()
            .find(|m| metric_key(m) == key)
            .unwrap_or_else(|| panic!("{key}"))
    };
    let b = |name: &str, label: Option<&str>| {
        let key = match label {
            Some(l) => format!("{name}{{corpus=demo,op={l},tier=S}}"),
            None => format!("{name}{{corpus=demo,tier=S}}"),
        };
        get(&key).better
    };
    assert_eq!(b("load.index.wall_s", None), Better::Lower);
    assert_eq!(b("load.index.files_per_s", None), Better::Higher);
    assert_eq!(b("load.index.peak_rss_mib", None), Better::Lower);
    assert_eq!(b("load.index.shard_bytes", None), Better::Lower);
    assert_eq!(b("load.index.cache_bytes", None), Better::Lower);
    assert_eq!(b("load.index.deterministic", None), Better::Higher);
    assert_eq!(
        get("load.index.deterministic{corpus=demo,tier=S}").value,
        1.0
    );
    assert_eq!(b("load.replay.p50_ms", None), Better::Lower);
    assert_eq!(b("load.replay.p95_ms", None), Better::Lower);
    assert_eq!(b("load.query.p50_ms", Some("find")), Better::Lower);
    assert_eq!(b("load.query.p95_ms", Some("find")), Better::Lower);
    assert_eq!(b("load.query.errors", Some("hubs")), Better::Lower);
    assert_eq!(
        get("load.query.errors{corpus=demo,op=hubs,tier=S}").value,
        2.0
    );
    assert_eq!(b("load.session_start.p50_ms", None), Better::Lower);
    assert_eq!(b("load.session_start.p95_ms", None), Better::Lower);
    assert_eq!(b("load.concurrent.p95_ms", None), Better::Lower);
    assert_eq!(b("load.concurrent.index_slowdown", None), Better::Lower);
    assert_eq!(b("load.concurrent.errors", None), Better::Lower);
    assert_eq!(b("load.discovery.unclaimed_per_s", None), Better::Higher);
    assert_eq!(b("load.discovery.files_unclaimed", None), Better::Info);
    assert_eq!(b("load.discovery.text_chunks", None), Better::Info);
    // Session start is not also a query sample.
    assert!(
        !v.iter()
            .any(|m| m.labels.get("op").is_some_and(|o| o == "session-start"))
    );
    // Every name has a family; info samples have none.
    for m in &v {
        let fam = family(&m.name);
        assert_eq!(fam == Family::None, m.better == Better::Info, "{}", m.name);
    }
    assert_eq!(tolerance("load.index.shard_bytes").rel, 0.02);
    assert_eq!(tolerance("load.index.peak_rss_mib").rel, 0.10);
    assert_eq!(tolerance("load.query.p95_ms").abs, 5.0);
    assert_eq!(tolerance("load.query.errors").abs, 0.0);
    let micro = micro_samples(&[MicroResult {
        id: "coverage_100k".into(),
        unit: "ms".into(),
        budget: 1500.0,
        runs: vec![1.0],
        median: 1200.0,
        verdict: crate::micro::Verdict::Met,
        proposed_budget: None,
        item: "FEAT-062".into(),
        req: "R-12".into(),
    }]);
    assert_eq!(micro[0].name, "bench.micro_ms");
    assert_eq!(micro[0].labels["id"], "coverage_100k");
}
