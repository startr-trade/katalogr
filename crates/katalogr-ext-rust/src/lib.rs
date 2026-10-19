//! Rust extension (FEAT-005, ADR-0015): syntax level plus module and import resolution, with
//! the workspace structure taken from the repository's `Cargo.toml` files.

pub mod bindings;
pub mod calls;
pub mod context;
pub mod extract;
pub mod impls;
pub mod items;
pub mod lines;
pub mod manifest;
pub mod ownership;
pub mod rationale;
pub mod roots;

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
        &[context::MANIFEST, context::LIB_FILE, context::MAIN_FILE]
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
            "annotations",
            "parse-error",
            "entry-root",
            "rationale-comment",
            "package-fact",
        ]
    }

    fn code_kinds(&self) -> &'static [&'static str] {
        roots::CODE_KINDS
    }

    /// Bumped when the insight facts (`root`, `package`, `rationale`) were added.
    fn revision_salt(&self) -> &'static str {
        "insights-1"
    }

    /// The [`WorkspaceContext`] as compact JSON with sorted keys and vectors.
    fn build_context(&self, files: &[(RelPath, Vec<u8>)]) -> Result<Vec<u8>, ExtractError> {
        Ok(context::build(files).to_bytes())
    }

    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        if input.path.as_str().rsplit('/').next() == Some(context::MANIFEST) {
            manifest::extract(input)
        } else {
            extract::extract(input)
        }
    }

    fn extract_with_errors(
        &self,
        input: &ExtractInput,
    ) -> Result<(Fragment, Vec<ExtractError>), ExtractError> {
        if input.path.as_str().rsplit('/').next() == Some(context::MANIFEST) {
            manifest::extract(input).map(|f| (f, Vec::new()))
        } else {
            extract::extract_with_errors(input)
        }
    }
}
