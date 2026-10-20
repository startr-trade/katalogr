//! Stamps the build with a commit short sha and a build date (FEAT-022 R-4).
//!
//! Never fails: a missing piece becomes `unknown` (sha) or is replaced by
//! today's UTC date (date).

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn git_sha() -> Option<String> {
    if let Ok(v) = std::env::var("KATALOGR_GIT_SHA") {
        // CI passes the full 40-character sha; the version string always carries 12 (FEAT-022 §3).
        let v: String = v.trim().chars().take(12).collect();
        if !v.is_empty() {
            return Some(v);
        }
    }
    let out = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into()))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

/// Resolves each name with `git rev-parse --git-path`, which follows a worktree's `.git` file
/// to its own HEAD and the shared refs.
fn git_paths(names: &[&str]) -> Vec<std::path::PathBuf> {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let mut args = vec!["rev-parse", "--path-format=absolute"];
    for n in names {
        args.extend(["--git-path", n]);
    }
    let Ok(out) = Command::new("git").args(&args).current_dir(dir).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| std::path::PathBuf::from(l.trim()))
        .collect()
}

/// Seconds since the Unix epoch, from `SOURCE_DATE_EPOCH` or the clock.
fn epoch_seconds() -> Option<i64> {
    if let Ok(v) = std::env::var("SOURCE_DATE_EPOCH")
        && let Ok(secs) = v.trim().parse::<i64>()
    {
        return Some(secs);
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(now.as_secs()).ok()
}

/// Converts days since 1970-01-01 to a proleptic Gregorian `(year, month, day)`.
/// Howard Hinnant's `civil_from_days` algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn build_date() -> String {
    match epoch_seconds() {
        Some(secs) => {
            let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
            format!("{y:04}-{m:02}-{d:02}")
        }
        None => "unknown".to_string(),
    }
}

fn main() {
    // Re-run when HEAD moves, so a new commit never keeps a stale sha. Paths come from git so a
    // worktree (where `.git` is a file) is covered too. Missing paths are fine.
    for p in git_paths(&["HEAD", "refs/heads", "packed-refs"]) {
        if p.exists() {
            println!("cargo:rerun-if-changed={}", p.display());
        }
    }
    println!("cargo:rerun-if-env-changed=KATALOGR_GIT_SHA");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    let sha = git_sha().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=KATALOGR_BUILD_SHA={sha}");
    println!("cargo:rustc-env=KATALOGR_BUILD_DATE={}", build_date());
}
