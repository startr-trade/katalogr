//! Store errors.

use crate::guard::GuardError;
use katalogr_model::NameError;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error(transparent)]
    Guard(#[from] GuardError),
    #[error(
        "project format {found} is newer than the supported format {supported}; upgrade katalogr to at least {min_katalogr}"
    )]
    FormatTooNew {
        found: u32,
        supported: u32,
        min_katalogr: String,
    },
    #[error("{0}: not a kindex project (format.json is missing)")]
    NotAProject(String),
    #[error("{0}: not a kindex (no projects directory)")]
    NotAKindex(String),
    #[error("project already exists: {0}")]
    ProjectExists(String),
    #[error("project not found: {0}")]
    ProjectNotFound(String),
    #[error("invalid project name: {0:?}")]
    BadProjectName(String),
    #[error("invalid repo name: {0:?}")]
    BadRepoName(String),
    #[error("invalid shard: {0}")]
    BadShard(String),
    #[error("unsupported shard format: {0}")]
    ShardFormat(String),
    #[error("{path}: {message}")]
    Parse { path: String, message: String },
    #[error("no kindex configured: no marker found and no override given")]
    NotConfigured,
    #[error("git: {0}")]
    Git(String),
    #[error(transparent)]
    Name(#[from] NameError),
    #[error("cache is busy: another process holds the database")]
    Busy,
    #[error("cache database: {0}")]
    Db(String),
    #[error("cache row: {0}")]
    Row(String),
    #[error("stitch: {0}")]
    Stitch(String),
}

impl StoreError {
    pub(crate) fn io(path: impl std::fmt::Display, source: std::io::Error) -> StoreError {
        StoreError::Io {
            path: path.to_string(),
            source,
        }
    }
}
