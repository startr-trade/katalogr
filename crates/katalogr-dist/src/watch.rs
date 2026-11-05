//! `katalogr watch` (FEAT-039 R-2): keep the index fresh for edits made outside Claude.
//!
//! A tick reads each checkout's position with the stat cache (`read_position_cached`), which is
//! cheap when nothing changed. Only when a position differs from the last tick's does the tick
//! open the engine and let it reconcile and process, exactly as a read at wait-all would. An
//! idle tick opens no engine and takes no lock, so it never blocks a CLI.

use crate::Katalogr;
use crate::error::DistError;
use katalogr_engine::position::{read_position_cached, stat_cache_path};
use katalogr_engine::{Consistency as EngineConsistency, QueryOptions, Scope as EngineScope};
use katalogr_query::PositionRef;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Shortest and longest `--interval`, in seconds.
pub const MIN_INTERVAL_SECS: f64 = 0.1;
pub const MAX_INTERVAL_SECS: f64 = 3600.0;
/// Default for `watch.interval`, in seconds (provisional; FEAT-039 T8 records a tick's cost).
pub const DEFAULT_INTERVAL_SECS: f64 = 2.0;
/// Seconds one tick may spend processing what it found.
const PROCESS_SECS: u64 = 30;

/// What one tick did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchTick {
    /// No checkout's position differs from the last tick's.
    Idle,
    /// A position differed; the index was brought up to date. `pending` is what was still
    /// queued afterwards (0 unless the wait timed out).
    Processed { pending: u64 },
    /// The tick failed; the loop goes on and the next tick tries again.
    Failed(String),
}

/// The interval as a duration, or a usage error outside 0.1 s to 3600 s.
pub fn interval_of(secs: f64) -> Result<Duration, DistError> {
    if !(MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(&secs) {
        return Err(DistError::usage(format!(
            "invalid --interval {secs}: use {MIN_INTERVAL_SECS} to {MAX_INTERVAL_SECS} seconds"
        )));
    }
    Duration::try_from_secs_f64(secs)
        .map_err(|_| DistError::usage(format!("invalid --interval {secs}")))
}

/// Poll the positions every `interval`; when one changed, queue the differences and process.
/// Returns when `stop` is set. A failed tick is reported and the loop goes on.
pub fn watch(
    k: &Katalogr,
    interval: Duration,
    stop: &AtomicBool,
    report: &mut dyn FnMut(WatchTick),
) -> Result<(), DistError> {
    let mut last: Option<Vec<PositionRef>> = None;
    while !stop.load(Ordering::Relaxed) {
        let tick = match k.watch_tick(&mut last) {
            Ok(t) => t,
            Err(e) => WatchTick::Failed(e.to_string()),
        };
        report(tick);
        let until = Instant::now() + interval;
        while !stop.load(Ordering::Relaxed) {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            std::thread::sleep(left.min(Duration::from_millis(50)));
        }
    }
    Ok(())
}

impl Katalogr {
    /// A numeric project setting, if set.
    pub fn setting_f64(&self, key: &str) -> Option<f64> {
        self.project
            .config()
            .settings
            .get(key)
            .and_then(serde_yaml_ng::Value::as_f64)
    }

    /// One tick. `last` is the positions the previous successful tick saw; it is replaced only
    /// when this tick succeeds, so a failure is retried.
    fn watch_tick(&self, last: &mut Option<Vec<PositionRef>>) -> Result<WatchTick, DistError> {
        let deadline = Instant::now() + Duration::from_secs(PROCESS_SECS);
        let cache = self.layer_dir(deadline)?.cache_dir().into_std_path_buf();
        let mut now = Vec::new();
        for (repo, root) in &self.roots {
            let (include, exclude) = self
                .project
                .config()
                .repos
                .iter()
                .find(|r| &r.name == repo)
                .map_or((Vec::new(), Vec::new()), |r| {
                    (r.include.clone(), r.exclude.clone())
                });
            let state = read_position_cached(
                repo,
                root.as_std_path(),
                &include,
                &exclude,
                Some(&stat_cache_path(&cache, repo)),
            )
            .map_err(|e| DistError::usage(format!("{repo}: {e}")))?;
            now.push(state.position);
        }
        now.sort();
        if last.as_ref() == Some(&now) {
            return Ok(WatchTick::Idle);
        }
        let engine = self.engine_for(EngineConsistency::WaitAll, deadline)?;
        let answer = engine.with_consistency(
            &QueryOptions {
                consistency: EngineConsistency::WaitAll,
                timeout: Duration::from_secs(PROCESS_SECS),
                scope: EngineScope::Project,
            },
            |_| (),
        )?;
        *last = Some(now);
        Ok(WatchTick::Processed {
            pending: answer.pending,
        })
    }
}

/// [`DEFAULT_INTERVAL_SECS`] as a duration.
pub fn default_interval() -> Duration {
    Duration::from_secs_f64(DEFAULT_INTERVAL_SECS)
}
