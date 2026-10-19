//! Declarative profiles (FEAT-008, ADR-0002, ADR-0017): match/emit rules that add bindings,
//! references and facts to a fragment after extraction and before it is encoded as a shard.
//!
//! Built-in profiles (embedded by the binary) run first, then the project's profiles from
//! `<project dir>/profiles/*.yaml`, in file-name order. A refused file is reported with its
//! file, line and reason, and the last version of that file that was valid stays in use.

mod eval;
pub mod format;

use camino::Utf8Path;
use format::{ProfileDoc, Rule};
use katalogr_model::Fragment;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub use eval::origin;

/// Folder of a project directory that holds its profiles.
pub const PROFILES_DIR: &str = "profiles";

/// Cache `meta` key: JSON map of the last valid text of each project profile file.
pub(crate) const META_GOOD: &str = "profiles_good";
/// Cache `meta` key: JSON list of the [`ProfileError`]s of the latest load.
pub(crate) const META_ERRORS: &str = "profiles_errors";

/// The profile files refused by the latest load that ran against this cache (empty when none).
pub fn stored_errors(r: &impl katalogr_store::Reader) -> Vec<ProfileError> {
    r.meta(META_ERRORS)
        .ok()
        .flatten()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Why a profile file was refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileError {
    /// `profiles/<file>` for a project profile, `builtin:<file>` for an embedded one.
    pub file: String,
    /// 1-based, when the reason has a position in the file.
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub message: String,
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(l) => write!(f, "{}:{}: {}", self.file, l, self.message),
            None => write!(f, "{}: {}", self.file, self.message),
        }
    }
}

/// One loaded file.
#[derive(Clone, Debug)]
struct Loaded {
    file: String,
    source: String,
    doc: ProfileDoc,
}

/// The profiles in force, built-ins first.
#[derive(Clone, Debug, Default)]
pub struct ProfileSet {
    loaded: Vec<Loaded>,
}

/// What a load found.
#[derive(Debug, Default)]
pub struct LoadReport {
    pub set: ProfileSet,
    /// Files that were refused.
    pub errors: Vec<ProfileError>,
    /// The source of every project file in force (its current text, or the last valid text
    /// when the current one was refused), by `profiles/<file>`. Keep it and pass it back as
    /// `previous` on the next load.
    pub good: BTreeMap<String, String>,
}

/// Parse and validate one profile file.
pub fn parse(file: &str, text: &str) -> Result<ProfileDoc, ProfileError> {
    serde_yaml_ng::from_str::<ProfileDoc>(text).map_err(|e| {
        let loc = e.location();
        let mut message = e.to_string();
        if let Some(i) = message.rfind(" at line ") {
            message.truncate(i);
        }
        let line = loc
            .as_ref()
            .map(|l| l.line())
            .or_else(|| document_line(text, &message));
        ProfileError {
            file: file.to_string(),
            line,
            column: loc.as_ref().map(|l| l.column()),
            message,
        }
    })
}

/// The line of a document-level refusal, which the YAML reader does not position: the
/// `version:` or `profile:` key, or the second rule with a repeated id.
fn document_line(text: &str, message: &str) -> Option<usize> {
    let key_line = |key: &str| {
        text.lines()
            .position(|l| l.trim_start().starts_with(&format!("{key}:")))
            .map(|i| i + 1)
    };
    if message.starts_with("unsupported version") {
        return key_line("version");
    }
    if message.starts_with("profile name") {
        return key_line("profile");
    }
    let id = message
        .strip_prefix("duplicate rule id \"")?
        .strip_suffix('"')?;
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            let l = l.trim_start().trim_start_matches("- ").trim_start();
            l.strip_prefix("id:")
                .is_some_and(|v| v.trim().trim_matches(|c| c == '"' || c == '\'') == id)
        })
        .nth(1)
        .map(|(i, _)| i + 1)
}

