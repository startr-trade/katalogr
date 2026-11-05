//! Contexts: a base plus per-checkout or per-commit layers (FEAT-043).

use crate::cache::{Cache, Reader, TableId};
use crate::error::StoreError;
use crate::kindex::ProjectDir;
use crate::kindex::{read_to_string, write_atomic};
use crate::shard::to_sorted_json;
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Instant;

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
    /// The commit a `Commit` context reads, as full hex; absent for a checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
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

/// `meta` key: `[[repo, path], …]` the base had index errors for when the layer was seeded.
pub const META_BASE_ERRORS: &str = "layer_base_errors";

/// Copy the base cache into `layer`, clear what belongs to the base, then rename into place.
/// The caller holds the base's processor lock. An interrupted seed leaves only a temp file, and
/// a layer directory without `context.json` is removed on sight. A layer that already has its
/// manifest is left as it is. The written manifest has `seed_commit` filled from the base's
/// `base_commit.<repo>` when the caller left it empty.
pub fn seed_layer(
    base: &ProjectDir,
    layer: &ProjectDir,
    m: &ContextManifest,
    deadline: Instant,
) -> Result<(), StoreError> {
    let mut m = m.clone();
    let root = layer.root();
    if root.join(ContextManifest::FILE).is_file() {
        return Ok(());
    }
    if root.exists() {
        std::fs::remove_dir_all(root).map_err(|e| StoreError::io(root, e))?;
    }
    let cache_dir = layer.cache_dir();
    std::fs::create_dir_all(&cache_dir).map_err(|e| StoreError::io(&cache_dir, e))?;
    let tmp = cache_dir.join("index.redb.tmp");
    let fin = cache_dir.join("index.redb");
    let from = base.cache_dir().join("index.redb");
    if from.is_file() {
        std::fs::copy(&from, &tmp).map_err(|e| StoreError::io(&from, e))?;
    }
    #[cfg(feature = "failpoints")]
    crate::failpoint::hit("seed_after_copy");
    if tmp.is_file() {
        let cache = Cache::open_file(&tmp, deadline)?;
        // The paths the base holds index errors for: shards carry no error rows, so a layer
        // must never reuse the base's shard for these (FEAT-043 R-20).
        let errored: Vec<(String, String)> = {
            cache
                .read()?
                .index_error_files()?
                .into_iter()
                .map(|((repo, path), _)| (repo, path.as_str().to_string()))
                .collect()
        };
        let mut tx = cache.write()?;
        tx.set_meta(
            META_BASE_ERRORS,
            &serde_json::to_vec(&errored).map_err(|e| StoreError::Row(e.to_string()))?,
        )?;
        for t in [
            TableId::FileRevisions,
            TableId::Pending,
            TableId::Events,
            TableId::Journal,
        ] {
            tx.clear_table(t)?;
        }
        // The commit the base reflected, when the caller did not name one.
        if m.seed_commit.is_none() {
            m.seed_commit = tx
                .meta(&format!("base_commit.{}", m.repo))?
                .and_then(|b| String::from_utf8(b).ok());
        }
        tx.set_meta(
            "layer_seed_commit",
            m.seed_commit.as_deref().unwrap_or("").as_bytes(),
        )?;
        // FEAT-084. A checkout layer reflects the seed commit exactly, with no uncommitted part;
        // the base's own record of its position does not carry over. With no seed commit the
        // layer has no record, and the first read rescans.
        let (pos_key, changed_key) = (
            format!("position.{}", m.repo),
            format!("position_changed.{}", m.repo),
        );
        match (&m.kind, m.seed_commit.as_deref()) {
            (ContextKind::Checkout, Some(c)) if !c.is_empty() => {
                let pos = serde_json::json!({"repo": m.repo, "commit": c, "dirty": null});
                tx.set_meta(&pos_key, pos.to_string().as_bytes())?;
                tx.set_meta(&changed_key, br#"{"over":false,"paths":[]}"#)?;
            }
            _ => {
                tx.set_meta(&pos_key, b"")?;
                tx.set_meta(&changed_key, b"")?;
            }
        }
        tx.commit()?;
        drop(cache);
        std::fs::File::open(&tmp)
            .and_then(|f| f.sync_all())
            .map_err(|e| StoreError::io(&tmp, e))?;
        #[cfg(feature = "failpoints")]
        crate::failpoint::hit("seed_before_rename");
        std::fs::rename(&tmp, &fin).map_err(|e| StoreError::io(&fin, e))?;
    }
    m.write(root)
}
