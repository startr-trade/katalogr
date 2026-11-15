//! A working file's bytes in their repository form (FEAT-141).
//!
//! Git decides that a file is unchanged by converting the working file "to git" first: line
//! endings per `core.autocrlf`, `core.eol` and the `text`, `eol` and `crlf` attributes (with
//! `text=auto` binary detection), plus `ident` and `working-tree-encoding`. [`RepoForm`] runs
//! that same conversion with gix's filter pipeline, so every "is this file the same as its blob"
//! comparison and the position's dirty hash see what `git add` would store.
//!
//! Known limit: driver filters (`filter=<name>` with `filter.<name>.clean` or `.process`, such
//! as LFS) are never run. A path with such a filter is converted as if it had none, so its raw
//! bytes are taken as they are. No external process is ever started.
//!
//! On a checkout with no attributes and no `core.autocrlf`/`core.eol`, the conversion is the
//! identity, so blob ids and hashes are those of the raw bytes.

use crate::blob::git_blob_id;
use gix::filter::plumbing::pipeline::CrlfRoundTripCheck;
use gix::filter::plumbing::pipeline::convert::ToGitOutcome;
use std::borrow::Cow;

struct Inner {
    pipeline: gix::filter::plumbing::Pipeline,
    stack: gix::worktree::Stack,
    index: gix::worktree::IndexPersistedOrInMemory,
}

/// The to-git conversion of one repository. Built lazily on first use; when there is no
/// repository, no work tree, or the pipeline cannot be built, it is the identity.
///
/// Not thread-safe (a `gix::Repository` is not `Sync`): use one per thread.
pub struct RepoForm<'r> {
    repo: Option<&'r gix::Repository>,
    inner: Option<Option<Inner>>,
    used_index: bool,
}

impl<'r> RepoForm<'r> {
    /// The conversion for `repo`'s work tree, honouring its config and `.gitattributes`.
    pub fn new(repo: &'r gix::Repository) -> Self {
        RepoForm {
            repo: Some(repo),
            inner: None,
            used_index: false,
        }
    }

    /// No conversion at all: for a directory that is not a git checkout.
    pub fn identity() -> Self {
        RepoForm {
            repo: None,
            inner: Some(None),
            used_index: false,
        }
    }

    fn inner(&mut self) -> Option<(&'r gix::Repository, &mut Inner)> {
        let repo = self.repo?;
        let inner = self.inner.get_or_insert_with(|| build(repo));
        inner.as_mut().map(|i| (repo, i))
    }

    /// The bytes `git add` would store for the regular file at `rela` (repo-relative,
    /// '/'-separated) whose working-tree content is `bytes`. Borrowed when nothing changes; the
    /// raw bytes when the conversion fails. Never call it for a symbolic link's target.
    pub fn convert<'a>(&mut self, rela: &str, bytes: &'a [u8]) -> Cow<'a, [u8]> {
        let (form, used_index) = self.convert_noting_index(rela, bytes);
        self.used_index |= used_index;
        form
    }

    /// [`RepoForm::convert`], and whether this conversion consulted the index.
    fn convert_noting_index<'a>(&mut self, rela: &str, bytes: &'a [u8]) -> (Cow<'a, [u8]>, bool) {
        let Some((repo, inner)) = self.inner() else {
            return (Cow::Borrowed(bytes), false);
        };
        let path = std::path::Path::new(rela);
        let Ok(entry) = inner.stack.at_path(path, None, &repo.objects) else {
            return (Cow::Borrowed(bytes), false);
        };
        let index = &inner.index;
        let mut used_index = false;
        let out = inner.pipeline.convert_to_git(
            bytes,
            path,
            &mut |_, attrs| {
                entry.matching_attributes(attrs);
            },
            &mut |buf| {
                // `text=auto`: a file whose index blob already has CRLF is not normalised.
                used_index = true;
                let Some(e) = index.entry_by_path(rela.into()) else {
                    return Ok(None);
                };
                use gix::objs::Find;
                Ok(repo
                    .objects
                    .try_find(&e.id, buf)
                    .ok()
                    .flatten()
                    .filter(|o| o.kind == gix::objs::Kind::Blob)
                    .map(|_| ()))
            },
        );
        let form = match out {
            Ok(ToGitOutcome::Buffer(b)) => Cow::Owned(b.to_vec()),
            // `Unchanged`, a driver process (never configured here), or a failure: raw bytes.
            _ => Cow::Borrowed(bytes),
        };
        (form, used_index)
    }

    /// Whether any conversion so far consulted the index. Git's `text=auto` rule (also what
    /// `core.autocrlf` applies to files with no `text` attribute) keeps CRLF in a file whose index
    /// blob already has CRLF, so such a file's repository form depends on the index, and changes
    /// when only the index does (`git add`). It is consulted only for a text file with CRLF under
    /// an automatic text decision; never for an LF file or with no conversion configured.
    pub fn used_index(&self) -> bool {
        self.used_index
    }

    /// The git blob id of [`RepoForm::convert`]'s output.
    pub fn blob_id(&mut self, rela: &str, bytes: &[u8]) -> String {
        git_blob_id(&self.convert(rela, bytes))
    }
}

