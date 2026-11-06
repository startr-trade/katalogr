//! The kindex repository and its projects.

use crate::context::{ContextId, ContextManifest};
use crate::error::StoreError;
use crate::shard;
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;

pub const SHARD_FORMAT: u32 = 2;
const MIN_KATALOGR: &str = "0.2.0";
const GITIGNORE: &str = "projects/*/.cache/\nprojects/*/shards/\n";

fn is_lower_alnum(c: u8) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit()
}

/// `[a-z0-9][a-z0-9-]{0,62}`
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectName(String);

impl ProjectName {
    pub fn new(s: &str) -> Result<ProjectName, StoreError> {
        let b = s.as_bytes();
        let ok = !b.is_empty()
            && b.len() <= 63
            && is_lower_alnum(b[0])
            && b.iter().all(|c| is_lower_alnum(*c) || *c == b'-');
        if ok {
            Ok(ProjectName(s.to_string()))
        } else {
            Err(StoreError::BadProjectName(s.to_string()))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProjectName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for ProjectName {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProjectName {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        ProjectName::new(&s).map_err(serde::de::Error::custom)
    }
}

fn default_include() -> Vec<String> {
    vec!["**".to_string()]
}

fn default_exclude() -> Vec<String> {
    vec![
        ".git/**".to_string(),
        "target/**".to_string(),
        "node_modules/**".to_string(),
    ]
}

/// Fields are declared in key order so the serialised form has sorted keys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoConfig {
    #[serde(default = "default_exclude")]
    pub exclude: Vec<String>,
    #[serde(default = "default_include")]
    pub include: Vec<String>,
    /// `[a-z0-9][a-z0-9-_]*`
    pub name: String,
}

impl RepoConfig {
    /// A repo with the default include and exclude globs.
    pub fn new(name: &str) -> RepoConfig {
        RepoConfig {
            exclude: default_exclude(),
            include: default_include(),
            name: name.to_string(),
        }
    }

    fn validate(&self) -> Result<(), StoreError> {
        let b = self.name.as_bytes();
        let ok = !b.is_empty()
            && is_lower_alnum(b[0])
            && b.iter()
                .all(|c| is_lower_alnum(*c) || *c == b'-' || *c == b'_');
        if ok {
            Ok(())
        } else {
            Err(StoreError::BadRepoName(self.name.clone()))
        }
    }
}

/// Fields are declared in key order so the serialised form has sorted keys.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub repos: Vec<RepoConfig>,
    #[serde(default)]
    pub settings: BTreeMap<String, serde_yaml_ng::Value>,
}

/// `format.json`; fields are declared in key order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatFile {
    pub min_katalogr: String,
    pub shard_format: u32,
}

pub(crate) fn write_atomic(path: &Utf8Path, bytes: &[u8]) -> Result<(), StoreError> {
    let dir = path.parent().unwrap_or(Utf8Path::new("."));
    let io = |e| StoreError::io(path, e);
    std::fs::create_dir_all(dir).map_err(io)?;
    let mut t = tempfile::NamedTempFile::new_in(dir).map_err(io)?;
    t.write_all(bytes).map_err(io)?;
    t.as_file().sync_all().map_err(io)?;
    t.persist(path).map_err(|e| StoreError::io(path, e.error))?;
    Ok(())
}

pub(crate) fn read_to_string(path: &Utf8Path) -> Result<String, StoreError> {
    std::fs::read_to_string(path).map_err(|e| StoreError::io(path, e))
}

fn mkdir(path: &Utf8Path) -> Result<(), StoreError> {
    std::fs::create_dir_all(path).map_err(|e| StoreError::io(path, e))
}

#[derive(Clone, Debug)]
pub struct Kindex {
    root: Utf8PathBuf,
}

impl Kindex {
    pub fn root(&self) -> &Utf8Path {
        &self.root
    }

    /// Open an existing kindex (a work tree holding `projects/`).
    pub fn open(root: &Utf8Path) -> Result<Kindex, StoreError> {
        if !root.join("projects").is_dir() {
            return Err(StoreError::NotAKindex(root.to_string()));
        }
        Ok(Kindex {
            root: root.to_owned(),
        })
    }

