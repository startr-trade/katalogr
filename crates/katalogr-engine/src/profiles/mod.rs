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
use katalogr_model::{Fragment, KeyNormaliser};
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
    /// The key normalisers rules may name (ADR-0020).
    norms: Norms,
    /// `(project file, built-in file)` for each built-in a project file replaced by name.
    overridden: Vec<(String, String)>,
}

/// The registered key normalisers, in registration order.
#[derive(Clone, Default)]
struct Norms(Vec<&'static dyn KeyNormaliser>);

impl std::fmt::Debug for Norms {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|n| n.id()))
            .finish()
    }
}

impl Norms {
    fn find(&self, id: &str) -> Option<&'static dyn KeyNormaliser> {
        self.0.iter().find(|n| n.id() == id).copied()
    }
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

/// Refuse a rule that names a normaliser no registered extension provides.
fn check_normalisers(
    file: &str,
    text: &str,
    doc: &ProfileDoc,
    norms: &Norms,
) -> Result<(), ProfileError> {
    for r in &doc.rules {
        let Some(n) = r.emit.bind.as_ref().and_then(|b| b.normalise.as_deref()) else {
            continue;
        };
        if norms.find(n).is_some() {
            continue;
        }
        let known: Vec<&str> = norms.0.iter().map(|n| n.id()).collect();
        return Err(ProfileError {
            file: file.to_string(),
            line: rule_key_line(text, Some(&r.id), "normalise"),
            column: None,
            message: format!(
                "rule {:?}: unknown normaliser {:?} (known: {})",
                r.id,
                n,
                known.join(", ")
            ),
        });
    }
    Ok(())
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
    if message == format::NEEDS_SELECT {
        return key_line("select");
    }
    if let Some(key) = format::V2_RULE_KEYS
        .iter()
        .find(|k| message == format::needs_v2(k))
    {
        return rule_key_line(text, None, key);
    }
    for (needs, marker) in [
        (format::NEEDS_FACT, "${fact:"),
        (format::NEEDS_SCOPE, "${scope:"),
    ] {
        if message == needs {
            return text.lines().position(|l| l.contains(marker)).map(|i| i + 1);
        }
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

/// The 1-based line of the first `<key>:` at or after the line of the rule `rule` (the first
/// rule when None), where the key follows the start of the line, whitespace, `{` or `,`.
fn rule_key_line(text: &str, rule: Option<&str>, key: &str) -> Option<usize> {
    let is_id = |l: &str| {
        let l = l.trim_start().trim_start_matches("- ").trim_start();
        l.strip_prefix("id:").is_some_and(|v| {
            let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
            rule.is_none_or(|r| v == r)
        })
    };
    let needle = format!("{key}:");
    let start = text.lines().position(is_id)?;
    text.lines()
        .enumerate()
        .skip(start)
        .find(|(_, l)| {
            if l.trim_start().starts_with('#') {
                return false;
            }
            l.match_indices(&needle).any(|(i, _)| {
                i == 0
                    || l[..i]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_whitespace() || c == '{' || c == ',')
            })
        })
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
        ProfileSet::load_full(project_dir, builtins, previous, &[])
    }

    /// As [`ProfileSet::load_keeping`], also validating every `bind.normalise` against
    /// `normalisers` (an unknown id refuses the file at the line of its `normalise:` key),
    /// dropping a built-in whose `profile` name equals a loaded project file's, and keeping the
    /// normalisers for [`ProfileSet::apply`] and [`ProfileSet::hash_for`].
    pub fn load_full(
        project_dir: &Utf8Path,
        builtins: &[(&str, &str)],
        previous: &BTreeMap<String, String>,
        normalisers: &[&'static dyn KeyNormaliser],
    ) -> LoadReport {
        let mut report = LoadReport::default();
        report.set.norms = Norms(normalisers.to_vec());
        let norms = report.set.norms.clone();
        // Parse and validate one file against the known normalisers.
        let read = |file: &str, text: &str| -> Result<ProfileDoc, ProfileError> {
            let doc = parse(file, text)?;
            check_normalisers(file, text, &doc, &norms)?;
            Ok(doc)
        };
        for (name, text) in builtins {
            let file = format!("builtin:{name}");
            match read(&file, text) {
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
                Ok(text) => read(&file, &text).map(|doc| (text, doc)),
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
                        .and_then(|old| read(&file, old).ok().map(|doc| (old.clone(), doc)))
                }
            };
            if let Some((source, doc)) = chosen {
                report.good.insert(file.clone(), source.clone());
                report.set.loaded.push(Loaded { file, source, doc });
            }
        }
        // A project file replaces the built-in of the same profile name (ADR-0020).
        let set = &mut report.set;
        let mut overridden = Vec::new();
        let project: Vec<(String, String)> = set
            .loaded
            .iter()
            .filter(|l| !l.file.starts_with("builtin:"))
            .map(|l| (l.file.clone(), l.doc.profile.clone()))
            .collect();
        set.loaded.retain(|l| {
            if !l.file.starts_with("builtin:") {
                return true;
            }
            match project.iter().find(|(_, p)| *p == l.doc.profile) {
                Some((pf, _)) => {
                    overridden.push((pf.clone(), l.file.clone()));
                    false
                }
                None => true,
            }
        });
        overridden.sort();
        set.overridden = overridden;
        report
    }

    /// `(project file, built-in file)` for each built-in a project file replaced by name.
    pub fn overridden(&self) -> &[(String, String)] {
        &self.overridden
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
        let scope = self
            .rules()
            .any(|(_, r)| r.uses_scope())
            .then(|| eval::ScopeIndex::build(f));
        let mut cx = eval::Cx::new(scope, f, self.rules().any(|(_, r)| r.has_holder()));
        for (profile, rule) in self.rules() {
            eval::apply_rule(profile, rule, f, &mut cx, &self.norms.0);
        }
    }

    /// The extension id named by the first `select` entry (in application order) whose glob
    /// matches `path`; None when no entry matches.
    pub fn route(&self, path: &katalogr_model::RelPath) -> Option<&str> {
        self.loaded
            .iter()
            .flat_map(|l| l.doc.select.iter())
            .find(|s| s.path.matcher.is_match(path.as_str()))
            .map(|s| s.extension.as_str())
    }

    /// Every select entry, in application order: (profile, extension, glob source).
    pub fn selects(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.loaded.iter().flat_map(|l| {
            l.doc.select.iter().map(|s| {
                (
                    l.doc.profile.as_str(),
                    s.extension.as_str(),
                    s.path.source.as_str(),
                )
            })
        })
    }

    /// A hash of the files that hold a rule that can apply to `extension` (a rule that names no
    /// extension applies to all) or a `select` entry that names it: their names and bytes, in application order. `None` when no
    /// such rule exists, so a project without profiles keeps its revisions.
    pub fn hash_for(&self, extension: &str) -> Option<[u8; 32]> {
        let mut h = Sha256::new();
        let mut any = false;
        for l in &self.loaded {
            let hit = l.doc.select.iter().any(|s| s.extension == extension)
                || l.doc
                    .rules
                    .iter()
                    .any(|r| r.when.extension.as_deref().is_none_or(|e| e == extension));
            if hit {
                any = true;
                h.update((l.file.len() as u64).to_le_bytes());
                h.update(l.file.as_bytes());
                h.update((l.source.len() as u64).to_le_bytes());
                h.update(l.source.as_bytes());
                // The normalisers a rule of this extension names take part, so a change in
                // their output rescans (ADR-0020).
                for r in &l.doc.rules {
                    if r.when.extension.as_deref().is_some_and(|e| e != extension) {
                        continue;
                    }
                    let Some(n) = r
                        .emit
                        .bind
                        .as_ref()
                        .and_then(|b| b.normalise.as_deref())
                        .and_then(|id| self.norms.find(id))
                    else {
                        continue;
                    };
                    h.update((n.id().len() as u64).to_le_bytes());
                    h.update(n.id().as_bytes());
                    h.update(n.version().to_le_bytes());
                }
            }
        }
        any.then(|| h.finalize().into())
    }
}
