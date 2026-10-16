//! The extractor SPI.

use crate::fragment::Fragment;
use crate::graph::Span;
use crate::relpath::RelPath;
use serde::{Deserialize, Serialize};

pub trait Extractor: Send + Sync {
    /// Stable extension id.
    fn id(&self) -> &'static str;
    /// By file name / extension.
    fn handles(&self, path: &RelPath) -> bool;
    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError>;
    /// The link patterns this extension produces, for the fixture coverage check (FEAT-023).
    fn patterns(&self) -> &'static [&'static str] {
        &[]
    }
    /// File names whose content forms this extension's project context (e.g. manifests).
    /// A file matches when its last path component equals one of these names.
    fn context_files(&self) -> &'static [&'static str] {
        &[]
    }
    /// Build deterministic context bytes from those files (sorted by path). Default: empty.
    fn build_context(&self, files: &[(RelPath, Vec<u8>)]) -> Result<Vec<u8>, ExtractError> {
        let _ = files;
        Ok(Vec::new())
    }
}

/// Borrowed input; not serialisable.
#[derive(Copy, Clone, Debug)]
pub struct ExtractInput<'a> {
    pub repo: &'a str,
    pub path: &'a RelPath,
    pub revision: &'a str,
    pub content: &'a [u8],
    /// The extension's context for this repository (empty when it has none).
    pub context: &'a [u8],
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum ExtractError {
    #[error("parse error: {message}")]
    Parse { span: Option<Span>, message: String },
    #[error("unsupported: {0}")]
    Unsupported(String),
}