fn build(repo: &gix::Repository) -> Option<Inner> {
    repo.workdir()?;
    let (pipeline, index) = repo.filter_pipeline(None).ok()?;
    let (mut pipeline, stack) = pipeline.into_parts();
    let options = pipeline.options_mut();
    // Driver filters (LFS and the like) are out of scope: no external process is started.
    options.drivers.clear();
    // `core.safecrlf` only warns or refuses an add; a comparison must not fail on it.
    options.crlf_roundtrip_check = CrlfRoundTripCheck::Skip;
    Some(Inner {
        pipeline,
        stack,
        index,
    })
}

/// What the conversion depends on beyond the work tree's `.gitattributes` files:
/// `core.autocrlf`, `core.eol`, and the size and mtime of `info/attributes` and of
/// `core.attributesFile` (or the XDG default). A cache of converted ids is valid only while this
/// stays the same.
pub fn settings_key(repo: &gix::Repository) -> String {
    let config = repo.config_snapshot();
    let mut key = String::new();
    for name in ["core.autocrlf", "core.eol"] {
        let v = config
            .string(name)
            .map(|v| v.to_string())
            .unwrap_or_default();
        key.push_str(&format!("{name}={v}\n"));
    }
    let mut files = vec![repo.common_dir().join("info/attributes")];
    match config.trusted_path("core.attributesFile") {
        Ok(Some(p)) => files.push(p),
        _ => {
            let base = std::env::var_os("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
                });
            if let Some(b) = base {
                files.push(b.join("git/attributes"));
            }
        }
    }
    for f in files {
        let stat = std::fs::metadata(&f).ok().map(|m| {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            (m.len(), mtime)
        });
        key.push_str(&format!("{}={stat:?}\n", f.display()));
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn git(root: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .expect("run git");
        assert!(st.success(), "git {args:?}");
    }

    fn repo(config: &[(&str, &str)], attributes: Option<&str>) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        for (k, v) in config {
            git(d.path(), &["config", k, v]);
        }
        if let Some(a) = attributes {
            std::fs::write(d.path().join(".gitattributes"), a).unwrap();
        }
        d
    }

    fn convert(d: &tempfile::TempDir, rela: &str, bytes: &[u8]) -> Vec<u8> {
        let git = gix::open(d.path()).unwrap();
        RepoForm::new(&git).convert(rela, bytes).into_owned()
    }

    /// FEAT-141. With no conversion configured the bytes are returned as they are.
    #[test]
    fn identity_without_conversion() {
        let d = repo(&[("core.autocrlf", "false")], None);
        assert_eq!(convert(&d, "a.txt", b"a\r\nb\r\n"), b"a\r\nb\r\n");
        assert_eq!(
            RepoForm::identity().convert("a.txt", b"a\r\n"),
            &b"a\r\n"[..]
        );
    }

    /// FEAT-141. `core.autocrlf=true` and `* text=auto eol=crlf` turn CRLF into LF for text,
    /// and leave a binary (a NUL byte) alone.
    #[test]
    fn crlf_to_lf_for_text_only() {
        let auto = repo(&[("core.autocrlf", "true")], None);
        let attrs = repo(
            &[("core.autocrlf", "false")],
            Some("* text=auto eol=crlf\n"),
        );
        for d in [&auto, &attrs] {
            assert_eq!(convert(d, "a.txt", b"a\r\nb\r\n"), b"a\nb\n");
            assert_eq!(convert(d, "x/bin.dat", b"\0a\r\n"), b"\0a\r\n");
        }
        // `-text` keeps CRLF even under `core.autocrlf=true`.
        let off = repo(&[("core.autocrlf", "true")], Some("*.bat -text\n"));
        assert_eq!(convert(&off, "run.bat", b"a\r\n"), b"a\r\n");
        assert_eq!(convert(&off, "run.txt", b"a\r\n"), b"a\n");
    }

    /// FEAT-141. The index is consulted only for a CRLF text file under an automatic text
    /// decision: never for an LF file, a binary, or with no conversion configured.
    #[test]
    fn index_consulted_only_for_auto_crlf_text() {
        let used = |d: &tempfile::TempDir, rela: &str, bytes: &[u8]| {
            let git = gix::open(d.path()).unwrap();
            let mut form = RepoForm::new(&git);
            form.convert(rela, bytes);
            form.used_index()
        };
        let auto = repo(&[("core.autocrlf", "true")], None);
        assert!(used(&auto, "a.txt", b"a\r\n"));
        assert!(!used(&auto, "a.txt", b"a\n"));
        assert!(!used(&auto, "bin.dat", b"\0a\r\n"));
        let off = repo(&[("core.autocrlf", "false")], None);
        assert!(!used(&off, "a.txt", b"a\r\n"));
        // An explicit `text` attribute converts whatever the index holds.
        let text = repo(&[("core.autocrlf", "true")], Some("* text\n"));
        assert!(!used(&text, "a.txt", b"a\r\n"));
        assert!(!RepoForm::identity().used_index());
    }

    /// FEAT-141. A driver filter is never run: its path is converted as if it had none.
    #[test]
    fn driver_filters_are_not_run() {
        let d = repo(
            &[
                ("core.autocrlf", "false"),
                ("filter.up.clean", "tr a-z A-Z"),
                ("filter.up.required", "true"),
            ],
            Some("*.up filter=up\n"),
        );
        assert_eq!(convert(&d, "a.up", b"abc\n"), b"abc\n");
    }
}
