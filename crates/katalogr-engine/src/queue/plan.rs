//! Planning one queued path: read the file, extract, profile and embed (FEAT-047, FEAT-072).
//!
//! Planning is a pure function of the file content, the [`PlanEnv`] (loaded once per round from
//! the round's snapshot), the profiles and the registry, so any number of paths can be planned
//! at once without touching the cache.

use crate::consistency::Engine;
use crate::context;
use crate::embed::{self, GuestContexts};
use crate::error::EngineError;
use crate::profiles::ProfileSet;
use crate::registry::ExtensionRegistry;
use camino::Utf8Path;
use katalogr_model::{ExtractError, ExtractInput, Extractor, Fragment, RelPath};
use katalogr_store::blob::git_blob_id;
use katalogr_store::{ErrorRow, PendingKey, ReadTx, Reader};
use std::collections::BTreeMap;
use std::fs::File;

/// The extractor for `path`: the profile route when it names a registered extension, else the
/// first registered extractor that handles the path.
pub(super) fn extractor_for<'r>(
    registry: &'r ExtensionRegistry,
    profiles: &ProfileSet,
    path: &RelPath,
) -> Option<&'r dyn Extractor> {
    profiles
        .route(path)
        .and_then(|id| registry.by_id(id))
        .or_else(|| registry.for_path(path))
}

/// What one pending path needs.
pub(super) enum Action {
    /// Nothing to publish; the entry is dropped.
    Skip,
    Replace {
        fragment: Box<Fragment>,
        errors: Vec<ErrorRow>,
        tmp: String,
        fin: String,
    },
    Delete {
        fin: Option<String>,
    },
    Failed(String),
}

pub(super) struct Planned {
    pub(super) key: PendingKey,
    pub(super) seq: u64,
    pub(super) action: Action,
}

/// The read-only inputs of planning a round, loaded once on the calling thread from the round's
/// snapshot, so planning never touches the database.
pub(super) struct PlanEnv<'e> {
    engine: &'e Engine,
    profiles: &'e ProfileSet,
    /// Stored `ext_context` bytes of each `(repo, routed extractor id)` the round touches.
    contexts: BTreeMap<(String, &'static str), Vec<u8>>,
    /// For each `(repo, host id)` whose routed extractor `embeds()`: its guests' contexts.
    guests: BTreeMap<(String, &'static str), GuestContexts>,
    embed_sig: Option<[u8; 32]>,
}

impl<'e> PlanEnv<'e> {
    pub(super) fn load(
        engine: &'e Engine,
        snap: &ReadTx,
        profiles: &'e ProfileSet,
        items: &[(PendingKey, u64)],
    ) -> Result<PlanEnv<'e>, EngineError> {
        let mut contexts = BTreeMap::new();
        let mut guests = BTreeMap::new();
        for (key, _) in items {
            if !engine.roots().contains_key(&key.repo) {
                continue;
            }
            let Some(ext) = extractor_for(engine.registry(), profiles, &key.path) else {
                continue;
            };
            let pair = (key.repo.clone(), ext.id());
            if !contexts.contains_key(&pair) {
                let bytes = snap
                    .ext_context(&key.repo, ext.id())?
                    .map(|(_, bytes)| bytes)
                    .unwrap_or_default();
                contexts.insert(pair.clone(), bytes);
            }
            if ext.embeds() && !guests.contains_key(&pair) {
                let g = GuestContexts::load(engine.registry(), snap, &key.repo, ext.id())?;
                guests.insert(pair, g);
            }
        }
        Ok(PlanEnv {
            engine,
            profiles,
            contexts,
            guests,
            embed_sig: embed::signature(engine.registry()),
        })
    }
}

