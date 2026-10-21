//! The corpus manifest (`corpora.yaml`): what to measure, pinned to commits.

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::error::{BenchError, Result};

pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub corpora: Vec<Corpus>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, clap::ValueEnum,
)]
pub enum Tier {
    S,
    M,
    L,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Corpus {
    /// `[a-z0-9-]{1,40}`, unique; also the katalogr project name.
    pub name: String,
    pub tier: Tier,
    /// Free text: "rust-xl", "sql", "polyglot-wasm", "go" ...
    pub category: String,
    /// Tags for the report only.
    pub languages: Vec<String>,
    /// 1..=8; one katalogr project holds them all.
    pub repos: Vec<RepoPin>,
    /// Repository used for replay; must be in `repos`.
    pub primary: String,
    /// Next N real commits to replay (0 = none).
    #[serde(default)]
    pub replay: u32,
    /// File name under `<workspace>/profiles/`.
    #[serde(default)]
    pub profile: Option<String>,
    pub approx: Approx,
    #[serde(default)]
    pub queries: QueryOverrides,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoPin {
    pub name: String,
    pub url: String,
    pub commit: String,
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approx {
    pub files: u64,
    pub tree_mib: u64,
    pub disk_gib: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryOverrides {
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default)]
    pub terms: Vec<Vec<String>>,
}

impl Tier {
    pub fn label(&self) -> &'static str {
        match self {
            Tier::S => "S",
            Tier::M => "M",
            Tier::L => "L",
        }
    }
}

/// Empty lists select everything.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub tiers: Vec<Tier>,
    pub names: Vec<String>,
}

fn bad(corpus: &str, field: &str, problem: impl std::fmt::Display) -> BenchError {
    BenchError::Manifest(format!("corpus {corpus}: {field}: {problem}"))
}

fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl Manifest {
    /// Read and validate. Errors name the corpus and field.
    pub fn load(path: &Utf8Path) -> Result<Manifest> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| BenchError::Manifest(format!("{path}: {e}")))?;
        Manifest::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Manifest> {
        let m: Manifest = serde_yaml_ng::from_str(text)
            .map_err(|e| BenchError::Manifest(format!("parse: {e}")))?;
        m.validate()?;
        Ok(m)
    }

    /// version == 1; names valid and unique; commit is 40 lowercase hex; depth >= 1; for the
    /// primary repo depth >= replay + 1; primary in repos; url starts with https:// or file://.
    pub fn validate(&self) -> Result<()> {
        if self.version != MANIFEST_VERSION {
            return Err(BenchError::Manifest(format!(
                "version {} is not supported (this harness reads version {MANIFEST_VERSION})",
                self.version
            )));
        }
        let mut seen = BTreeSet::new();
        for c in &self.corpora {
            if !valid_name(&c.name) {
                return Err(bad(&c.name, "name", "must match [a-z0-9-]{1,40}"));
            }
            if !seen.insert(c.name.as_str()) {
                return Err(bad(&c.name, "name", "duplicate corpus name"));
            }
            if c.repos.is_empty() || c.repos.len() > 8 {
                return Err(bad(&c.name, "repos", "needs 1 to 8 repositories"));
            }
            let mut repo_names = BTreeSet::new();
            for r in &c.repos {
                if !valid_name(&r.name) {
                    return Err(bad(
                        &c.name,
                        "repos.name",
                        format!("`{}` must match [a-z0-9-]{{1,40}}", r.name),
                    ));
                }
                if !repo_names.insert(r.name.as_str()) {
                    return Err(bad(
                        &c.name,
                        "repos.name",
                        format!("duplicate repository `{}`", r.name),
                    ));
                }
                let hex = r.commit.len() == 40
                    && r.commit
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
                if !hex {
                    return Err(bad(
                        &c.name,
                        "repos.commit",
                        format!("`{}` is not a pinned 40-hex commit", r.name),
                    ));
                }
                if r.depth < 1 {
                    return Err(bad(
                        &c.name,
                        "repos.depth",
                        format!("`{}` must be at least 1", r.name),
                    ));
                }
                if !(r.url.starts_with("https://") || r.url.starts_with("file://")) {
                    return Err(bad(
                        &c.name,
                        "repos.url",
                        format!("`{}` must start with https:// or file://", r.name),
                    ));
                }
            }
            let Some(primary) = c.repos.iter().find(|r| r.name == c.primary) else {
                return Err(bad(
                    &c.name,
                    "primary",
                    format!("`{}` is not in repos", c.primary),
                ));
            };
            if primary.depth < c.replay + 1 {
                return Err(bad(
                    &c.name,
                    "repos.depth",
                    format!(
                        "`{}` has depth {} but replay {} needs at least {}",
                        primary.name,
                        primary.depth,
                        c.replay,
                        c.replay + 1
                    ),
                ));
            }
            if !(c.approx.disk_gib.is_finite() && c.approx.disk_gib >= 0.0) {
                return Err(bad(
                    &c.name,
                    "approx.disk_gib",
                    "must be a non-negative number",
                ));
            }
        }
        Ok(())
    }

    /// Corpora matching the selection, in manifest order.
    pub fn select(&self, sel: &Selection) -> Vec<&Corpus> {
        self.corpora
            .iter()
            .filter(|c| sel.tiers.is_empty() || sel.tiers.contains(&c.tier))
            .filter(|c| sel.names.is_empty() || sel.names.contains(&c.name))
            .collect()
    }
}