impl ProfileSet {
    pub fn empty() -> ProfileSet {
        ProfileSet::default()
    }

    /// Load `builtins` (`(file name, text)`), then every `*.yaml` and `*.yml` file of
    /// `<project_dir>/profiles`, refusing the invalid ones.
    pub fn load(project_dir: &Utf8Path, builtins: &[(&str, &str)]) -> LoadReport {
        ProfileSet::load_keeping(project_dir, builtins, &BTreeMap::new())
    }

    /// As [`ProfileSet::load`]; a project file that is refused is replaced by its text in
    /// `previous` (the `good` map of an earlier load), when that parses.
    pub fn load_keeping(
        project_dir: &Utf8Path,
        builtins: &[(&str, &str)],
        previous: &BTreeMap<String, String>,
    ) -> LoadReport {
        let mut report = LoadReport::default();
        for (name, text) in builtins {
            let file = format!("builtin:{name}");
            match parse(&file, text) {
                Ok(doc) => report.set.loaded.push(Loaded {
                    file,
                    source: (*text).to_string(),
                    doc,
                }),
                Err(e) => report.errors.push(e),
            }
        }
        let dir = project_dir.join(PROFILES_DIR);
        let mut names: Vec<String> = match std::fs::read_dir(&dir) {
            Ok(rd) => rd
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.ends_with(".yaml") || n.ends_with(".yml"))
                .collect(),
            Err(_) => Vec::new(),
        };
        names.sort();
        for name in names {
            let file = format!("{PROFILES_DIR}/{name}");
            let parsed = match std::fs::read_to_string(dir.join(&name)) {
                Ok(text) => parse(&file, &text).map(|doc| (text, doc)),
                Err(e) => Err(ProfileError {
                    file: file.clone(),
                    line: None,
                    column: None,
                    message: format!("cannot be read: {e}"),
                }),
            };
            let chosen = match parsed {
                Ok(x) => Some(x),
                Err(e) => {
                    report.errors.push(e);
                    previous
                        .get(&file)
                        .and_then(|old| parse(&file, old).ok().map(|doc| (old.clone(), doc)))
                }
            };
            if let Some((source, doc)) = chosen {
                report.good.insert(file.clone(), source.clone());
                report.set.loaded.push(Loaded { file, source, doc });
            }
        }
        report
    }

    /// Profile names in application order.
    pub fn names(&self) -> Vec<&str> {
        self.loaded.iter().map(|l| l.doc.profile.as_str()).collect()
    }

    /// The `(profile, rule)` pairs in application order.
    pub fn rules(&self) -> impl Iterator<Item = (&str, &Rule)> {
        self.loaded
            .iter()
            .flat_map(|l| l.doc.rules.iter().map(|r| (l.doc.profile.as_str(), r)))
    }

    pub fn is_empty(&self) -> bool {
        self.loaded.iter().all(|l| l.doc.rules.is_empty())
    }

    /// Run every rule, in order, on `f`.
    pub fn apply(&self, f: &mut Fragment) {
        for (profile, rule) in self.rules() {
            eval::apply_rule(profile, rule, f);
        }
    }

    /// A hash of the files that hold a rule that can apply to `extension` (a rule that names no
    /// extension applies to all): their names and bytes, in application order. `None` when no
    /// such rule exists, so a project without profiles keeps its revisions.
    pub fn hash_for(&self, extension: &str) -> Option<[u8; 32]> {
        let mut h = Sha256::new();
        let mut any = false;
        for l in &self.loaded {
            let hit = l
                .doc
                .rules
                .iter()
                .any(|r| r.when.extension.as_deref().is_none_or(|e| e == extension));
            if hit {
                any = true;
                h.update((l.file.len() as u64).to_le_bytes());
                h.update(l.file.as_bytes());
                h.update((l.source.len() as u64).to_le_bytes());
                h.update(l.source.as_bytes());
            }
        }
        any.then(|| h.finalize().into())
    }
}
