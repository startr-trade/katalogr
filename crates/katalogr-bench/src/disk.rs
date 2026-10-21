//! Disk budget, free-space floor, usage and eviction.

use camino::Utf8Path;
use serde::{Deserialize, Serialize};

use crate::error::{BenchError, Result};
use crate::workspace::Workspace;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
#[value(rename_all = "kebab-case")]
pub enum Keep {
    All,
    Clones,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskPolicy {
    pub budget_bytes: u64,
    pub min_free_bytes: u64,
}

pub const GIB: u64 = 1 << 30;

impl Default for DiskPolicy {
    fn default() -> Self {
        DiskPolicy {
            budget_bytes: 30 * GIB,
            min_free_bytes: 10 * GIB,
        }
    }
}

/// `statvfs` `f_bavail * f_frsize` on unix; elsewhere `u64::MAX` (the check is skipped).
#[cfg(unix)]
pub fn free_bytes(path: &Utf8Path) -> Result<u64> {
    use std::ffi::CString;
    let c = CString::new(path.as_str()).map_err(|_| BenchError::Disk("path holds a NUL".into()))?;
    // SAFETY: `c` is a valid NUL-terminated string and `st` is a plain-old-data out parameter
    // that statvfs fills in on success.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        return Err(BenchError::Io(std::io::Error::last_os_error()));
    }
    Ok((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

#[cfg(not(unix))]
pub fn free_bytes(_path: &Utf8Path) -> Result<u64> {
    Ok(u64::MAX)
}

/// Recursive sum of file sizes (symlinks not followed). A missing path is 0.
pub fn usage_bytes(path: &Utf8Path) -> Result<u64> {
    let md = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    if md.is_file() {
        return Ok(md.len());
    }
    if !md.is_dir() {
        return Ok(0);
    }
    let mut total = 0;
    for e in std::fs::read_dir(path)? {
        let e = e?;
        let p = camino::Utf8PathBuf::from_path_buf(e.path())
            .map_err(|p| BenchError::Disk(format!("non-UTF-8 path {}", p.display())))?;
        total += usage_bytes(&p)?;
    }
    Ok(total)
}

/// Refuse when `usage + need` exceeds the budget, or `free - need` is below the floor. `need`
/// is `approx.disk_gib` minus what this corpus already uses in the workspace.
pub fn check(p: &DiskPolicy, usage: u64, free: u64, need: u64) -> Result<()> {
    if usage.saturating_add(need) > p.budget_bytes {
        return Err(BenchError::Disk(format!(
            "workspace uses {:.1} GiB and the corpus needs {:.1} GiB, over the {:.1} GiB budget",
            usage as f64 / GIB as f64,
            need as f64 / GIB as f64,
            p.budget_bytes as f64 / GIB as f64
        )));
    }
    if free.saturating_sub(need) < p.min_free_bytes {
        return Err(BenchError::Disk(format!(
            "{:.1} GiB free and the corpus needs {:.1} GiB, below the {:.1} GiB free-space floor",
            free as f64 / GIB as f64,
            need as f64 / GIB as f64,
            p.min_free_bytes as f64 / GIB as f64
        )));
    }
    Ok(())
}

/// What a corpus still needs: its estimate minus what it already uses.
pub fn need_bytes(approx_disk_gib: f64, already_used: u64) -> u64 {
    ((approx_disk_gib * GIB as f64) as u64).saturating_sub(already_used)
}

fn remove_dir(p: &Utf8Path) -> Result<()> {
    match std::fs::remove_dir_all(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// After measuring: `All` leaves everything; `Clones` deletes `kindex/projects/<c>`; `None`
/// deletes that and `clones/<c>`.
pub fn evict(ws: &Workspace, corpus: &str, keep: Keep) -> Result<()> {
    match keep {
        Keep::All => {}
        Keep::Clones => remove_dir(&ws.project_dir(corpus))?,
        Keep::None => {
            remove_dir(&ws.project_dir(corpus))?;
            remove_dir(&ws.clones(corpus))?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[test]
fn refuses_over_budget() {
    let p = DiskPolicy {
        budget_bytes: 30 * GIB,
        min_free_bytes: 10 * GIB,
    };
    assert!(check(&p, 20 * GIB, 100 * GIB, 5 * GIB).is_ok());
    let e = check(&p, 28 * GIB, 100 * GIB, 3 * GIB).unwrap_err();
    assert_eq!(e.exit_code(), 3);
    assert!(e.to_string().contains("budget"), "{e}");
    assert_eq!(need_bytes(3.0, GIB), 2 * GIB);
    assert_eq!(need_bytes(1.0, 5 * GIB), 0);
}

#[cfg(test)]
#[test]
fn refuses_below_free_floor() {
    let p = DiskPolicy {
        budget_bytes: 30 * GIB,
        min_free_bytes: 10 * GIB,
    };
    let e = check(&p, 0, 12 * GIB, 3 * GIB).unwrap_err();
    assert_eq!(e.exit_code(), 3);
    assert!(e.to_string().contains("free-space floor"), "{e}");
    assert!(check(&p, 0, 13 * GIB, 3 * GIB).is_ok());
}

#[cfg(test)]
#[test]
fn evict_keeps_clones() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = Workspace::open(Some(Utf8Path::from_path(tmp.path()).unwrap())).unwrap();
    let mk = || {
        std::fs::create_dir_all(ws.project_dir("c1").join("shards")).unwrap();
        std::fs::write(ws.project_dir("c1").join("shards/a"), b"12345").unwrap();
        std::fs::create_dir_all(ws.clone_dir("c1", "r")).unwrap();
        std::fs::write(ws.clone_dir("c1", "r").join("f"), b"123").unwrap();
    };
    mk();
    evict(&ws, "c1", Keep::All).unwrap();
    assert!(ws.project_dir("c1").exists() && ws.clones("c1").exists());
    evict(&ws, "c1", Keep::Clones).unwrap();
    assert!(!ws.project_dir("c1").exists());
    assert_eq!(usage_bytes(&ws.clones("c1")).unwrap(), 3);
    mk();
    evict(&ws, "c1", Keep::None).unwrap();
    assert!(!ws.project_dir("c1").exists() && !ws.clones("c1").exists());
    // Evicting nothing is fine.
    evict(&ws, "c1", Keep::None).unwrap();
    assert!(free_bytes(ws.root()).unwrap() > 0);
}
