//! Rust extension (FEAT-005, ADR-0015): syntax level plus module and import resolution, with
//! the workspace structure taken from the repository's `Cargo.toml` files.

pub mod context;
pub mod manifest;
pub mod ownership;

pub use context::{CrateInfo, Dep, Target, TargetKind, WorkspaceContext};

use katalogr_model::{ExtractError, ExtractInput, Extractor, Fragment, RelPath};

#[derive(Clone, Copy, Debug, Default)]
pub struct RustExtractor;

impl Extractor for RustExtractor {
    fn id(&self) -> &'static str {
        "rust"
    }

    fn handles(&self, p: &RelPath) -> bool {
        p.as_str().ends_with(".rs") || p.as_str().rsplit('/').next() == Some(context::MANIFEST)
    }

    fn context_files(&self) -> &'static [&'static str] {
        &[context::MANIFEST]
    }

    fn patterns(&self) -> &'static [&'static str] {
        &[
            "crate-deps",
            "items",
            "impl-links",
            "use-resolution",
            "re-export",
            "call-path",
            "call-method",
            "attributes",
            "parse-error",
        ]
    }

    /// The [`WorkspaceContext`] as compact JSON with sorted keys and vectors.
    fn build_context(&self, files: &[(RelPath, Vec<u8>)]) -> Result<Vec<u8>, ExtractError> {
        Ok(context::build(files).to_bytes())
    }

    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        if input.path.as_str().rsplit('/').next() == Some(context::MANIFEST) {
            manifest::extract(input)
        } else {
            Err(ExtractError::Unsupported(
                "Rust source extraction is not implemented yet".to_string(),
            ))
        }
    }
}
