//! Peers of a project (FEAT-020): where they live, how each is opened read-only, and which of
//! their repos the user may read. A peer is read through a read-only handle to its graph database
//! and nothing is stored across projects (ADR-0038).

use crate::error::DistError;
use crate::{Katalogr, build_registry, roots};
use camino::{Utf8Path, Utf8PathBuf};
use katalogr_engine::Engine;
use katalogr_engine::consistency::recorded_position;
use katalogr_query::{PeerState, PositionRef, SourceAccess};
use katalogr_store::{Kindex, ProjectDir, ProjectName, Reader, StoreError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::time::Instant;

/// The project setting that names the peers.
pub const PEERS_SETTING: &str = "workspace.peers";
/// `<project>/.cache/peers.yaml`: alias to the directory of another kindex on this machine.
const LOCATIONS_FILE: &str = "peers.yaml";
/// Most rounds of `impact` across peers.
pub const MAX_IMPACT_ROUNDS: u32 = 3;
/// The meta key an imported snapshot writes for a repo it holds rows for (FEAT-097).
fn snapshot_repo_key(repo: &str) -> String {
    format!("snapshot.repo.{repo}")
}

/// One named peer: a project of this kindex, or of the kindex the alias locates.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PeerSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kindex: Option<String>,
    pub project: String,
}

/// Which repos of a peer the user can read: a readable checkout on this machine, or a repo a
/// snapshot was imported for. Opened once per peer.
#[derive(Clone, Debug, Default)]
pub struct RootsAccess {
    project: String,
    readable: BTreeSet<String>,
}

impl RootsAccess {
    /// The repos of `config` that have a readable root in `roots` or a snapshot key in the cache.
    fn open(
        project: &str,
        repos: &[String],
        roots: &BTreeMap<String, Utf8PathBuf>,
        snap: Option<&dyn SnapshotKeys>,
    ) -> RootsAccess {
        let mut readable = BTreeSet::new();
        for repo in repos {
            let checkout = roots
                .get(repo)
                .is_some_and(|r| std::fs::read_dir(r).is_ok());
            let imported = snap.is_some_and(|s| s.has(&snapshot_repo_key(repo)));
            if checkout || imported {
                readable.insert(repo.clone());
            }
        }
        RootsAccess {
            project: project.to_string(),
            readable,
        }
    }

    /// Repos the user can read, sorted.
    pub fn readable(&self) -> impl Iterator<Item = &str> {
        self.readable.iter().map(String::as_str)
    }
}

impl SourceAccess for RootsAccess {
    fn can_read(&self, project: &str, repo: &str, _path: &str) -> bool {
        project == self.project && self.readable.contains(repo)
    }
}

/// A meta lookup, so the roots can be read from any snapshot.
trait SnapshotKeys {
    fn has(&self, key: &str) -> bool;
}

impl<R: Reader> SnapshotKeys for R {
    fn has(&self, key: &str) -> bool {
        self.meta(key).ok().flatten().is_some()
    }
}

/// One peer for one read.
pub(crate) struct OpenPeer {
    pub spec: PeerSpec,
    pub state: PeerState,
    pub engine: Option<Engine>,
    pub access: RootsAccess,
    /// The facts version of the peer's snapshot.
    pub facts: String,
}

