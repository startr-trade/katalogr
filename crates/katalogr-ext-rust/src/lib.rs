//! Rust extension (FEAT-005, ADR-0015): syntax level plus module and import resolution, with
//! the workspace structure taken from the repository's `Cargo.toml` files.

pub mod bindings;
pub mod calls;
pub mod context;
pub mod extract;
pub mod impls;
pub mod items;
pub mod lines;
pub mod literals;
pub mod manifest;
pub mod ownership;

pub use context::{CrateInfo, Dep, Target, TargetKind, WorkspaceContext};

use katalogr_model::{ExtractError, ExtractInput, Extraction, Extractor, Fragment, RelPath};

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
            "embedded-literals",
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
            extract::extract(input)
        }
    }

    fn extract_with_errors(
        &self,
        input: &ExtractInput,
    ) -> Result<(Fragment, Vec<ExtractError>), ExtractError> {
        self.extract_full(input).map(|x| (x.fragment, x.errors))
    }

    fn embeds(&self) -> bool {
        true
    }

    fn extract_full(&self, input: &ExtractInput) -> Result<Extraction, ExtractError> {
        if input.path.as_str().rsplit('/').next() == Some(context::MANIFEST) {
            manifest::extract(input).map(|fragment| Extraction {
                fragment,
                errors: Vec::new(),
                embedded: Vec::new(),
            })
        } else {
            extract::extract_full(input)
        }
    }
}
