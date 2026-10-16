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
}

/// Borrowed input; not serialisable.
#[derive(Copy, Clone, Debug)]
pub struct ExtractInput<'a> {
    pub repo: &'a str,
    pub path: &'a RelPath,
    pub revision: &'a str,
    pub content: &'a [u8],
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum ExtractError {
    #[error("parse error: {message}")]
    Parse { span: Option<Span>, message: String },
    #[error("unsupported: {0}")]
    Unsupported(String),
}