    /// Create the directory if missing, initialise a git repository when it is not one, write
    /// `.gitignore` if absent. Idempotent.
    pub fn init(root: &Utf8Path) -> Result<Kindex, StoreError> {
        mkdir(&root.join("projects"))?;
        if !root.join(".git").exists() {
            gix::init(root.as_std_path()).map_err(|e| StoreError::Git(e.to_string()))?;
        }
        let ignore = root.join(".gitignore");
        if !ignore.exists() {
            write_atomic(&ignore, GITIGNORE.as_bytes())?;
        }
        Ok(Kindex {
            root: root.to_owned(),
        })
    }

    /// Reads `projects/<name>/format.json` without the newer-format refusal that
    /// [`Kindex::project`] applies, so a health check can report a format it cannot open.
    pub fn format_file(&self, name: &ProjectName) -> Result<FormatFile, StoreError> {
        let fpath = self.project_root(name).join("format.json");
        serde_json::from_str(&read_to_string(&fpath)?).map_err(|e| StoreError::Parse {
            path: fpath.to_string(),
            message: e.to_string(),
        })
    }

    fn project_root(&self, name: &ProjectName) -> Utf8PathBuf {
        self.root.join("projects").join(name.as_str())
    }

    /// R-1. Fails with `ProjectExists` if the folder exists.
    pub fn create_project(
        &self,
        name: &ProjectName,
        mut config: ProjectConfig,
    ) -> Result<ProjectDir, StoreError> {
        let dir = self.project_root(name);
        if dir.exists() {
            return Err(StoreError::ProjectExists(name.to_string()));
        }
        config.repos.iter().try_for_each(RepoConfig::validate)?;
        config.repos.sort_by(|a, b| a.name.cmp(&b.name));
        for sub in ["profiles", ".cache"] {
            mkdir(&dir.join(sub))?;
        }
        let format = FormatFile {
            min_katalogr: MIN_KATALOGR.to_string(),
            shard_format: SHARD_FORMAT,
        };
        let v = serde_json::to_value(&format).map_err(|e| StoreError::BadShard(e.to_string()))?;
        write_atomic(
            &dir.join("format.json"),
            shard::to_sorted_json(&v).as_bytes(),
        )?;
        let yaml = serde_yaml_ng::to_string(&config).map_err(|e| StoreError::Parse {
            path: "config.yaml".into(),
            message: e.to_string(),
        })?;
        write_atomic(&dir.join("config.yaml"), yaml.as_bytes())?;
        Ok(ProjectDir {
            project_root: dir.clone(),
            root: dir,
            kindex_root: self.root.clone(),
            config,
        })
    }

    /// Opens a project; refuses a newer shard format (R-8).
    pub fn project(&self, name: &ProjectName) -> Result<ProjectDir, StoreError> {
        let dir = self.project_root(name);
        if !dir.is_dir() {
            return Err(StoreError::ProjectNotFound(name.to_string()));
        }
        let fpath = dir.join("format.json");
        if !fpath.is_file() {
            return Err(StoreError::NotAProject(dir.to_string()));
        }
        let format: FormatFile =
            serde_json::from_str(&read_to_string(&fpath)?).map_err(|e| StoreError::Parse {
                path: fpath.to_string(),
                message: e.to_string(),
            })?;
        if format.shard_format > SHARD_FORMAT {
            return Err(StoreError::FormatTooNew {
                found: format.shard_format,
                supported: SHARD_FORMAT,
                min_katalogr: format.min_katalogr,
            });
        }
        let cpath = dir.join("config.yaml");
        let config: ProjectConfig =
            serde_yaml_ng::from_str(&read_to_string(&cpath)?).map_err(|e| StoreError::Parse {
                path: cpath.to_string(),
                message: e.to_string(),
            })?;
        Ok(ProjectDir {
            project_root: dir.clone(),
            root: dir,
            kindex_root: self.root.clone(),
            config,
        })
    }

