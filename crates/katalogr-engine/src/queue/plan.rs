//! Planning one queued path: read the file, extract, profile and embed (FEAT-047, FEAT-072).
//!
//! Planning is a pure function of the file content, the [`PlanEnv`] (loaded once per round from
//! the round's snapshot), the profiles and the registry, so any number of paths can be planned
//! at once without touching the cache.

use crate::census::extractor_for;
use crate::consistency::Engine;
use crate::context;
use crate::embed::{self, GuestContexts};
use crate::error::EngineError;
use crate::parallel::{Parallelism, wave_size};
use crate::profiles::ProfileSet;
use crate::scan::glob_set;
use crate::stitch::{PreparedFragment, Stitcher};
use camino::Utf8Path;
use katalogr_model::{ExtractError, ExtractInput, Fragment, RelPath};
use katalogr_store::blob::git_blob_id;
use katalogr_store::{ErrorRow, PendingKey, ReadTx, Reader, scan_for_absolute};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

/// What one pending path needs.
pub(super) enum Action {
    /// Nothing to publish; the entry is dropped.
    Skip,
    Replace {
        fragment: Box<Fragment>,
        errors: Vec<ErrorRow>,
        /// Bytes of the source file, for the round's byte cap.
        source_len: u64,
    },
    Delete,
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
    /// The stored file revision (SHA-256 of the revision string) of each path of the round.
    revisions: BTreeMap<(String, RelPath), [u8; 32]>,
    /// Paths of the round that git ignores and does not track: treated as gone (FEAT-099).
    not_admitted: BTreeSet<(String, RelPath)>,
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
        let mut revisions = BTreeMap::new();
        for (key, _) in items {
            if let Some(h) = snap.file_revision(&key.repo, &key.path)? {
                revisions.insert((key.repo.clone(), key.path.clone()), h);
            }
        }
        let mut not_admitted = BTreeSet::new();
        let mut by_repo: BTreeMap<&str, Vec<&RelPath>> = BTreeMap::new();
        for (key, _) in items {
            if !key
                .path
                .as_str()
                .starts_with(crate::history::HISTORY_PREFIX)
            {
                by_repo.entry(&key.repo).or_default().push(&key.path);
            }
        }
        for (repo, paths) in by_repo {
            let Some(source) = engine.source(repo) else {
                continue;
            };
            let (gone, warnings) = source.not_admitted(paths);
            for w in warnings {
                eprintln!("warning: {w}");
            }
            not_admitted.extend(gone.into_iter().map(|p| (repo.to_string(), p)));
        }
        // R-12: a path that has a file revision but that the repo's globs no longer admit is
        // treated as gone, so a rescan removes it.
        let mut globs: BTreeMap<&str, Option<(globset::GlobSet, globset::GlobSet)>> =
            BTreeMap::new();
        for (key, _) in items {
            if !revisions.contains_key(&(key.repo.clone(), key.path.clone()))
                || key
                    .path
                    .as_str()
                    .starts_with(crate::history::HISTORY_PREFIX)
            {
                continue;
            }
            let set = globs.entry(&key.repo).or_insert_with(|| {
                let (include, exclude) = engine.repo_filters(&key.repo);
                Some((glob_set(&include).ok()?, glob_set(&exclude).ok()?))
            });
            if let Some((include, exclude)) = set
                && !(include.is_match(key.path.as_str()) && !exclude.is_match(key.path.as_str()))
            {
                not_admitted.insert((key.repo.clone(), key.path.clone()));
            }
        }
        Ok(PlanEnv {
            engine,
            not_admitted,
            profiles,
            contexts,
            guests,
            embed_sig: embed::signature(engine.registry()),
            revisions,
        })
    }
}

