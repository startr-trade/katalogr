//! Incremental reindex over the next real commits of the primary repository.

use super::{Ctx, REPLAY_TIMEOUT, ReplayResult, mib};
use crate::error::{BenchError, Result};
use crate::fetch;
use crate::proc;
use crate::stats::summary;

/// Each commit `c_i` of the replay chain after the base is checked out, then
/// `index --range <c_{i-1}>..<c_i>` runs from the primary clone. Wall time, peak RSS and
/// `git diff --name-only` files changed are recorded per commit. The index must already be
/// at the base commit (the index phase leaves it there). Afterwards the clone is at the head
/// commit, which is the state the queries run against.
pub fn run(cx: &mut Ctx) -> Result<ReplayResult> {
    let chain = cx.primary().replay.clone();
    let repo = cx.primary().repo.clone();
    let dir = cx.ws.clone_dir(&cx.c.name, &repo);
    let mut wall_ms = Vec::new();
    let mut rss = Vec::new();
    let mut changed = Vec::new();
    for pair in chain.windows(2) {
        let (prev, cur) = (&pair[0], &pair[1]);
        fetch::checkout(cx.git, &dir, cur)?;
        let files = cx
            .git
            .run(&dir, &["diff", "--name-only", prev, cur])?
            .lines()
            .filter(|l| !l.is_empty())
            .count();
        cx.gate()?;
        let range = format!("{prev}..{cur}");
        let r = proc::run_with_timeout(
            &mut cx.k.cmd_in(&dir, &["index", "--range", &range]),
            REPLAY_TIMEOUT,
        )?;
        cx.after();
        if r.status != Some(0) {
            return Err(BenchError::Katalogr(format!(
                "index --range exited {:?}: {}",
                r.status,
                super::first_line(&r.stderr)
            )));
        }
        wall_ms.push(r.wall.as_secs_f64() * 1000.0);
        rss.push(mib(r.peak_rss_kib));
        changed.push(files as f64);
    }
    cx.checkout_head()?;
    Ok(ReplayResult {
        commits: wall_ms.len() as u32,
        files_changed: summary(&changed),
        wall_ms: summary(&wall_ms),
        peak_rss_mib: summary(&rss),
    })
}