    /// Sorted project names.
    pub fn projects(&self) -> Result<Vec<ProjectName>, StoreError> {
        let dir = self.root.join("projects");
        let mut out = Vec::new();
        for e in std::fs::read_dir(&dir).map_err(|e| StoreError::io(&dir, e))? {
            let e = e.map_err(|e| StoreError::io(&dir, e))?;
            if e.path().is_dir()
                && let Some(n) = e.file_name().to_str()
                && let Ok(p) = ProjectName::new(n)
            {
                out.push(p);
            }
        }
        out.sort();
        Ok(out)
    }
}

/// How the files people write in a project (`config.yaml`, `format.json`, `profiles/`, `eval/`
/// and `audit/`) compare with the kindex git repository's HEAD.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitState {
    /// The kindex root is not a git repository.
    NotARepo,
    /// A repository with no commit.
    NoCommits,
    Repo {
        /// HEAD commit id, lower-case hex.
        head: String,
        /// Files whose bytes differ from the blob in HEAD.
        modified: Vec<String>,
        /// Files with no entry in HEAD.
        added: Vec<String>,
        /// Files in HEAD that are gone from disk.
        deleted: Vec<String>,
    },
}

#[derive(Clone, Debug)]
pub struct ProjectDir {
    root: Utf8PathBuf,
    project_root: Utf8PathBuf,
    kindex_root: Utf8PathBuf,
    config: ProjectConfig,
}

impl ProjectDir {
    pub fn root(&self) -> &Utf8Path {
        &self.root
    }

    pub fn config(&self) -> &ProjectConfig {
        &self.config
    }

    /// The same project with its data root at `<project>/.cache/contexts/<id>`; config and
    /// profiles still come from the project.
    pub fn layer(&self, id: &ContextId) -> ProjectDir {
        ProjectDir {
            root: self
                .project_root
                .join(".cache")
                .join("contexts")
                .join(id.as_str()),
            project_root: self.project_root.clone(),
            kindex_root: self.kindex_root.clone(),
            config: self.config.clone(),
        }
    }

    /// The base of this project: the same project with its data root at the project itself.
    pub fn base(&self) -> ProjectDir {
        ProjectDir {
            root: self.project_root.clone(),
            project_root: self.project_root.clone(),
            kindex_root: self.kindex_root.clone(),
            config: self.config.clone(),
        }
    }

    /// Where `config.yaml` and `profiles/` live, for a layer too. `root()` keeps meaning the
    /// data root, so journal and shard paths stay relative to it.
    pub fn project_root(&self) -> &Utf8Path {
        &self.project_root
    }

    pub fn is_layer(&self) -> bool {
        self.root != self.project_root
    }

