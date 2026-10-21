//! Planning one queued path: read the file, extract, profile and embed (FEAT-047, FEAT-072).
//!
//! Planning is a pure function of the file content, the [`PlanEnv`] (loaded once per round from
//! the round's snapshot), the profiles and the registry, so any number of paths can be planned
//! at once without touching the cache.

use crate::consistency::Engine;
use crate::context;
use crate::embed::{self, GuestContexts};
use crate::error::EngineError;
use crate::parallel::{Parallelism, wave_size};
use crate::profiles::ProfileSet;
use crate::registry::ExtensionRegistry;
use crate::stitch::{PreparedFragment, Stitcher};
use camino::Utf8Path;
use katalogr_model::{ExtractError, ExtractInput, Extractor, Fragment, RelPath};
use katalogr_store::blob::git_blob_id;
use katalogr_store::{ErrorRow, PendingKey, ReadTx, Reader};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::time::Instant;

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

/// A planned path with everything computed off the writer: the shard is encoded, hashed and
/// written to its temp file, and the stitch inputs are prepared.
pub(super) struct Prepared {
    pub(super) key: PendingKey,
    pub(super) seq: u64,
    pub(super) ready: Ready,
}

pub(super) enum Ready {
    Skip,
    Replace {
        prepared: Box<PreparedFragment>,
        errors: Vec<ErrorRow>,
        hash: [u8; 32],
        encoded_len: u64,
        tmp: String,
        fin: String,
    },
    Delete {
        fin: Option<String>,
    },
    Failed(String),
}

/// Encode, hash and write the temp shard of a `Replace`, then prepare its stitch inputs. Other
/// actions pass through.
pub(super) fn prepare_one(engine: &Engine, p: Planned) -> Result<Prepared, EngineError> {
    let Planned { key, seq, action } = p;
    let ready = match action {
        Action::Skip => Ready::Skip,
        Action::Delete { fin } => Ready::Delete { fin },
        Action::Failed(m) => Ready::Failed(m),
        Action::Replace {
            fragment,
            errors,
            tmp,
            fin,
        } => {
            let dir = engine.dir();
            let bytes = dir.encode_shard(&fragment)?;
            let hash = <[u8; 32]>::from(Sha256::digest(&bytes));
            write_synced(&dir.root().join(&tmp), &bytes)?;
            let prepared = Box::new(Stitcher::prepare(*fragment)?);
            Ready::Replace {
                prepared,
                errors,
                hash,
                encoded_len: bytes.len() as u64,
                tmp,
                fin,
            }
        }
    };
    Ok(Prepared { key, seq, ready })
}

/// Whether to start another wave: always for the first, then only while the deadline has not
/// passed and the encoded bytes held are below the cap (R-4, R-5).
pub(super) fn next_wave(
    done: usize,
    bytes: u64,
    now: Instant,
    deadline: Instant,
    cap: u64,
) -> bool {
    done == 0 || (now < deadline && bytes < cap)
}

/// Plan and prepare the paths of a round in waves of `wave_size(threads)`, on `pool` when there
/// is one. The result is a prefix of `items` in batch order; the first error in batch order
/// wins (R-7).
pub(super) fn run_batch(
    env: &PlanEnv,
    items: &[(PendingKey, u64)],
    deadline: Instant,
    par: Parallelism,
    pool: Option<&rayon::ThreadPool>,
) -> Result<Vec<Prepared>, EngineError> {
    let engine = env.engine;
    let one = |(key, seq): &(PendingKey, u64)| -> Result<Prepared, EngineError> {
        let action = plan_one(env, key, *seq)?;
        prepare_one(
            engine,
            Planned {
                key: key.clone(),
                seq: *seq,
                action,
            },
        )
    };
    let mut out: Vec<Prepared> = Vec::new();
    let mut bytes = 0u64;
    for wave in items.chunks(wave_size(par.threads)) {
        if !next_wave(out.len(), bytes, Instant::now(), deadline, par.batch_bytes) {
            break;
        }
        let results: Vec<Result<Prepared, EngineError>> = match pool {
            Some(pool) => pool.install(|| wave.par_iter().map(one).collect()),
            None => {
                // Sequential: stop at the first error without planning the rest.
                let mut v = Vec::with_capacity(wave.len());
                for it in wave {
                    let r = one(it);
                    let failed = r.is_err();
                    v.push(r);
                    if failed {
                        break;
                    }
                }
                v
            }
        };
        for r in results {
            let p = r?;
            if let Ready::Replace { encoded_len, .. } = &p.ready {
                bytes += encoded_len;
            }
            out.push(p);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn stops_after_byte_cap() {
        let now = Instant::now();
        let far = now + Duration::from_secs(60);
        assert!(next_wave(0, 0, now, far, 100), "the first wave always runs");
        assert!(next_wave(4, 99, now, far, 100));
        assert!(!next_wave(4, 100, now, far, 100));
        assert!(!next_wave(4, 101, now, far, 100));
        assert!(next_wave(0, 1000, now, far, 100), "a round is never empty");
    }

    #[test]
    fn deadline_checked_between_waves() {
        let now = Instant::now();
        let past = now - Duration::from_millis(1);
        assert!(next_wave(0, 0, now, past, u64::MAX), "at least one wave");
        assert!(!next_wave(8, 0, now, past, u64::MAX));
        assert!(next_wave(8, 0, now, now + Duration::from_secs(1), u64::MAX));
    }
}
