//! Facade errors, classified so a wrapper can pick an exit status.

use katalogr_engine::EngineError;
use katalogr_engine::scan::ScanError;
use katalogr_store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum DistError {
    /// The caller asked for something that cannot be done as stated (bad flag value, missing
    /// configuration, unknown repo, path outside the repo).
    #[error("{0}")]
    Usage(String),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Scan(#[from] ScanError),
    #[error("{path}: {message}")]
    Io { path: String, message: String },
}

impl DistError {
    /// True for errors the user fixes by changing the command line or configuration.
    pub fn is_usage(&self) -> bool {
        match self {
            DistError::Usage(_) => true,
            DistError::Store(e) => matches!(
                e,
                StoreError::NotConfigured
                    | StoreError::BadProjectName(_)
                    | StoreError::BadRepoName(_)
                    | StoreError::ProjectExists(_)
                    | StoreError::ProjectNotFound(_)
                    | StoreError::NotAKindex(_)
            ),
            _ => false,
        }
    }

    pub(crate) fn usage(msg: impl Into<String>) -> DistError {
        DistError::Usage(msg.into())
    }

    pub(crate) fn io(path: impl std::fmt::Display, e: impl std::fmt::Display) -> DistError {
        DistError::Io {
            path: path.to_string(),
            message: e.to_string(),
        }
    }
}
