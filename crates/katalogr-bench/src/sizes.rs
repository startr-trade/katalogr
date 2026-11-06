//! Cache size, the graph digest, and whether an index has published: all read from the
//! project's graph database or through the binary. There are no shard files to measure
//! (FEAT-094).

use camino::{Utf8Path, Utf8PathBuf};
use std::time::Duration;

use crate::error::{BenchError, Result};
use crate::katalogr::Katalogr;
use crate::proc;

fn walk(dir: &Utf8Path, out: &mut Vec<(Utf8PathBuf, u64)>) -> Result<()> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for e in rd {
        let e = e?;
        let ft = e.file_type()?;
        let p = Utf8PathBuf::from_path_buf(e.path()).map_err(|p| {
            BenchError::Io(std::io::Error::other(format!(
                "non-UTF-8 path {}",
                p.display()
            )))
        })?;
        if ft.is_dir() {
            walk(&p, out)?;
        } else if ft.is_file() {
            out.push((p, e.metadata()?.len()));
        }
    }
    Ok(())
}

/// Total bytes under `.cache/`.
pub fn cache_bytes(project_dir: &Utf8Path) -> Result<u64> {
    let mut v = Vec::new();
    walk(&project_dir.join(".cache"), &mut v)?;
    Ok(v.iter().map(|(_, n)| n).sum())
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(120);

/// The SHA-256 of the project's canonical graph, as hex, from `audit record --json`. The
/// command waits for the index to catch up and writes a small record in the project's
/// `audit/` folder, which is part of the benchmark's own workspace.
pub fn graph_digest(k: &Katalogr) -> Result<String> {
    let r = proc::run_with_timeout(&mut k.cmd(&["audit", "record"]), PROBE_TIMEOUT)?;
    if r.status != Some(0) {
        return Err(BenchError::Katalogr(format!(
            "audit record: exit {:?}: {}",
            r.status,
            crate::measure::first_line(&r.stderr)
        )));
    }
    let v: serde_json::Value = serde_json::from_slice(&r.stdout)
        .map_err(|e| BenchError::Katalogr(format!("audit record: bad JSON: {e}")))?;
    v.get("graph_digest")
        .and_then(|d| d.as_str())
        .map(str::to_string)
        .ok_or_else(|| BenchError::Katalogr("audit record: no graph_digest".into()))
}

/// Whether a round has published: the worktree watermark of `status` is above zero. A project
/// with no cache yet, or a query that fails, is not published.
pub fn first_round_published(k: &Katalogr) -> bool {
    let Ok(r) = proc::run_with_timeout(
        &mut k.cmd(&["--consistency", "stale-ok", "status"]),
        PROBE_TIMEOUT,
    ) else {
        return false;
    };
    r.status == Some(0)
        && serde_json::from_slice::<serde_json::Value>(&r.stdout)
            .ok()
            .and_then(|v| v.get("watermark").and_then(|w| w.as_u64()))
            .is_some_and(|w| w > 0)
}

#[cfg(test)]
#[test]
fn cache_bytes_counts_the_cache_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = Utf8Path::from_path(tmp.path()).unwrap();
    std::fs::create_dir_all(dir.join(".cache")).unwrap();
    std::fs::write(dir.join(".cache/index.redb"), vec![0u8; 100]).unwrap();
    std::fs::write(dir.join(".cache/roots.yaml"), vec![0u8; 5]).unwrap();
    assert_eq!(cache_bytes(dir).unwrap(), 105);
    // No project: zero.
    assert_eq!(cache_bytes(&dir.join("nothing")).unwrap(), 0);
}
