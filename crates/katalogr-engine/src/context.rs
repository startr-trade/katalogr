//! Extension context plumbing (FEAT-005 section 3, task T1; ADR-0015 amendment).
//!
//! An extension may name context files (`Extractor::context_files`), for example manifests.
//! The engine builds the extension's context for each repository from every such file in the
//! repository and passes it to `extract`. When the context's hash changes, every file of the
//! repository is queued for re-extraction.

use crate::consistency::Engine;
use crate::error::EngineError;
use crate::scan;
use katalogr_model::{Extractor, RelPath};
use sha2::{Digest, Sha256};

/// The last path component.
pub(crate) fn file_name(path: &RelPath) -> &str {
    path.as_str().rsplit('/').next().unwrap_or("")
}

/// True when `path` is one of the extension's context files.
pub(crate) fn is_context_file(ext: &dyn Extractor, path: &RelPath) -> bool {
    ext.context_files().contains(&file_name(path))
}

pub(crate) fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// The revision an extractor is given: the blob id, followed by `+<16 hex digits of the
/// context hash>` when the context is not empty. A changed context therefore changes the
/// revision, so the unchanged file is extracted again.
pub(crate) fn revision(blob: &str, context: &[u8]) -> String {
    if context.is_empty() {
        return blob.to_string();
    }
    let h = hash(context);
    let hex: String = h[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("{blob}+{hex}")
}

/// Build the context of `ext` for `repo` from the files currently on disk. `None` when the
/// repo has no root here or the extension refuses its files (the previous context is kept).
pub(crate) fn build(
    engine: &Engine,
    repo: &str,
    ext: &dyn Extractor,
) -> Result<Option<Vec<u8>>, EngineError> {
    let Some(root) = engine.roots().get(repo) else {
        return Ok(None);
    };
    let (include, exclude) = engine.repo_filters(repo);
    let mut files = Vec::new();
    for p in scan::list_files(root.as_std_path(), &include, &exclude)? {
        if !is_context_file(ext, &p) {
            continue;
        }
        let file = root.join(p.as_str());
        match std::fs::read(&file) {
            Ok(bytes) => files.push((p, bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(EngineError::io(&file, e)),
        }
    }
    Ok(ext.build_context(&files).ok())
}
