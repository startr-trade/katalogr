//! The command builder for the katalogr binary under test, and project set-up.

use camino::{Utf8Path, Utf8PathBuf};
use std::process::Command;

use crate::error::{BenchError, Result};
use crate::manifest::Corpus;
use crate::workspace::Workspace;

#[derive(Debug, Clone)]
pub struct Katalogr {
    pub bin: Utf8PathBuf,
    pub kindex: Utf8PathBuf,
    pub project: String,
}

impl Katalogr {
    fn base(&self) -> Command {
        let mut c = Command::new(&self.bin);
        c.env_clear();
        for k in ["PATH", "HOME"] {
            if let Some(v) = std::env::var_os(k) {
                c.env(k, v);
            }
        }
        c
    }

    /// `<bin> --kindex <kindex> --project <project> --json --timeout 86400 <args...>`, with the
    /// environment cleared except `PATH` and `HOME` (so no `KATALOGR_*` or `RUST_LOG`).
    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = self.base();
        c.args([
            "--kindex",
            self.kindex.as_str(),
            "--project",
            &self.project,
            "--json",
            "--timeout",
            "86400",
        ]);
        c.args(args);
        c
    }

    /// As [`Katalogr::cmd`], run from `dir` (the repository the project is found from).
    pub fn cmd_in(&self, dir: &Utf8Path, args: &[&str]) -> Command {
        let mut c = self.cmd(args);
        c.current_dir(dir);
        c
    }

    /// `kindex init` (idempotent) and, when the project does not exist yet, `project init
    /// <project> --repo n=path ... --mark`; then copies the corpus profile to
    /// `projects/<p>/profiles/<file>`.
    pub fn ensure_project(&self, ws: &Workspace, c: &Corpus) -> Result<()> {
        std::fs::create_dir_all(&self.kindex)?;
        self.run_ok(self.base_kindex(&["kindex", "init", self.kindex.as_str()]))?;
        let project_dir = ws.project_dir(&c.name);
        if !project_dir.join("config.yaml").is_file() {
            let repos: Vec<String> = c
                .repos
                .iter()
                .map(|r| format!("{}={}", r.name, ws.clone_dir(&c.name, &r.name)))
                .collect();
            let mut args: Vec<&str> = vec!["project", "init", &c.name];
            for r in &repos {
                args.push("--repo");
                args.push(r);
            }
            args.push("--mark");
            self.run_ok(self.base_kindex(&args))?;
        }
        if let Some(file) = &c.profile {
            let src = ws.profile(file);
            if !src.is_file() {
                return Err(BenchError::Workspace(format!(
                    "corpus {}: profile `{file}` is missing from the workspace profiles/",
                    c.name
                )));
            }
            let dst_dir = project_dir.join("profiles");
            std::fs::create_dir_all(&dst_dir)?;
            std::fs::copy(&src, dst_dir.join(file))?;
        }
        Ok(())
    }

    fn base_kindex(&self, args: &[&str]) -> Command {
        let mut c = self.base();
        c.args(["--kindex", self.kindex.as_str()]).args(args);
        c
    }

    fn run_ok(&self, mut cmd: Command) -> Result<()> {
        let out = cmd
            .output()
            .map_err(|e| BenchError::Katalogr(format!("cannot start {}: {e}", self.bin)))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(BenchError::Katalogr(format!(
                "{}: {}",
                cmd.get_args()
                    .nth(2)
                    .map(|a| a.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                err.lines().next().unwrap_or("failed")
            )));
        }
        Ok(())
    }

    /// A cold start: delete `shards/` and the cache database. The repository roots recorded in
    /// `.cache/roots.yaml` and the lock file stay, because they are machine configuration and
    /// not index data.
    pub fn reset_index(&self, ws: &Workspace) -> Result<()> {
        let dir = ws.project_dir(&self.project);
        for p in [dir.join("shards")] {
            match std::fs::remove_dir_all(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        let cache = dir.join(".cache");
        if let Ok(rd) = std::fs::read_dir(&cache) {
            for e in rd.flatten() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                if name == "roots.yaml" || name == "processor.lock" {
                    continue;
                }
                let p = e.path();
                if p.is_dir() {
                    std::fs::remove_dir_all(p)?;
                } else {
                    std::fs::remove_file(p)?;
                }
            }
        }
        Ok(())
    }

    /// `katalogr --version`, trimmed.
    pub fn version(&self) -> Result<String> {
        let out = self
            .base()
            .arg("--version")
            .output()
            .map_err(|e| BenchError::Katalogr(format!("cannot start {}: {e}", self.bin)))?;
        if !out.status.success() {
            return Err(BenchError::Katalogr("--version failed".into()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}
