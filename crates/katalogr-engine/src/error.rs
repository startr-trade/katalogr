//! Engine errors.

use katalogr_model::Id;
use katalogr_store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Two different names hash to the same id; the update of that file is refused. The
    /// transaction may hold part of the update, so the caller must abandon it.
    #[error("id collision on {}: {existing} and {new}", .id.to_hex())]
    IdCollision {
        id: Id,
        existing: String,
        new: String,
    },
    #[error("{path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error(transparent)]
    Scan(#[from] crate::scan::ScanError),
    #[error("queue: {0}")]
    Queue(String),
    #[error("history: {0}")]
    History(String),
}

impl EngineError {
    pub(crate) fn io(path: impl std::fmt::Display, source: std::io::Error) -> EngineError {
        EngineError::Io {
            path: path.to_string(),
            source,
        }
    }
}
