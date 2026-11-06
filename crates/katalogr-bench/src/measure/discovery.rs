//! The baseline for languages with no extractor: files seen, indexed and unclaimed, unclaimed
//! suffixes, and what the text fallback indexed.

use camino::Utf8Path;
use serde_json::Value;
use std::collections::BTreeMap;

use super::{Ctx, DiscoveryResult};
use crate::error::Result;

/// Read the `coverage --suffixes N` JSON envelope: `totals`, the `by_extension` row for `text`,
/// and the unclaimed suffixes summed over repositories (by count, then suffix).
/// `unclaimed_per_s = files_seen / wall_s` applies only when every claimed file is `text` or
/// `md`. `text_chunks` is filled in by the caller from the graph database.
pub fn from_coverage(cov: &Value, wall_s: Option<f64>) -> DiscoveryResult {
    let u = |p: &str| cov.pointer(p).and_then(Value::as_u64).unwrap_or(0);
    let files_indexed = u("/result/totals/files_indexed");
    let files_unclaimed = u("/result/totals/files_unclaimed");
    let files_seen = files_indexed + files_unclaimed;
    let mut text_files = 0;
    let mut only_text_or_md = true;
    if let Some(rows) = cov
        .pointer("/result/by_extension")
        .and_then(Value::as_array)
    {
        for r in rows {
            let ext = r.get("extension").and_then(Value::as_str).unwrap_or("");
            let n = r
                .pointer("/counts/files_indexed")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if ext == "text" {
                text_files += n;
            } else if ext != "md" && n > 0 {
                only_text_or_md = false;
            }
        }
    }
    let mut by: BTreeMap<String, u64> = BTreeMap::new();
    if let Some(rows) = cov.pointer("/result/unclaimed").and_then(Value::as_array) {
        for r in rows {
            for s in r
                .get("suffixes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let suffix = s
                    .get("suffix")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                *by.entry(suffix).or_default() +=
                    s.get("files").and_then(Value::as_u64).unwrap_or(0);
            }
        }
    }
    let mut by_suffix: Vec<(String, u64)> = by.into_iter().collect();
    by_suffix.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let unclaimed_per_s = match wall_s {
        Some(w) if w > 0.0 && only_text_or_md => Some(files_seen as f64 / w),
        _ => None,
    };
    DiscoveryResult {
        files_seen,
        files_indexed,
        files_unclaimed,
        unclaimed_per_s,
        text_files,
        text_chunks: None,
        by_suffix,
    }
}

/// Number of `chunk` symbols from artifacts of the `text` extension, read from the graph
/// database (FEAT-094: there are no shard files to count).
pub fn text_chunks(project_dir: &Utf8Path) -> u64 {
    // `<kindex>/projects/<project>`.
    let (Some(projects), Some(project)) = (project_dir.parent(), project_dir.file_name()) else {
        return 0;
    };
    let Some(kindex) = projects.parent() else {
        return 0;
    };
    katalogr_dist::symbol_count(kindex, project, "text", "chunk").unwrap_or(0)
}

/// Discovery after the index phase: the coverage report plus the text-fallback chunk count.
pub fn run(cx: &mut Ctx, wall_s: Option<f64>) -> Result<DiscoveryResult> {
    let cov = cx.run_json(&["coverage", "--suffixes", "50"])?;
    let mut d = from_coverage(&cov, wall_s);
    if d.text_files > 0 {
        d.text_chunks = Some(text_chunks(&cx.project_dir()));
    }
    Ok(d)
}
