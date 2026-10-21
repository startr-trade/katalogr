//! Errors of the harness and their exit codes.

#[derive(Debug, thiserror::Error)]
pub enum BenchError {
    /// Exit 2.
    #[error("manifest: {0}")]
    Manifest(String),
    /// Exit 2 (inside a checkout, unreadable).
    #[error("workspace: {0}")]
    Workspace(String),
    /// Exit 3 (budget or free floor).
    #[error("disk: {0}")]
    Disk(String),
    /// Exit 3.
    #[error("machine not idle: load {load:.2} > {max:.2} after {waited_s} s")]
    NotIdle { load: f64, max: f64, waited_s: u64 },
    /// Exit 3: the workspace lock is held.
    #[error("{0}")]
    Busy(String),
    /// Exit 1.
    #[error("git: {0}")]
    Git(String),
    /// Exit 1 (index failed, bad JSON).
    #[error("katalogr: {0}")]
    Katalogr(String),
    /// Exit 1.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl BenchError {
    pub fn exit_code(&self) -> i32 {
        match self {
            BenchError::Manifest(_) | BenchError::Workspace(_) => 2,
            BenchError::Disk(_) | BenchError::NotIdle { .. } | BenchError::Busy(_) => 3,
            BenchError::Git(_) | BenchError::Katalogr(_) | BenchError::Io(_) => 1,
        }
    }
}

pub type Result<T> = std::result::Result<T, BenchError>;