/// Decide what one queued path needs, from the file's current content.
pub(super) fn plan_one(env: &PlanEnv, key: &PendingKey) -> Result<Action, EngineError> {
    let engine = env.engine;
    let profiles = env.profiles;
    let Some(root) = engine.roots().get(&key.repo) else {
        return Ok(Action::Skip);
    };
    let held = env.revisions.get(&(key.repo.clone(), key.path.clone()));
    if let Some(sha) = key
        .path
        .as_str()
        .strip_prefix(crate::history::HISTORY_PREFIX)
    {
        return plan_history(key, root, sha, held);
    }
    let Some(source) = engine.source(&key.repo) else {
        return Ok(Action::Skip);
    };
    let content = if env
        .not_admitted
        .contains(&(key.repo.clone(), key.path.clone()))
    {
        None
    } else {
        source.read(&key.path)?
    };
    let delete_or_skip = || {
        if held.is_some() {
            Action::Delete
        } else {
            Action::Skip
        }
    };
    let Some(content) = content else {
        return Ok(delete_or_skip());
    };
    let Some(extractor) = extractor_for(engine.registry(), profiles, &key.path) else {
        // No extractor applies any more (a `select` was removed, or the extension is gone).
        return Ok(delete_or_skip());
    };
    let pair = (key.repo.clone(), extractor.id());
    let ctx: Vec<u8> = env.contexts.get(&pair).cloned().unwrap_or_default();
    // The revision depends on, in a fixed order: blob id, extension id, context, profile hash,
    // engine revision, revision salt, embed signature and guest key bytes. A host's output also
    // depends on the guest set and on each guest's projected context.
    let no_guests = GuestContexts::default();
    let (guests, embed_sig, guest_bytes) = if extractor.embeds() {
        let g = env.guests.get(&pair).unwrap_or(&no_guests);
        (g, env.embed_sig, g.key_bytes())
    } else {
        (&no_guests, None, Vec::new())
    };
    let revision = context::revision_of(
        &git_blob_id(&content),
        &context::RevisionFields {
            extension: extractor.id(),
            context: &ctx,
            profile: profiles.hash_for(extractor.id()),
            salt: extractor.revision_salt(),
            embed: embed_sig,
            guests: &guest_bytes,
        },
    );
    let digest: [u8; 32] = Sha256::digest(revision.as_bytes()).into();
    if held == Some(&digest) {
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
            Ok(Action::Replace {
                fragment: Box::new(fragment),
                errors,
                source_len: content.len() as u64,
            })
        }
        Err(e) => Ok(Action::Failed(e.to_string())),
    }
}

/// A history path (`@history/<sha>`) names a commit, not a file: its fragment comes from the
/// repository's history. A commit's fragment never changes, so a stored revision for `sha` is
/// current.
fn plan_history(
    key: &PendingKey,
    root: &Utf8Path,
    sha: &str,
    held: Option<&[u8; 32]>,
) -> Result<Action, EngineError> {
    let digest: [u8; 32] = Sha256::digest(sha.as_bytes()).into();
    if held == Some(&digest) {
        return Ok(Action::Skip);
    }
    let opts = crate::history::HistoryOptions::default();
    match crate::history::history_fragment(&key.repo, root.as_std_path(), sha, &opts) {
        Ok(Some(fragment)) => Ok(Action::Replace {
            fragment: Box::new(fragment),
            errors: Vec::new(),
            source_len: 0,
        }),
        Ok(None) => Ok(if held.is_some() {
            Action::Delete
        } else {
            Action::Skip
        }),
        Err(e) => Ok(Action::Failed(e.to_string())),
    }
}

/// A planned path with everything computed off the writer: the guard has run and the stitch
/// inputs are prepared.
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
        revision: String,
        source_len: u64,
    },
    Delete,
    Failed(String),
}

/// Run the absolute-path guard on a `Replace` and prepare its stitch inputs. Other actions pass
/// through. No encoding happens.
pub(super) fn prepare_one(engine: &Engine, p: Planned) -> Result<Prepared, EngineError> {
    let Planned { key, seq, action } = p;
    let ready = match action {
        Action::Skip => Ready::Skip,
        Action::Delete => Ready::Delete,
        Action::Failed(m) => Ready::Failed(m),
        Action::Replace {
            fragment,
            errors,
            source_len,
        } => {
            // A guard refusal fails only this path (an index error row, no rows, the earlier
            // artifact kept); any other error aborts the batch (FEAT-073).
            if let Err(g) = scan_for_absolute(&fragment, Some(engine.dir().kindex_root())) {
                return Ok(Prepared {
                    key,
                    seq,
                    ready: Ready::Failed(format!("shard refused: {g}")),
                });
            }
            let revision = fragment.provenance.revision.clone();
            let prepared = Box::new(Stitcher::prepare(*fragment)?);
            Ready::Replace {
                prepared,
                errors,
                revision,
                source_len,
            }
        }
    };
    Ok(Prepared { key, seq, ready })
}

/// Whether to start another wave: always for the first, then only while the deadline has not
/// passed and the source bytes held are below the cap (R-4, R-5).
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
        let action = plan_one(env, key)?;
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
            if let Ready::Replace { source_len, .. } = &p.ready {
                bytes += source_len;
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
