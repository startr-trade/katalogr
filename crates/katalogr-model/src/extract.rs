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
    /// Like [`Extractor::extract`], also returning recoverable errors (such as syntax errors)
    /// found in a file whose fragment is still published. Default: no errors.
    fn extract_with_errors(
        &self,
        input: &ExtractInput,
    ) -> Result<(Fragment, Vec<ExtractError>), ExtractError> {
        self.extract(input).map(|f| (f, Vec::new()))
    }
    /// The link patterns this extension produces, for the fixture coverage check (FEAT-023).
    fn patterns(&self) -> &'static [&'static str] {
        &[]
    }
    /// File names whose content forms this extension's project context (e.g. manifests).
    /// A file matches when its last path component equals one of these names.
    fn context_files(&self) -> &'static [&'static str] {
        &[]
    }
    /// True when `path` is one of this extension's context files. Default: the last path
    /// component equals one of [`Extractor::context_files`]. An extension overrides it to match
    /// names that an exact list cannot express (for example a prefix rule).
    fn is_context_file(&self, path: &RelPath) -> bool {
        let name = path.as_str().rsplit('/').next().unwrap_or("");
        self.context_files().contains(&name)
    }
    /// Build deterministic context bytes from those files (sorted by path). Default: empty.
    fn build_context(&self, files: &[(RelPath, Vec<u8>)]) -> Result<Vec<u8>, ExtractError> {
        let _ = files;
        Ok(Vec::new())
    }
    /// Bumped by an extension whose output changes for unchanged input (new facts, new symbol
    /// kinds). It is mixed into the shard revision, so `index --full` re-extracts that
    /// extension's files once. Default: empty, which leaves every revision unchanged
    /// (ADR-0018; shared by FEAT-030 and FEAT-016).
    fn revision_salt(&self) -> &'static str {
        ""
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
