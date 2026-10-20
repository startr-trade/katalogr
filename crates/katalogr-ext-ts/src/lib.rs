//! TypeScript and JavaScript extension (FEAT-006, ADR-0015): syntax level plus module and
//! import resolution. Module specifiers become ordered candidate keys derived from the
//! project context (`tsconfig.json`, `jsconfig.json`, `package.json`); extraction never reads
//! the filesystem.

pub mod calls;
pub mod context;
pub mod extract;
pub mod items;
pub mod lines;
pub mod literals;
pub mod modules;
pub mod rationale;
pub mod roots;
pub mod strcalls;
pub(crate) mod tests;
pub(crate) mod wasm;

pub use context::{TsContext, TsProject};

use katalogr_model::{ExtractError, ExtractInput, Extraction, Extractor, Fragment, RelPath};

#[derive(Clone, Copy, Debug, Default)]
pub struct TsExtractor;

const SOURCE_EXTENSIONS: [&str; 8] = [".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"];

impl Extractor for TsExtractor {
    fn id(&self) -> &'static str {
        "ts"
    }

    fn handles(&self, p: &RelPath) -> bool {
        let s = p.as_str();
        let name = context::file_name(s);
        SOURCE_EXTENSIONS.iter().any(|e| s.ends_with(e))
            || name == context::PACKAGE_JSON
            || context::is_config_name(name)
    }

    fn language(&self, p: &RelPath) -> &'static str {
        let s = p.as_str();
        let name = context::file_name(s);
        if name == context::PACKAGE_JSON || context::is_config_name(name) {
            "json"
        } else if [".js", ".jsx", ".mjs", ".cjs"]
            .iter()
            .any(|e| s.ends_with(e))
        {
            "javascript"
        } else {
            "typescript"
        }
    }

    fn context_files(&self) -> &'static [&'static str] {
        &[
            context::TSCONFIG,
            context::JSCONFIG,
            context::PACKAGE_JSON,
            context::CARGO_TOML,
        ]
    }

    /// The exact names plus any `tsconfig*.json` / `jsconfig*.json`, so a config that another
    /// one `extends` is found whatever its name.
    fn is_context_file(&self, p: &RelPath) -> bool {
        let name = context::file_name(p.as_str());
        self.context_files().contains(&name) || context::is_config_name(name)
    }

    fn patterns(&self) -> &'static [&'static str] {
        &[
            "items",
            "relative-import",
            "path-alias",
            "esm-js-specifier",
            "commonjs-export",
            "extends-any-name",
            "constructor-call",
            "package-import",
            "call-binding",
            "call-member",
            "require",
            "annotations",
            "parse-error",
            "wasm-import",
            "wasm-glue",
            "embedded-literals",
            "entry-root",
            "rationale-comment",
            "package-fact",
            "test-marks",
            "string-calls",
        ]
    }

    fn code_kinds(&self) -> &'static [&'static str] {
        roots::CODE_KINDS
    }

    /// Bumped when extraction output changes for unchanged input: `1` adds the wasm package
    /// bindings (FEAT-016); `2` offers string literals to guest
    /// extensions (FEAT-017); `3` adds the `root`, `package` and `rationale`
    /// facts (FEAT-030); `;tests-1` adds the `test` fact, test symbols and the `test-name`
    /// exports (FEAT-063); `;rest-1` adds the `str_calls` fact (FEAT-014).
    fn revision_salt(&self) -> &'static str {
        "ts-extract-3;tests-1;rest-1"
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

    fn embeds(&self) -> bool {
        true
    }

    fn extract_full(&self, input: &ExtractInput) -> Result<Extraction, ExtractError> {
        extract::extract_full(input)
    }
}
