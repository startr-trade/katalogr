//! Dependency inventory (FEAT-007): lockfile entries become `package@version` nodes, linked to
//! the code that uses them through the package names they export.

pub mod cargo_lock;
pub mod lock_common;
pub mod npm_lock;
pub mod pnpm_lock;

use katalogr_model::{ExtractError, ExtractInput, Extractor, Fragment, RelPath};

/// The lockfile names this extension reads.
pub const CARGO_LOCK: &str = "Cargo.lock";
pub const NPM_LOCK: &str = "package-lock.json";
pub const PNPM_LOCK: &str = "pnpm-lock.yaml";

fn file_name(path: &RelPath) -> &str {
    path.as_str().rsplit('/').next().unwrap_or("")
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DepsExtractor;

impl Extractor for DepsExtractor {
    fn id(&self) -> &'static str {
        "deps"
    }

    /// `1` is the first output.
    fn revision_salt(&self) -> &'static str {
        "1"
    }

    fn handles(&self, path: &RelPath) -> bool {
        matches!(file_name(path), CARGO_LOCK | NPM_LOCK | PNPM_LOCK)
    }

    fn patterns(&self) -> &'static [&'static str] {
        &["packages", "versions", "members", "dependencies"]
    }

    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        self.extract_with_errors(input).map(|(f, _)| f)
    }

    fn extract_with_errors(
        &self,
        input: &ExtractInput,
    ) -> Result<(Fragment, Vec<ExtractError>), ExtractError> {
        let text = std::str::from_utf8(input.content).map_err(|e| ExtractError::Parse {
            span: None,
            message: format!("not utf-8: {e}"),
        })?;
        let lock = match file_name(input.path) {
            CARGO_LOCK => cargo_lock::parse(text)?,
            NPM_LOCK => npm_lock::parse(text)?,
            PNPM_LOCK => pnpm_lock::parse(text)?,
            other => {
                return Err(ExtractError::Unsupported(format!(
                    "no lockfile format for {other}"
                )));
            }
        };
        Ok(lock_common::entries_to_fragment(input, lock))
    }
}
