//! Contexts: a base plus per-checkout or per-commit layers (FEAT-043).

use crate::error::StoreError;
use crate::kindex::{read_to_string, write_atomic};
use crate::shard::to_sorted_json;
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// `base`, `w-<16 hex>` or `c-<16 hex>`. Valid text is `[a-z0-9-]{1,40}`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContextId(String);

fn hex16(digest: &[u8]) -> String {
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

impl ContextId {
    pub const BASE: &'static str = "base";

    /// Fails with `StoreError::BadContext` on any other text.
    pub fn new(s: &str) -> Result<ContextId, StoreError> {
        let ok = !s.is_empty()
            && s.len() <= 40
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if ok {
            Ok(ContextId(s.to_string()))
        } else {
            Err(StoreError::BadContext(s.to_string()))
        }
    }

    /// `w-` plus 16 hex of the SHA-256 of the canonical absolute root.
    pub fn for_checkout(root: &Utf8Path) -> ContextId {
        let d = Sha256::digest(root.as_str().as_bytes());
        ContextId(format!("w-{}", hex16(&d)))
    }

    /// `c-` plus 16 hex of the SHA-256 of `repo`, a NUL and `commit`.
    pub fn for_commit(repo: &str, commit: &str) -> ContextId {
        let mut h = Sha256::new();
        h.update(repo.as_bytes());
        h.update([0u8]);
        h.update(commit.as_bytes());
        ContextId(format!("c-{}", hex16(&h.finalize())))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ContextId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    Checkout,
    Commit,
}

/// `<layer>/context.json`. Machine-local, so an absolute root is allowed here and only here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextManifest {
    pub format: u32,
    pub id: ContextId,
    pub kind: ContextKind,
    pub repo: String,
    /// `None` for a commit context.
    pub root: Option<Utf8PathBuf>,
    /// The base commit this was copied from.
    pub seed_commit: Option<String>,
}

impl ContextManifest {
    pub const FILE: &'static str = "context.json";
    pub const FORMAT: u32 = 1;

    /// Sorted-key JSON, as written to `context.json`.
    pub fn to_json(&self) -> Result<String, StoreError> {
        let v = serde_json::to_value(self).map_err(|e| StoreError::BadShard(e.to_string()))?;
        Ok(to_sorted_json(&v))
    }

    /// Write `<layer_root>/context.json` temp-then-rename.
    pub fn write(&self, layer_root: &Utf8Path) -> Result<(), StoreError> {
        write_atomic(&layer_root.join(Self::FILE), self.to_json()?.as_bytes())
    }

    pub fn read(layer_root: &Utf8Path) -> Result<ContextManifest, StoreError> {
        let p = layer_root.join(Self::FILE);
        serde_json::from_str(&read_to_string(&p)?).map_err(|e| StoreError::Parse {
            path: p.to_string(),
            message: e.to_string(),
        })
    }
}
