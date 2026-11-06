//! Synthetic workspaces for the FEAT-020 tests: kindexes with projects whose repos hold `.tx`
//! files. Names are generated; no scanned repository's names, paths or code appear.
#![allow(dead_code)]

use katalogr_dist::{Katalogr, Options, Overrides, kindex_init, project_init};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

pub struct World {
    pub tmp: tempfile::TempDir,
    pub root: PathBuf,
}

/// A repo to build: its name and its `.tx` files as `(path, text)`.
pub type RepoSpec<'a> = (&'a str, &'a [(&'a str, &'a str)]);

impl World {
    pub fn new() -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        World { tmp, root }
    }

    /// A new kindex folder named `name`.
    pub fn kindex(&self, name: &str) -> PathBuf {
        let kx = self.root.join(name);
        kindex_init(&kx).unwrap();
        kx
    }

    /// Where the repo `repo` of `project` in `kx` is checked out.
    pub fn repo_dir(&self, kx: &Path, project: &str, repo: &str) -> PathBuf {
        self.root.join(format!(
            "{}-{project}-{repo}",
            kx.file_name().unwrap().to_string_lossy()
        ))
    }

    /// Creates the project with its repos (each a git repo with one commit), then indexes it.
    pub fn project(&self, kx: &Path, project: &str, repos: &[RepoSpec<'_>]) {
        let mut roots = Vec::new();
        for (repo, files) in repos {
            let dir = self.repo_dir(kx, project, repo);
            std::fs::create_dir_all(&dir).unwrap();
            git(&dir, &["init", "-q", "-b", "main"]);
            self.write(&dir, files);
            git(&dir, &["add", "."]);
            git(&dir, &["commit", "-q", "-m", "base"]);
            roots.push((repo.to_string(), dir));
        }
        project_init(kx, project, roots, false).unwrap();
        self.index(kx, project);
    }

    pub fn write(&self, dir: &Path, files: &[(&str, &str)]) {
        for (path, text) in files {
            let p = dir.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
    }

    /// Replaces the files of a repo and commits them.
    pub fn commit(&self, kx: &Path, project: &str, repo: &str, files: &[(&str, &str)]) {
        let dir = self.repo_dir(kx, project, repo);
        self.write(&dir, files);
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "edit"]);
    }

    pub fn index(&self, kx: &Path, project: &str) {
        open(kx, project)
            .index(None, true, None, &Options::default())
            .unwrap();
    }
}

pub fn open(kx: &Path, project: &str) -> Katalogr {
    Katalogr::resolve(
        kx,
        &Overrides {
            kindex: Some(kx.to_string_lossy().into_owned()),
            project: Some(project.to_string()),
            ..Overrides::default()
        },
    )
    .unwrap()
}

/// SHA-256 of every file under `dir`, by relative path.
pub fn hash_tree(dir: &Path) -> BTreeMap<String, String> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(base, &p, out);
            } else if let Ok(bytes) = std::fs::read(&p) {
                let h = Sha256::digest(&bytes);
                out.insert(
                    p.strip_prefix(base).unwrap().to_string_lossy().into_owned(),
                    h.iter().map(|b| format!("{b:02x}")).collect(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// A provider of a contract: one `.tx` file.
pub fn provider(contract: &str) -> String {
    format!("sym fn serve\nbind serve provides {contract}\n")
}

/// A consumer of a contract.
pub fn consumer(contract: &str) -> String {
    format!("sym fn call\nbind call consumes {contract}\n")
}