    /// Manifests under `.cache/contexts/`, sorted by id; an unreadable one is skipped.
    pub fn contexts(&self) -> Result<Vec<ContextManifest>, StoreError> {
        let dir = self.project_root.join(".cache").join("contexts");
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(StoreError::io(&dir, e)),
        };
        let mut out = Vec::new();
        for entry in rd {
            let entry = entry.map_err(|e| StoreError::io(&dir, e))?;
            let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) else {
                continue;
            };
            if let Ok(m) = ContextManifest::read(&path) {
                out.push(m);
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// Reads this project's `format.json` without the newer-format refusal.
    pub fn format_file(&self) -> Result<FormatFile, StoreError> {
        let fpath = self.root.join("format.json");
        serde_json::from_str(&read_to_string(&fpath)?).map_err(|e| StoreError::Parse {
            path: fpath.to_string(),
            message: e.to_string(),
        })
    }

    /// True when a `shards/` folder exists in the project: a format 1 leftover, which nothing
    /// reads or writes.
    pub fn leftover_shards(&self) -> bool {
        self.project_root.join("shards").is_dir()
    }

    pub fn cache_dir(&self) -> Utf8PathBuf {
        self.root.join(".cache")
    }

    /// The root of the kindex repository this project lives in.
    pub fn kindex_root(&self) -> &Utf8Path {
        &self.kindex_root
    }

    /// Compares the files people write (`config.yaml`, `format.json`, `profiles/`, `eval/`,
    /// `audit/`) with the blob in HEAD's tree at the same path (`git_blob_id` of the file bytes
    /// against the tree entry id). Lists are sorted paths relative to the project directory.
    /// Writes nothing.
    pub fn git_state(&self) -> Result<GitState, StoreError> {
        let git = |e: &dyn std::fmt::Display| StoreError::Git(e.to_string());
        if !self.kindex_root.join(".git").exists() {
            return Ok(GitState::NotARepo);
        }
        let repo = gix::open(self.kindex_root.as_std_path()).map_err(|e| git(&e))?;
        let Ok(commit) = repo.head_commit() else {
            return Ok(GitState::NoCommits);
        };
        let project = self.root.file_name().unwrap_or_default();
        let tree = commit.tree().map_err(|e| git(&e))?;
        let mut in_head: BTreeMap<String, String> = BTreeMap::new();
        if let Some(entry) = tree
            .lookup_entry_by_path(format!("projects/{project}"))
            .map_err(|e| git(&e))?
            .filter(|e| e.mode().is_tree())
        {
            let sub = entry
                .object()
                .map_err(|e| git(&e))?
                .try_into_tree()
                .map_err(|e| git(&e))?;
            collect_tree(&repo, &sub, "", &mut in_head)?;
        }
        let mut on_disk = Vec::new();
        for name in HUMAN_FILES {
            let p = self.root.join(name);
            if p.is_dir() {
                walk_files(&p, &format!("{name}/"), &mut on_disk)?;
            } else if p.is_file() {
                on_disk.push((*name).to_string());
            }
        }
        on_disk.sort();
        let (mut modified, mut added) = (Vec::new(), Vec::new());
        for rel in on_disk {
            let file = self.root.join(&rel);
            let bytes = std::fs::read(&file).map_err(|e| StoreError::io(&file, e))?;
            match in_head.remove(&rel) {
                Some(id) if id == crate::blob::git_blob_id(&bytes) => {}
                Some(_) => modified.push(rel),
                None => added.push(rel),
            }
        }
        // What is left in the map has no file.
        let deleted: Vec<String> = in_head.into_keys().collect();
        Ok(GitState::Repo {
            head: commit.id().to_string(),
            modified,
            added,
            deleted,
        })
    }
}

/// The files a person writes in a project: a file name, or a folder whose files all count.
const HUMAN_FILES: &[&str] = &["config.yaml", "format.json", "profiles", "eval", "audit"];

/// Collects `path -> blob id` of the human-written files below a project's tree in HEAD.
fn collect_tree(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
    prefix: &str,
    out: &mut BTreeMap<String, String>,
) -> Result<(), StoreError> {
    let git = |e: &dyn std::fmt::Display| StoreError::Git(e.to_string());
    for entry in tree.iter() {
        let entry = entry.map_err(|e| git(&e))?;
        let name = entry.filename().to_string();
        if prefix.is_empty() && !HUMAN_FILES.contains(&name.as_str()) {
            continue;
        }
        let rel = format!("{prefix}{name}");
        if entry.mode().is_tree() {
            let sub = repo.find_tree(entry.oid()).map_err(|e| git(&e))?;
            collect_tree(repo, &sub, &format!("{rel}/"), out)?;
        } else if entry.mode().is_blob() {
            out.insert(rel, entry.oid().to_string());
        }
    }
    Ok(())
}

/// Every file below `dir`, as `prefix` plus its relative path.
fn walk_files(dir: &Utf8Path, prefix: &str, out: &mut Vec<String>) -> Result<(), StoreError> {
    for e in std::fs::read_dir(dir).map_err(|e| StoreError::io(dir, e))? {
        let e = e.map_err(|e| StoreError::io(dir, e))?;
        let Some(name) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let full = dir.join(&name);
        if full.is_dir() {
            walk_files(&full, &format!("{prefix}{name}/"), out)?;
        } else {
            out.push(format!("{prefix}{name}"));
        }
    }
    Ok(())
}
