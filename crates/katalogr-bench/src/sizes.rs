//! Shard and cache sizes, and the digest of a project's shard tree.

use camino::{Utf8Path, Utf8PathBuf};
use sha2::{Digest, Sha256};

use crate::error::{BenchError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShardStats {
    pub files: u64,
    pub bytes: u64,
}

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

fn shard_files(project_dir: &Utf8Path) -> Result<Vec<(Utf8PathBuf, u64)>> {
    let mut v = Vec::new();
    walk(&project_dir.join("shards"), &mut v)?;
    v.retain(|(p, _)| p.as_str().ends_with(".kshard.json"));
    v.sort();
    Ok(v)
}

/// Count and bytes of `shards/**/*.kshard.json`.
pub fn shard_stats(project_dir: &Utf8Path) -> Result<ShardStats> {
    let v = shard_files(project_dir)?;
    Ok(ShardStats {
        files: v.len() as u64,
        bytes: v.iter().map(|(_, n)| n).sum(),
    })
}

/// Total bytes under `.cache/`.
pub fn cache_bytes(project_dir: &Utf8Path) -> Result<u64> {
    let mut v = Vec::new();
    walk(&project_dir.join(".cache"), &mut v)?;
    Ok(v.iter().map(|(_, n)| n).sum())
}

/// SHA-256 over (relative path, NUL, bytes, NUL) of every shard in sorted path order, as hex.
pub fn shard_digest(project_dir: &Utf8Path) -> Result<String> {
    let shards = project_dir.join("shards");
    let mut h = Sha256::new();
    for (p, _) in shard_files(project_dir)? {
        let rel = p.strip_prefix(&shards).unwrap_or(&p);
        h.update(rel.as_str().as_bytes());
        h.update([0u8]);
        h.update(std::fs::read(&p)?);
        h.update([0u8]);
    }
    let d = h.finalize();
    Ok(d.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
#[test]
fn shard_and_cache_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = Utf8Path::from_path(tmp.path()).unwrap();
    std::fs::create_dir_all(dir.join("shards/r/sub")).unwrap();
    std::fs::create_dir_all(dir.join(".cache")).unwrap();
    std::fs::write(dir.join("shards/r/a.rs.kshard.json"), vec![b'a'; 10]).unwrap();
    std::fs::write(dir.join("shards/r/sub/b.rs.kshard.json"), vec![b'b'; 20]).unwrap();
    std::fs::write(dir.join("shards/r/ignored.txt"), b"zzzzzzzz").unwrap();
    std::fs::write(dir.join(".cache/index.redb"), vec![0u8; 100]).unwrap();
    std::fs::write(dir.join(".cache/roots.yaml"), vec![0u8; 5]).unwrap();
    assert_eq!(
        shard_stats(dir).unwrap(),
        ShardStats {
            files: 2,
            bytes: 30
        }
    );
    assert_eq!(cache_bytes(dir).unwrap(), 105);
    let d1 = shard_digest(dir).unwrap();
    assert_eq!(d1.len(), 64);
    assert_eq!(d1, shard_digest(dir).unwrap());
    // mtime does not matter; one changed byte does.
    let f = std::fs::File::options()
        .write(true)
        .open(dir.join("shards/r/a.rs.kshard.json"))
        .unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
        .unwrap();
    drop(f);
    assert_eq!(d1, shard_digest(dir).unwrap());
    std::fs::write(dir.join("shards/r/a.rs.kshard.json"), b"abaaaaaaaa").unwrap();
    assert_ne!(d1, shard_digest(dir).unwrap());
    // No project: zeros.
    let none = dir.join("nothing");
    assert_eq!(
        shard_stats(&none).unwrap(),
        ShardStats { files: 0, bytes: 0 }
    );
    assert_eq!(cache_bytes(&none).unwrap(), 0);
}