/// Decide what one queued path needs, from the file's current content.
pub(super) fn plan_one(env: &PlanEnv, key: &PendingKey, seq: u64) -> Result<Action, EngineError> {
    let engine = env.engine;
    let profiles = env.profiles;
    let dir = engine.dir();
    let Some(root) = engine.roots().get(&key.repo) else {
        return Ok(Action::Skip);
    };
    let file = root.join(key.path.as_str());
    let fin = relative_shard(dir.root(), &dir.shard_path(&key.repo, &key.path));
    if let Some(sha) = key
        .path
        .as_str()
        .strip_prefix(crate::history::HISTORY_PREFIX)
    {
        return plan_history(engine, key, seq, root, sha, fin);
    }
    let content = match std::fs::read(&file) {
        Ok(c) => Some(c),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(EngineError::io(&file, e)),
    };
    let existing = dir.read_shard(&key.repo, &key.path).unwrap_or_default();
    let Some(content) = content else {
        let has_shard = dir.shard_path(&key.repo, &key.path).exists();
        return Ok(Action::Delete {
            fin: has_shard.then_some(fin),
        });
    };
    let Some(extractor) = extractor_for(engine.registry(), profiles, &key.path) else {
        // No extractor applies any more (a `select` was removed, or the extension is gone):
        // a shard left from before is stale.
        let has_shard = dir.shard_path(&key.repo, &key.path).exists();
        return Ok(if has_shard {
            Action::Delete { fin: Some(fin) }
        } else {
            Action::Skip
        });
    };
    let pair = (key.repo.clone(), extractor.id());
    let ctx: Vec<u8> = env.contexts.get(&pair).cloned().unwrap_or_default();
    // The profiles that can apply to this extension are part of what the shard depends on.
    let mut keyed = ctx.clone();
    if let Some(h) = profiles.hash_for(extractor.id()) {
        keyed.extend_from_slice(&h);
    }
    // Revision key, in a fixed order: blob id, context, profile hash, revision salt, embed
    // signature, guest key bytes. Every input takes part.
    // An extension that changes what it extracts from unchanged input bumps its salt.
    let salt = extractor.revision_salt();
    if !salt.is_empty() {
        keyed.extend_from_slice(b"salt:");
        keyed.extend_from_slice(salt.as_bytes());
    }
    // A host's output also depends on the guest set and on each guest's projected context.
    let no_guests = GuestContexts::default();
    let guests = if extractor.embeds() {
        let g = env.guests.get(&pair).unwrap_or(&no_guests);
        if let Some(sig) = env.embed_sig {
            keyed.extend_from_slice(&sig);
        }
        keyed.extend_from_slice(&g.key_bytes());
        g
    } else {
        &no_guests
    };
    let revision = context::revision(&git_blob_id(&content), &keyed);
    if existing.is_some_and(|f| f.provenance.revision == revision) {
        return Ok(Action::Skip);
    }
    match extractor.extract_full(&ExtractInput {
        repo: &key.repo,
        path: &key.path,
        revision: &revision,
        content: &content,
        context: &ctx,
    }) {
        Ok(katalogr_model::Extraction {
            mut fragment,
            mut errors,
            embedded,
        }) => {
            errors.extend(embed::apply(
                engine.registry(),
                &key.repo,
                &key.path,
                extractor.id(),
                &embedded,
                guests,
                &mut fragment,
            ));
            profiles.apply(&mut fragment);
            let errors = errors
                .into_iter()
                .map(|e| ErrorRow {
                    span: match &e {
                        ExtractError::Parse { span, .. } => *span,
                        ExtractError::Unsupported(_) => None,
                    },
                    message: e.to_string(),
                })
                .collect();
            let leaf = Utf8Path::new(&fin).file_name().unwrap_or("shard");
            let parent = Utf8Path::new(&fin).parent().unwrap_or(Utf8Path::new(""));
            Ok(Action::Replace {
                fragment: Box::new(fragment),
                errors,
                tmp: parent.join(format!(".tmp-{seq}-{leaf}")).to_string(),
                fin,
            })
        }
        Err(e) => Ok(Action::Failed(e.to_string())),
    }
}

/// A history path (`@history/<sha>`) names a commit, not a file: its fragment comes from the
/// repository's history. A commit's fragment never changes, so an existing shard is current.
fn plan_history(
    engine: &Engine,
    key: &PendingKey,
    seq: u64,
    root: &Utf8Path,
    sha: &str,
    fin: String,
) -> Result<Action, EngineError> {
    let dir = engine.dir();
    let has_shard = dir.shard_path(&key.repo, &key.path).exists();
    let existing = dir.read_shard(&key.repo, &key.path).unwrap_or_default();
    if existing.is_some_and(|f| f.provenance.revision == sha) {
        return Ok(Action::Skip);
    }
    let opts = crate::history::HistoryOptions::default();
    match crate::history::history_fragment(&key.repo, root.as_std_path(), sha, &opts) {
        Ok(Some(fragment)) => {
            let leaf = Utf8Path::new(&fin).file_name().unwrap_or("shard");
            let parent = Utf8Path::new(&fin).parent().unwrap_or(Utf8Path::new(""));
            Ok(Action::Replace {
                fragment: Box::new(fragment),
                errors: Vec::new(),
                tmp: parent.join(format!(".tmp-{seq}-{leaf}")).to_string(),
                fin,
            })
        }
        Ok(None) => Ok(Action::Delete {
            fin: has_shard.then_some(fin),
        }),
        Err(e) => Ok(Action::Failed(e.to_string())),
    }
}

pub(super) fn relative_shard(root: &Utf8Path, full: &Utf8Path) -> String {
    full.strip_prefix(root).unwrap_or(full).to_string()
}

pub(super) fn write_synced(path: &Utf8Path, bytes: &[u8]) -> Result<(), EngineError> {
    use std::io::Write;
    let io = |e| EngineError::io(path, e);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| EngineError::io(parent, e))?;
    }
    let mut f = File::create(path).map_err(io)?;
    f.write_all(bytes).map_err(io)?;
    f.sync_all().map_err(io)
}
