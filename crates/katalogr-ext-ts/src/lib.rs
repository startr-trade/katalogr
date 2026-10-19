//! TypeScript and JavaScript extension (FEAT-006, ADR-0015): syntax level plus module and
//! import resolution. Module specifiers become ordered candidate keys derived from the
//! project context (`tsconfig.json`, `jsconfig.json`, `package.json`); extraction never reads
//! the filesystem.

pub mod calls;
pub mod context;
pub mod extract;
pub mod items;
pub mod lines;
pub mod modules;

pub use context::{TsContext, TsProject};

use katalogr_model::{ExtractError, ExtractInput, Extractor, Fragment, RelPath};

#[derive(Clone, Copy, Debug, Default)]
pub struct TsExtractor;

const SOURCE_EXTENSIONS: [&str; 8] = [".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"];

impl Extractor for TsExtractor {
    fn id(&self) -> &'static str {
        "ts"
    }

    fn handles(&self, p: &RelPath) -> bool {
        let s = p.as_str();
        SOURCE_EXTENSIONS.iter().any(|e| s.ends_with(e))
            || self.context_files().contains(&context::file_name(s))
    }

    fn context_files(&self) -> &'static [&'static str] {
        &[context::TSCONFIG, context::JSCONFIG, context::PACKAGE_JSON]
    }

    fn patterns(&self) -> &'static [&'static str] {
        &[
            "items",
            "relative-import",
            "path-alias",
            "package-import",
            "call-binding",
            "call-member",
            "require",
            "parse-error",
        ]
    }

    /// The [`TsContext`] as compact JSON with sorted keys and vectors.
    fn build_context(&self, files: &[(RelPath, Vec<u8>)]) -> Result<Vec<u8>, ExtractError> {
        Ok(context::build(files).to_bytes())
    }

    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        extract::extract(input)
    }

    fn extract_with_errors(
        &self,
        input: &ExtractInput,
    ) -> Result<(Fragment, Vec<ExtractError>), ExtractError> {
        extract::extract_with_errors(input)
    }
}
