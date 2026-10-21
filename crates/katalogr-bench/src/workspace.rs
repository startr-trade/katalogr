//! The load-test workspace: clones, kindex, workloads, results and baselines, outside any
//! katalogr checkout.

use camino::{Utf8Path, Utf8PathBuf};

use crate::error::{BenchError, Result};

pub const ENV_HOME: &str = "KATALOGR_LOADTEST_HOME";

#[derive(Debug, Clone)]
pub struct Workspace {
    root: Utf8PathBuf,
}

/// Holds the exclusive workspace lock until dropped.
#[derive(Debug)]
pub struct LockGuard {
    _file: std::fs::File,
}

/// The default root: `$KATALOGR_LOADTEST_HOME`, else `$HOME/.katalogr-loadtest`.
pub fn default_root(env_home: Option<&str>, home: Option<&str>) -> Result<Utf8PathBuf> {
    if let Some(h) = env_home.filter(|h| !h.is_empty()) {
        return Ok(Utf8PathBuf::from(h));
    }
    match home.filter(|h| !h.is_empty()) {
        Some(h) => Ok(Utf8PathBuf::from(h).join(".katalogr-loadtest")),
        None => Err(BenchError::Workspace(
            "no workspace: pass --workspace or set KATALOGR_LOADTEST_HOME or HOME".into(),
        )),
    }
}

/// True when `dir` or an ancestor holds `crates/katalogr-bench/Cargo.toml`.
fn inside_checkout(dir: &Utf8Path) -> bool {
    dir.ancestors()
        .any(|a| a.join("crates/katalogr-bench/Cargo.toml").is_file())
}

impl Workspace {
    /// `--workspace`, else the default root. Creates the directory. Refuses
    /// (`BenchError::Workspace`) when the root or any ancestor contains
    /// `crates/katalogr-bench/Cargo.toml`, so it is never inside a katalogr checkout.
    pub fn open(explicit: Option<&Utf8Path>) -> Result<Workspace> {
        let root = match explicit {
            Some(p) => p.to_path_buf(),
            None => default_root(
                std::env::var(ENV_HOME).ok().as_deref(),
                std::env::var("HOME").ok().as_deref(),
            )?,
        };
        let root = if root.is_absolute() {
            root
        } else {
            let cwd = std::env::current_dir().map_err(BenchError::Io)?;
            let cwd = Utf8PathBuf::from_path_buf(cwd).map_err(|p| {
                BenchError::Workspace(format!("non-UTF-8 directory {}", p.display()))
            })?;
            cwd.join(root)
        };
        let refuse = |root: &Utf8Path| {
            BenchError::Workspace(format!(
                "{root} is inside a katalogr checkout; the workspace must live outside the repository"
            ))
        };
        // Refuse before anything is created.
        if inside_checkout(&root) {
            return Err(refuse(&root));
        }
        std::fs::create_dir_all(&root)
            .map_err(|e| BenchError::Workspace(format!("cannot create {root}: {e}")))?;
        let root = Utf8PathBuf::from_path_buf(
            root.as_std_path()
                .canonicalize()
                .map_err(|e| BenchError::Workspace(format!("cannot resolve {root}: {e}")))?,
        )
        .map_err(|p| BenchError::Workspace(format!("non-UTF-8 path {}", p.display())))?;
        if inside_checkout(&root) {
            return Err(refuse(&root));
        }
        Ok(Workspace { root })
    }

    /// Take the exclusive `.lock`; a second holder gets exit 3.
    pub fn lock(&self) -> Result<LockGuard> {
        use fs4::FileExt;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.root.join(".lock"))?;
        match FileExt::try_lock(&file) {
            Ok(()) => Ok(LockGuard { _file: file }),
            Err(fs4::TryLockError::WouldBlock) => {
                Err(BenchError::Busy("another load test is running".into()))
            }
            Err(fs4::TryLockError::Error(e)) => Err(BenchError::Io(e)),
        }
    }

    pub fn root(&self) -> &Utf8Path {
        &self.root
    }
    pub fn manifest(&self) -> Utf8PathBuf {
        self.root.join("corpora.yaml")
    }
    pub fn profile(&self, file: &str) -> Utf8PathBuf {
        self.root.join("profiles").join(file)
    }
    pub fn clones(&self, corpus: &str) -> Utf8PathBuf {
        self.root.join("clones").join(corpus)
    }
    pub fn clone_dir(&self, corpus: &str, repo: &str) -> Utf8PathBuf {
        self.clones(corpus).join(repo)
    }
    pub fn kindex(&self) -> Utf8PathBuf {
        self.root.join("kindex")
    }
    pub fn project_dir(&self, corpus: &str) -> Utf8PathBuf {
        self.kindex().join("projects").join(corpus)
    }
    pub fn workload(&self, corpus: &str) -> Utf8PathBuf {
        self.root.join("workloads").join(format!("{corpus}.json"))
    }
    pub fn results(&self, run_id: &str) -> Utf8PathBuf {
        self.root.join("results").join(run_id)
    }
    pub fn baseline(&self, name: &str) -> Utf8PathBuf {
        self.root.join("baselines").join(format!("{name}.json"))
    }

    /// Create the layout directories.
    pub fn init_layout(&self) -> Result<()> {
        for d in ["profiles", "clones", "workloads", "results", "baselines"] {
            std::fs::create_dir_all(self.root.join(d))?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[test]
fn refuses_inside_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let base = Utf8PathBuf::from_path_buf(tmp.path().canonicalize().unwrap()).unwrap();
    std::fs::create_dir_all(base.join("crates/katalogr-bench")).unwrap();
    std::fs::write(base.join("crates/katalogr-bench/Cargo.toml"), "").unwrap();
    let inside = base.join("a/b/ws");
    let e = Workspace::open(Some(&inside)).unwrap_err();
    assert!(matches!(e, BenchError::Workspace(_)), "{e}");
    assert_eq!(e.exit_code(), 2);
    assert!(e.to_string().contains("inside a katalogr checkout"));
    // Outside a checkout it opens and a second lock is refused with exit 3.
    let other = tempfile::tempdir().unwrap();
    let ws = Workspace::open(Some(Utf8Path::from_path(other.path()).unwrap())).unwrap();
    let _g = ws.lock().unwrap();
    assert_eq!(ws.lock().unwrap_err().exit_code(), 3);
}

#[cfg(test)]
#[test]
fn default_location() {
    // KATALOGR_LOADTEST_HOME wins over HOME.
    assert_eq!(
        default_root(Some("/x/lt"), Some("/home/u")).unwrap(),
        Utf8PathBuf::from("/x/lt")
    );
    assert_eq!(
        default_root(None, Some("/home/u")).unwrap(),
        Utf8PathBuf::from("/home/u/.katalogr-loadtest")
    );
    assert!(default_root(None, None).is_err());
}
