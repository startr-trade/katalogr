//! Cold full index runs.

use super::{Ctx, INDEX_TIMEOUT, IndexResult, mib};
use crate::error::{BenchError, Result};
use crate::proc;
use crate::sizes;
use crate::stats::{self, summary};

/// N cold `index --full` runs: reset, check out the base commits, index, then record wall time,
/// peak RSS, shard and cache sizes and the shard digest. A non-zero exit is an error.
pub fn run(cx: &mut Ctx) -> Result<IndexResult> {
    let runs = cx.s.runs.max(1);
    let mut wall = Vec::new();
    let mut rss = Vec::new();
    let mut digests: Vec<String> = Vec::new();
    let mut last = None;
    for _ in 0..runs {
        cx.gate()?;
        cx.k.reset_index(cx.ws)?;
        cx.checkout_base()?;
        let r = proc::run_with_timeout(&mut cx.k.cmd(&["index", "--full"]), INDEX_TIMEOUT)?;
        cx.after();
        if r.timed_out {
            return Err(BenchError::Katalogr(format!(
                "index --full exceeded {} h",
                INDEX_TIMEOUT.as_secs() / 3600
            )));
        }
        if r.status != Some(0) {
            return Err(BenchError::Katalogr(format!(
                "index --full exited {:?}: {}",
                r.status,
                super::first_line(&r.stderr)
            )));
        }
        wall.push(r.wall.as_secs_f64());
        rss.push(mib(r.peak_rss_kib));
        let dir = cx.project_dir();
        digests.push(sizes::shard_digest(&dir)?);
        last = Some((sizes::shard_stats(&dir)?, sizes::cache_bytes(&dir)?));
    }
    let (shards, cache) = last.expect("at least one run");
    // Files indexed come from the coverage report after the last run.
    let cov = cx.run_json(&["coverage", "--suffixes", "50"])?;
    let files_indexed = cov
        .pointer("/result/totals/files_indexed")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let wall_s = summary(&wall);
    let cv = stats::cv(&wall);
    Ok(IndexResult {
        runs,
        files_indexed,
        files_per_s: if wall_s.p50 > 0.0 {
            files_indexed as f64 / wall_s.p50
        } else {
            0.0
        },
        wall_s,
        peak_rss_mib: summary(&rss),
        shard_files: shards.files,
        shard_bytes: shards.bytes,
        cache_bytes: cache,
        digests_equal: digests.windows(2).all(|w| w[0] == w[1]),
        digest: digests.last().cloned().unwrap_or_default(),
        wall_cv: cv,
        noisy: runs >= 2 && cv > 0.10,
    })
}
