#![cfg(feature = "web")]
//! FEAT-126 R-1: a spawned server is stopped when its guard goes out of scope, panic or not.

#[path = "support/server_guard.rs"]
mod support;

#[path = "views_support/mod.rs"]
mod project_support;

mod server_guard {
    use super::support::ServerGuard;
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU32, Ordering};

    static PID: AtomicU32 = AtomicU32::new(0);

    #[cfg(unix)]
    fn alive(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    /// Windows has no `kill -0` (the `kill` on a runner's PATH is MSYS's, which takes MSYS
    /// process ids, not Windows ones); `tasklist` lists the process while it exists.
    #[cfg(windows)]
    fn alive(pid: u32) -> bool {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .stderr(Stdio::null())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
    }

    #[test]
    fn killed_on_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = super::project_support::project(tmp.path());
        let result = std::panic::catch_unwind(|| {
            let mut child = Command::new(env!("CARGO_BIN_EXE_katalogr"))
                .current_dir(&repo)
                .env_remove("KATALOGR_KINDEX")
                .env_remove("KATALOGR_PROJECT")
                .env_remove("KANBANR_DATA_DIR")
                .args(["serve", "--listen", "127.0.0.1:0"])
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let stderr = child.stderr.take().unwrap();
            let guard = ServerGuard::new(child);
            // Wait until it is really serving, then fail the way a bench assertion would.
            let started = BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
                .any(|l| l.contains("listening on http://"));
            assert!(started, "server never started");
            assert!(alive(guard.id()), "server not running before the panic");
            PID.store(guard.id(), Ordering::SeqCst);
            panic!("assertion fails while the server runs");
        });
        assert!(result.is_err());
        let pid = PID.load(Ordering::SeqCst);
        assert!(pid != 0, "server never spawned");
        // kill(pid, 0) fails once the process is gone and reaped.
        assert!(!alive(pid), "server {pid} still running after panic");
    }
}
