//! Working-set inputs gathered by the composition root (FEAT-039).
//!
//! The edit log is the "recent" input: `<context>/.cache/recent-edits`, one repo-relative path
//! per line, newest last. It is a hint, not a record: two writers may race and the last rename
//! wins, which costs at most a boost.

use katalogr_model::RelPath;
use std::path::Path;

/// File name of the edit log inside the context's cache directory.
pub const EDIT_LOG: &str = "recent-edits";
/// Default for `working_set.recent_max` (provisional; FEAT-039 T7 sweeps it).
pub const DEFAULT_RECENT_MAX: usize = 32;

/// The paths in the edit log, oldest first. A missing or unreadable log is empty, and lines that
/// are not valid repo-relative paths are skipped.
pub fn read_edits(cache_dir: &Path) -> Vec<RelPath> {
    std::fs::read_to_string(cache_dir.join(EDIT_LOG))
        .map(|s| s.lines().filter_map(|l| RelPath::new(l).ok()).collect())
        .unwrap_or_default()
}

/// Append `path` as the newest edit: any earlier line for it is dropped, only the last `max`
/// paths are kept, and the file is written temp-then-rename.
pub fn append_edit(cache_dir: &Path, path: &RelPath, max: usize) -> std::io::Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    let mut edits = read_edits(cache_dir);
    edits.retain(|p| p != path);
    edits.push(path.clone());
    if edits.len() > max {
        edits.drain(..edits.len() - max);
    }
    let mut text = String::new();
    for p in &edits {
        text.push_str(p.as_str());
        text.push('\n');
    }
    let tmp = cache_dir.join(format!("{EDIT_LOG}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, cache_dir.join(EDIT_LOG)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}
