//! The project's processor lock: `.cache/processor.lock`, held exclusively by one process.

use crate::error::EngineError;
use camino::Utf8Path;
use fs4::FileExt;
use std::fs::File;

/// Non-blocking exclusive lock on `<cache_dir>/processor.lock` (the file and directory are
/// created when missing). `None` when another holder has it. The lock is released when the
/// returned file is dropped.
pub(crate) fn try_lock(cache_dir: &Utf8Path) -> Result<Option<File>, EngineError> {
    std::fs::create_dir_all(cache_dir).map_err(|e| EngineError::io(cache_dir, e))?;
    let path = cache_dir.join("processor.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| EngineError::io(&path, e))?;
    match FileExt::try_lock(&file) {
        Ok(()) => Ok(Some(file)),
        Err(fs4::TryLockError::WouldBlock) => Ok(None),
        Err(fs4::TryLockError::Error(e)) => Err(EngineError::io(&path, e)),
    }
}
