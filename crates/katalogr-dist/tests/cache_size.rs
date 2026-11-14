//! FEAT-123: a full index leaves the cache compacted, base advance included. The base advance
//! (FEAT-043) writes the default branch's tip into the same cache, so a compaction that ran
//! before it left the file several times the size of its rows.

#[path = "support/synth.rs"]
mod synth;

use katalogr_dist::{Katalogr, Options, Overrides, kindex_init, project_init};
use katalogr_store::{Cache, CompactMode, Kindex, ProjectName};
use std::path::Path;
use std::time::{Duration, Instant};

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// R-1 (FEAT-123): after `index --full` on a repository whose working tree differs from its
/// default branch, so the base advance rewrites every file, the cache file is within 25% of what
/// a rewrite of the same rows produces.
#[test]
fn full_index_leaves_cache_compacted_after_base_advance() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let repo = root.join("app");
    synth::rust_corpus(&repo, 6, 20, 8);
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), ".katalogr\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    // The working tree now differs from `main` in every module.
    synth::rust_corpus(&repo, 6, 20, 14);
    let kx = root.join("kx");
    kindex_init(&kx).unwrap();
    project_init(&kx, "demo", vec![("app".into(), repo.clone())], false).unwrap();
    let k = Katalogr::resolve(
        &repo,
        &Overrides {
            kindex: Some(kx.display().to_string()),
            project: Some("demo".into()),
            context: None,
            snapshot_dirs: Vec::new(),
        },
    )
    .unwrap();
    let opts = Options {
        timeout: Some(600.0),
        ..Options::default()
    };
    let report = k.index(None, true, None, &opts).unwrap();
    assert_eq!(report.pending, 0, "{:?}", report.warnings);
    let dir = Kindex::open(camino::Utf8Path::from_path(&kx).unwrap())
        .unwrap()
        .project(&ProjectName::new("demo").unwrap())
        .unwrap();
    let cache = Cache::open(&dir, Instant::now() + Duration::from_secs(60)).unwrap();
    let before = cache.file_bytes().unwrap();
    let (_, rep) = cache
        .compact(
            &dir,
            CompactMode::Rewrite,
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
    assert!(
        before as f64 <= 1.25 * rep.bytes_after as f64,
        "cache {before} bytes; a rewrite of the same rows is {}",
        rep.bytes_after
    );
}