#[cfg(test)]
const FULL: &str = r#"
version: 1
corpora:
  - name: docs
    tier: S
    category: markdown
    languages: [md]
    primary: main
    replay: 2
    repos:
      - { name: main, url: "https://example.invalid/docs.git", commit: 0123456789abcdef0123456789abcdef01234567, depth: 3 }
    approx: { files: 100, tree_mib: 5, disk_gib: 0.1 }
  - name: polyglot
    tier: M
    category: polyglot-wasm
    languages: [rust, ts]
    primary: core
    replay: 20
    profile: polyglot.yaml
    repos:
      - { name: core, url: "https://example.invalid/core.git", commit: 0123456789abcdef0123456789abcdef01234567, depth: 21 }
      - { name: site, url: "https://example.invalid/site.git", commit: fedcba9876543210fedcba9876543210fedcba98, depth: 1 }
    approx: { files: 34333, tree_mib: 253, disk_gib: 3.0 }
    queries: { targets: [alpha], terms: [[buffer, size]] }
"#;

#[cfg(test)]
#[test]
fn parse_full_manifest() {
    let m = Manifest::parse(FULL).unwrap();
    assert_eq!(m.corpora.len(), 2);
    assert_eq!(m.corpora[1].repos[1].name, "site");
    assert_eq!(m.corpora[1].tier, Tier::M);
    assert_eq!(
        m.corpora[1].queries.terms,
        vec![vec!["buffer".to_string(), "size".to_string()]]
    );
    // Round trip.
    let again = Manifest::parse(&serde_yaml_ng::to_string(&m).unwrap()).unwrap();
    assert_eq!(m, again);
    let only_m = m.select(&Selection {
        tiers: vec![Tier::M],
        names: vec![],
    });
    assert_eq!(only_m.len(), 1);
    assert_eq!(only_m[0].name, "polyglot");
    assert_eq!(m.select(&Selection::default()).len(), 2);
}

#[cfg(test)]
#[test]
fn rejects_unpinned_or_bad_depth() {
    let err = |from: &str, to: &str| -> String {
        let text = FULL.replacen(from, to, 1);
        Manifest::parse(&text).unwrap_err().to_string()
    };
    let e = err("0123456789abcdef0123456789abcdef01234567", "0123456");
    assert!(e.contains("corpus docs: repos.commit"), "{e}");
    let e = err("depth: 21", "depth: 5");
    assert!(e.contains("corpus polyglot: repos.depth"), "{e}");
    let e = err("name: polyglot", "name: docs");
    assert!(e.contains("duplicate"), "{e}");
    let e = err("primary: core", "primary: elsewhere");
    assert!(e.contains("corpus polyglot: primary"), "{e}");
    let e = err("version: 1", "version: 2");
    assert!(e.contains("version 2"), "{e}");
    let e = err(
        "https://example.invalid/docs.git",
        "http://example.invalid/docs.git",
    );
    assert!(e.contains("repos.url"), "{e}");
}
