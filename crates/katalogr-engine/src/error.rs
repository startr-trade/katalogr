//! Engine errors.

use katalogr_model::Id;
use katalogr_store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Two different names hash to the same id; the update of that file is refused. The
    /// transaction may hold part of the update, so the caller must abandon it.
    #[error("id collision on {}: {existing} and {new}", .id.to_hex())]
    IdCollision {
        id: Id,
        existing: String,
        new: String,
    },
}