/// SHA-256 hex of each peer's kindex alias, project, state, sorted position texts and facts.
/// The same for the same peers in any order.
pub(crate) fn fingerprint(peers: &[OpenPeer]) -> String {
    let mut rows: Vec<String> = peers
        .iter()
        .map(|p| {
            let mut positions: Vec<String> = p.state.positions.iter().map(|x| x.text()).collect();
            positions.sort();
            format!(
                "{}\0{}\0{}\0{}\0{}",
                p.spec.kindex.as_deref().unwrap_or(""),
                p.spec.project,
                p.state.state,
                positions.join(","),
                p.facts
            )
        })
        .collect();
    rows.sort();
    let mut h = Sha256::new();
    for r in rows {
        h.update((r.len() as u64).to_le_bytes());
        h.update(r.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn read_locations(cache_dir: &Utf8Path) -> Result<BTreeMap<String, Utf8PathBuf>, DistError> {
    let p = cache_dir.join(LOCATIONS_FILE);
    match std::fs::read_to_string(&p) {
        Ok(text) => serde_yaml_ng::from_str(&text).map_err(|e| DistError::io(&p, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(DistError::io(&p, e)),
    }
}

/// Atomic replace, sorted by construction.
fn write_locations(
    cache_dir: &Utf8Path,
    locations: &BTreeMap<String, Utf8PathBuf>,
) -> Result<(), DistError> {
    let p = cache_dir.join(LOCATIONS_FILE);
    let io = |e: std::io::Error| DistError::io(&p, e);
    std::fs::create_dir_all(cache_dir).map_err(io)?;
    let yaml = serde_yaml_ng::to_string(locations).map_err(|e| DistError::io(&p, e))?;
    let mut t = tempfile::NamedTempFile::new_in(cache_dir).map_err(io)?;
    t.write_all(yaml.as_bytes()).map_err(io)?;
    t.as_file().sync_all().map_err(io)?;
    t.persist(&p).map_err(|e| io(e.error))?;
    Ok(())
}

/// An alias is a short name, never a path.
fn valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && !alias.starts_with('.')
}

fn state_of(spec: &PeerSpec, state: &str, fix: Option<String>) -> PeerState {
    PeerState {
        project: spec.project.clone(),
        kindex: spec.kindex.clone(),
        state: state.to_string(),
        positions: Vec::new(),
        repos_absent: Vec::new(),
        stale: false,
        fix,
    }
}

fn closed(spec: &PeerSpec, state: &str, fix: Option<String>) -> OpenPeer {
    OpenPeer {
        state: state_of(spec, state, fix),
        spec: spec.clone(),
        engine: None,
        access: RootsAccess::default(),
        facts: String::new(),
    }
}

pub(crate) fn index_fix(spec: &PeerSpec) -> String {
    format!(
        "katalogr --kindex <dir> --project {} index --full",
        spec.project
    )
}

/// Spec to `OpenPeer`: the first step that applies decides the state.
fn open_one(
    spec: &PeerSpec,
    locations: &Result<BTreeMap<String, Utf8PathBuf>, ()>,
    own_kindex: &Utf8Path,
    deadline: Instant,
) -> OpenPeer {
    if Instant::now() >= deadline {
        return closed(spec, "timeout", None);
    }
    let kindex_dir = match &spec.kindex {
        None => own_kindex.to_owned(),
        Some(alias) => match locations.as_ref().ok().and_then(|l| l.get(alias)) {
            Some(d) => d.clone(),
            None => {
                return closed(
                    spec,
                    "no-location",
                    Some(format!("katalogr workspace locate {alias} <dir>")),
                );
            }
        },
    };
    let found = Kindex::open(&kindex_dir)
        .and_then(|k| ProjectName::new(&spec.project).and_then(|n| k.project(&n)));
    let dir: ProjectDir = match found {
        Ok(d) => d,
        Err(StoreError::FormatTooNew { .. }) => {
            return closed(spec, "newer-format", Some("update katalogr".to_string()));
        }
        Err(
            StoreError::ProjectNotFound(_)
            | StoreError::NotAKindex(_)
            | StoreError::NotAProject(_)
            | StoreError::BadProjectName(_),
        ) => return closed(spec, "not-found", None),
        Err(_) => return closed(spec, "unavailable", Some(index_fix(spec))),
    };
    let repos: Vec<String> = dir.config().repos.iter().map(|r| r.name.clone()).collect();
    let peer_roots = roots::read(&dir.cache_dir()).unwrap_or_default();
    if !dir.cache_dir().join("index.redb").is_file() {
        let mut p = closed(spec, "no-index", Some(index_fix(spec)));
        p.access = RootsAccess::open(&spec.project, &repos, &peer_roots, None);
        p.state.repos_absent = repos;
        return p;
    }
    // Base only: a peer answers from what its default branch says, never a checkout's context.
    let engine = match Engine::open_reader(dir.base(), build_registry(), BTreeMap::new()) {
        Ok(Some(e)) => e,
        _ => return closed(spec, "unavailable", Some(index_fix(spec))),
    };
    let Ok(snap) = engine.cache().read() else {
        return closed(spec, "unavailable", Some(index_fix(spec)));
    };
    let mut positions = Vec::new();
    let mut absent = Vec::new();
    for repo in &repos {
        let base = snap
            .meta(&crate::contexts::base_commit_key(repo))
            .ok()
            .flatten()
            .and_then(|b| String::from_utf8(b).ok())
            .filter(|c| !c.is_empty());
        let pos = match base {
            Some(commit) => Some(PositionRef {
                repo: repo.clone(),
                commit: Some(commit),
                dirty: None,
            }),
            None => recorded_position(&snap, repo).ok().flatten(),
        };
        match pos {
            Some(p) => positions.push(p),
            None => absent.push(repo.clone()),
        }
    }
    positions.sort();
    let stale = snap
        .pending_entries(None)
        .map(|p| !p.is_empty())
        .unwrap_or(true);
    let Ok(facts) = engine.facts_version(&snap) else {
        drop(snap);
        return closed(spec, "unavailable", Some(index_fix(spec)));
    };
    let access = RootsAccess::open(&spec.project, &repos, &peer_roots, Some(&snap));
    drop(snap);
    // A repo the user cannot read shows nowhere, its position included, so that restricted
    // structure changes no byte of an answer (R-11).
    positions.retain(|p| access.readable.contains(&p.repo));
    let mut state = state_of(spec, "ok", None);
    state.positions = positions;
    state.repos_absent = absent;
    state.stale = stale;
    OpenPeer {
        spec: spec.clone(),
        state,
        engine: Some(engine),
        access,
        facts,
    }
}

impl Katalogr {
    fn own_name(&self) -> String {
        self.project
            .project_root()
            .file_name()
            .unwrap_or("")
            .to_string()
    }

    /// The peers named, sorted, deduplicated. A malformed setting is a usage error.
    pub fn peers(&self) -> Result<Vec<PeerSpec>, DistError> {
        let Some(value) = self.project.config().settings.get(PEERS_SETTING) else {
            return Ok(Vec::new());
        };
        let mut peers: Vec<PeerSpec> = serde_yaml_ng::from_value(value.clone())
            .map_err(|e| DistError::usage(format!("setting {PEERS_SETTING} is malformed: {e}")))?;
        let own = self.own_name();
        for p in &peers {
            if p.kindex.as_deref().is_some_and(|a| !valid_alias(a)) {
                return Err(DistError::usage(format!(
                    "setting {PEERS_SETTING}: kindex must be an alias, not a path"
                )));
            }
        }
        // A project naming itself reads nothing new.
        peers.retain(|p| !(p.kindex.is_none() && p.project == own));
        peers.sort();
        peers.dedup();
        Ok(peers)
    }

    /// Each peer's state, with no answer: `katalogr workspace status`.
    pub fn workspace_status(&self, opts: &crate::Options) -> Result<Vec<PeerState>, DistError> {
        let deadline = Instant::now() + opts.timeout()?;
        Ok(self
            .open_peers(deadline)?
            .into_iter()
            .map(|p| p.state)
            .collect())
    }

    /// Adds a peer to `workspace.peers`. A project naming itself is a usage error.
    pub fn workspace_add(&self, project: &str, kindex: Option<&str>) -> Result<(), DistError> {
        ProjectName::new(project)?;
        if let Some(a) = kindex
            && !valid_alias(a)
        {
            return Err(DistError::usage(format!(
                "{a:?}: a kindex is named by an alias, not a path; \
                 record where it is with katalogr workspace locate"
            )));
        }
        if kindex.is_none() && project == self.own_name() {
            return Err(DistError::usage(format!(
                "project {project} cannot name itself as a peer"
            )));
        }
        let mut peers = self.peers()?;
        peers.push(PeerSpec {
            kindex: kindex.map(str::to_string),
            project: project.to_string(),
        });
        peers.sort();
        peers.dedup();
        let value = serde_yaml_ng::to_value(&peers).map_err(|e| DistError::usage(e.to_string()))?;
        self.project.set_setting(PEERS_SETTING, value)?;
        Ok(())
    }

    /// Records alias to directory in this machine's `.cache/peers.yaml`. A directory with no
    /// kindex is a usage error.
    pub fn workspace_locate(&self, alias: &str, dir: &std::path::Path) -> Result<(), DistError> {
        if !valid_alias(alias) {
            return Err(DistError::usage(format!(
                "{alias:?}: an alias is letters, digits, `-`, `_` and `.`"
            )));
        }
        let abs = crate::setup::absolute(dir)?;
        let abs = Utf8PathBuf::from_path_buf(
            std::fs::canonicalize(&abs).map_err(|e| DistError::io(&abs, e))?,
        )
        .map_err(|p| DistError::usage(format!("{}: path is not UTF-8", p.display())))?;
        Kindex::open(&abs).map_err(|_| DistError::usage(format!("{abs} is not a kindex")))?;
        let cache = self.project.cache_dir();
        let mut stored = read_locations(&cache)?;
        stored.insert(alias.to_string(), abs);
        write_locations(&cache, &stored)
    }

    /// Opens every peer, sorted. Never fails for one peer's sake: a peer that cannot open is a
    /// state. An unreadable `peers.yaml` is a warning, and each alias is then `no-location`.
    pub(crate) fn open_peers(&self, deadline: Instant) -> Result<Vec<OpenPeer>, DistError> {
        let peers = self.peers()?;
        if peers.is_empty() {
            return Ok(Vec::new());
        }
        let locations = if peers.iter().any(|p| p.kindex.is_some()) {
            match read_locations(&self.project.cache_dir()) {
                Ok(l) => Ok(l),
                Err(_) => {
                    self.note(
                        "workspace: this machine's peer locations are unreadable".to_string(),
                    );
                    Err(())
                }
            }
        } else {
            Ok(BTreeMap::new())
        };
        let own = self.project.kindex_root().to_owned();
        Ok(peers
            .iter()
            .map(|s| open_one(s, &locations, &own, deadline))
            .collect())
    }
}
