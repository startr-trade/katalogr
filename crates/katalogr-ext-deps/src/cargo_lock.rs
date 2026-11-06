//! Lockfile parser (stub until its task lands).

use crate::lock_common::ParsedLock;
use katalogr_model::ExtractError;

/// The lockfile's entries in file order.
pub fn parse(text: &str) -> Result<ParsedLock, ExtractError> {
    let _ = text;
    Err(ExtractError::Unsupported("not implemented".to_string()))
}
