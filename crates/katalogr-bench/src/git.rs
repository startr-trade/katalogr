//! A thin wrapper that spawns `git`.

use camino::{Utf8Path, Utf8PathBuf};

use crate::error::{BenchError, Result};

#[derive(Debug, Clone)]
pub struct Git {
    pub bin: Utf8PathBuf,
}

impl Default for Git {
    fn default() -> Self {
        Git { bin: "git".into() }
    }
}

impl Git {
    /// Run `git <args>` in `cwd` and return trimmed stdout. A non-zero exit gives
    /// `BenchError::Git` with the first line of stderr. Hooks are disabled and prompts are off.
    pub fn run(&self, cwd: &Utf8Path, args: &[&str]) -> Result<String> {
        let out = std::process::Command::new(&self.bin)
            .current_dir(cwd)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "advice.detachedHead=false",
            ])
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| BenchError::Git(format!("cannot start git: {e}")))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let first = err.lines().next().unwrap_or("").trim();
            return Err(BenchError::Git(format!(
                "git {}: {}",
                args.first().copied().unwrap_or(""),
                if first.is_empty() { "failed" } else { first }
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}
