//! Idle protocol: wait until the 1-minute load average is low enough to measure.

use std::time::Duration;

use crate::error::{BenchError, Result};

pub trait LoadSource {
    fn load1(&self) -> Result<f64>;
}

/// First field of `/proc/loadavg`; elsewhere `Ok(0.0)` and `machine.load_source = "none"`.
pub struct ProcLoadavg;

impl LoadSource for ProcLoadavg {
    fn load1(&self) -> Result<f64> {
        match std::fs::read_to_string("/proc/loadavg") {
            Ok(s) => s
                .split_whitespace()
                .next()
                .and_then(|f| f.parse::<f64>().ok())
                .ok_or_else(|| BenchError::Io(std::io::Error::other("unreadable /proc/loadavg"))),
            Err(_) => Ok(0.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IdleOutcome {
    Idle { load: f64 },
    Loaded { load: f64 },
}

impl IdleOutcome {
    pub fn load(&self) -> f64 {
        match self {
            IdleOutcome::Idle { load } | IdleOutcome::Loaded { load } => *load,
        }
    }
    pub fn is_loaded(&self) -> bool {
        matches!(self, IdleOutcome::Loaded { .. })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct IdlePolicy {
    pub max_load: f64,
    pub wait: Duration,
    pub poll: Duration,
    pub allow_loaded: bool,
}

impl Default for IdlePolicy {
    fn default() -> Self {
        IdlePolicy {
            max_load: 1.0,
            wait: Duration::from_secs(600),
            poll: Duration::from_secs(10),
            allow_loaded: false,
        }
    }
}

/// Poll until `load1 <= max_load` (Idle) or `wait` has elapsed. On timeout: Loaded when
/// `allow_loaded`, else `Err(NotIdle)`. `sleep` is injected for tests; time is counted in
/// polls, so the total wait is `ceil(wait / poll)` polls.
pub fn wait_idle(
    src: &dyn LoadSource,
    p: &IdlePolicy,
    sleep: &mut dyn FnMut(Duration),
) -> Result<IdleOutcome> {
    let mut waited = Duration::ZERO;
    loop {
        let load = src.load1()?;
        if load <= p.max_load {
            return Ok(IdleOutcome::Idle { load });
        }
        if waited >= p.wait {
            return if p.allow_loaded {
                Ok(IdleOutcome::Loaded { load })
            } else {
                Err(BenchError::NotIdle {
                    load,
                    max: p.max_load,
                    waited_s: waited.as_secs(),
                })
            };
        }
        sleep(p.poll);
        waited += p.poll;
    }
}

#[cfg(test)]
struct Scripted(std::cell::RefCell<Vec<f64>>);

#[cfg(test)]
impl LoadSource for Scripted {
    fn load1(&self) -> Result<f64> {
        let mut v = self.0.borrow_mut();
        Ok(if v.len() > 1 { v.remove(0) } else { v[0] })
    }
}

#[cfg(test)]
fn policy(allow: bool) -> IdlePolicy {
    IdlePolicy {
        max_load: 1.0,
        wait: Duration::from_secs(60),
        poll: Duration::from_secs(10),
        allow_loaded: allow,
    }
}

#[cfg(test)]
#[test]
fn waits_until_idle() {
    let src = Scripted(std::cell::RefCell::new(vec![3.0, 2.0, 0.5]));
    let mut sleeps = 0;
    let out = wait_idle(&src, &policy(false), &mut |_| sleeps += 1).unwrap();
    assert_eq!(out, IdleOutcome::Idle { load: 0.5 });
    assert_eq!(sleeps, 2);
}

#[cfg(test)]
#[test]
fn aborts_when_never_idle() {
    let src = Scripted(std::cell::RefCell::new(vec![4.0]));
    let mut sleeps = 0;
    let e = wait_idle(&src, &policy(false), &mut |_| sleeps += 1).unwrap_err();
    assert!(matches!(e, BenchError::NotIdle { .. }), "{e}");
    assert_eq!(e.exit_code(), 3);
    assert_eq!(sleeps, 6);
}

#[cfg(test)]
#[test]
fn loaded_run_is_marked() {
    let src = Scripted(std::cell::RefCell::new(vec![4.0]));
    let out = wait_idle(&src, &policy(true), &mut |_| {}).unwrap();
    assert_eq!(out, IdleOutcome::Loaded { load: 4.0 });
    assert!(out.is_loaded());
}
