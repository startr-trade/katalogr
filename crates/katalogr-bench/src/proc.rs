//! Child process runner: wall time, exit status, captured output and the child's peak
//! resident memory from `wait4` rusage (Linux).

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::error::{BenchError, Result};

#[derive(Debug, Clone)]
pub struct RunStats {
    pub wall: Duration,
    /// Exit code; `None` when killed by a signal (including a timeout kill).
    pub status: Option<i32>,
    /// `ru_maxrss` in KiB; `None` where `wait4` is not available.
    pub peak_rss_kib: Option<u64>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// The per-child timeout killed the child.
    pub timed_out: bool,
}

/// A spawned child that has not been reaped yet.
pub struct Running {
    child: std::process::Child,
    start: Instant,
    out: Option<JoinHandle<Vec<u8>>>,
    err: Option<JoinHandle<Vec<u8>>>,
    /// Dropping the sender (or sending) releases the timeout watcher.
    done: Option<mpsc::Sender<()>>,
    watcher: Option<JoinHandle<bool>>,
}

fn drain<R: Read + Send + 'static>(r: Option<R>) -> Option<JoinHandle<Vec<u8>>> {
    r.map(|mut r| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf);
            buf
        })
    })
}

/// Spawn, drain stdout and stderr on two threads and return without waiting. `timeout` kills
/// the child with SIGKILL when it runs longer.
pub fn spawn_with(cmd: &mut Command, timeout: Option<Duration>) -> Result<Running> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| BenchError::Katalogr(format!("cannot start {:?}: {e}", cmd.get_program())))?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let (done, watcher) = match timeout {
        Some(t) => {
            let (tx, rx) = mpsc::channel::<()>();
            let pid = child.id();
            let h = std::thread::spawn(move || {
                if rx.recv_timeout(t).is_err()
                    && matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty))
                {
                    kill(pid);
                    true
                } else {
                    false
                }
            });
            (Some(tx), Some(h))
        }
        None => (None, None),
    };
    Ok(Running {
        child,
        start,
        out,
        err,
        done,
        watcher,
    })
}

pub fn spawn(cmd: &mut Command) -> Result<Running> {
    spawn_with(cmd, None)
}

#[cfg(unix)]
fn kill(pid: u32) {
    // SAFETY: plain kill(2) on a child pid this process spawned and has not reaped yet (the
    // watcher is released before the reap completes its result is used).
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill(_pid: u32) {}

impl Running {
    /// Reap the child and collect its output.
    pub fn finish(mut self) -> Result<RunStats> {
        let (status, rss) = reap(&mut self.child)?;
        let wall = self.start.elapsed();
        drop(self.done.take());
        let timed_out = self
            .watcher
            .take()
            .map(|h| h.join().unwrap_or(false))
            .unwrap_or(false);
        let stdout = self
            .out
            .take()
            .map(|h| h.join().unwrap_or_default())
            .unwrap_or_default();
        let stderr = self
            .err
            .take()
            .map(|h| h.join().unwrap_or_default())
            .unwrap_or_default();
        Ok(RunStats {
            wall,
            status,
            peak_rss_kib: rss,
            stdout,
            stderr,
            timed_out,
        })
    }
}

#[cfg(target_os = "linux")]
fn reap(child: &mut std::process::Child) -> Result<(Option<i32>, Option<u64>)> {
    let pid = child.id() as libc::pid_t;
    let mut status: libc::c_int = 0;
    // SAFETY: `rusage` is plain data filled in by wait4; `pid` is our own unreaped child.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    loop {
        let rc = unsafe { libc::wait4(pid, &mut status, 0, &mut ru) };
        if rc == pid {
            break;
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(BenchError::Io(e));
        }
    }
    let code = if libc::WIFEXITED(status) {
        Some(libc::WEXITSTATUS(status))
    } else {
        None
    };
    Ok((code, Some(ru.ru_maxrss as u64)))
}

#[cfg(not(target_os = "linux"))]
fn reap(child: &mut std::process::Child) -> Result<(Option<i32>, Option<u64>)> {
    let st = child.wait()?;
    Ok((st.code(), None))
}

/// Spawn and reap. `wall` is measured from just before spawn to just after the reap.
pub fn run(cmd: &mut Command) -> Result<RunStats> {
    spawn(cmd)?.finish()
}

pub fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<RunStats> {
    spawn_with(cmd, Some(timeout))?.finish()
}

#[cfg(test)]
#[test]
fn peak_rss_of_child() {
    const VAR: &str = "KATALOGR_BENCH_ALLOC_MIB";
    if let Ok(mib) = std::env::var(VAR) {
        let n = mib.parse::<usize>().unwrap() << 20;
        let mut v = vec![0u8; n];
        for i in (0..n).step_by(4096) {
            v[i] = 1;
        }
        std::hint::black_box(&v);
        // Exit before the harness runs anything else.
        std::process::exit(0);
    }
    let exe = std::env::current_exe().unwrap();
    let mut cmd = Command::new(exe);
    cmd.args(["--exact", "proc::peak_rss_of_child", "--nocapture"])
        .env(VAR, "64");
    let r = run(&mut cmd).unwrap();
    assert_eq!(r.status, Some(0));
    if cfg!(target_os = "linux") {
        assert!(r.peak_rss_kib.unwrap() >= 60 * 1024, "{:?}", r.peak_rss_kib);
    } else {
        assert!(r.peak_rss_kib.is_none());
    }
}

#[cfg(test)]
#[test]
fn timeout_kills_child() {
    let mut cmd = Command::new("sleep");
    cmd.arg("30");
    let r = run_with_timeout(&mut cmd, Duration::from_millis(200)).unwrap();
    assert!(r.timed_out);
    assert_eq!(r.status, None);
    assert!(r.wall < Duration::from_secs(5));
    let mut cmd = Command::new("echo");
    cmd.arg("hi");
    let r = run_with_timeout(&mut cmd, Duration::from_secs(30)).unwrap();
    assert!(!r.timed_out);
    assert_eq!(String::from_utf8_lossy(&r.stdout).trim(), "hi");
}
