//! The extractor SPI.

use crate::embed::{EmbeddedInput, EmbeddedText};
use crate::fragment::{Binding, Fragment};
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

    /// The whole extraction, including literals offered to guests. The default wraps
    /// `extract_with_errors` with no literals. The engine calls this method.
    fn extract_full(&self, input: &ExtractInput) -> Result<Extraction, ExtractError> {
        self.extract_with_errors(input)
            .map(|(fragment, errors)| Extraction {
                fragment,
                errors,
                embedded: Vec::new(),
            })
    }
    /// Host: true when `extract_full` may return embedded texts. Default false.
    fn embeds(&self) -> bool {
        false
    }
    /// Guest: `Some(version)` when this extension binds embedded text. The version string changes
    /// whenever its embedded output can change. Default None.
    fn embedded_version(&self) -> Option<&'static str> {
        None
    }
    /// Guest: a cheap test of whether `text` is worth `extract_embedded`. Default false.
    fn accepts_embedded(&self, text: &str) -> bool {
        let _ = text;
        false
    }
    /// Guest: bindings for one embedded text. Each binding's `symbol` must be `input.owner`.
    /// It must be pure and deterministic over `input` (including `input.context` and `input.call`),
    /// and must not panic on any input. Default empty.
    fn extract_embedded(&self, input: &EmbeddedInput) -> Vec<Binding> {
        let _ = input;
        Vec::new()
    }
    /// Guest: the part of this extension's repository context (its `build_context` bytes, or empty)
    /// that `extract_embedded` depends on. Must be deterministic and as small as possible, because
    /// every host file of the repository is re-extracted when it changes. Default: empty (the guest
    /// uses no repository context).
    fn embedded_context(&self, context: &[u8]) -> Vec<u8> {
        let _ = context;
        Vec::new()
    }
}

/// The full result of extracting one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extraction {
    pub fragment: Fragment,
    pub errors: Vec<ExtractError>,
    pub embedded: Vec<EmbeddedText>,
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
