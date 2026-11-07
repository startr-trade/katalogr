//! The diff report between two commits (FEAT-095, ADR-0040): a stored fact about two positions
//! under one set of extraction rules.
//!
//! The key (commits, facts version, rules) is read from git and the base alone, so a stored
//! report is found without opening either side's context. A miss builds both sides as commit
//! contexts, compares them, and stores the report under its view ID.

use crate::answers::{AnswerRequest, DiffQuery, first_difference};
use crate::contexts::{ContextChoice, base_commit_key};
use crate::diff_render::{to_html, to_markdown};
use crate::{Consistency, CoverageOptions, DistError, Katalogr, Options, Outcome, Reply, Report};
use katalogr_analytics::insight_shift::{InsightShifts, InsightSide, insight_shifts};
use katalogr_analytics::semantic_diff::{DiffOptions, SemanticDiff, semantic_diff};
use katalogr_analytics::{
    CommunityOptions, CommunityReport, CoverageReport, CycleOptions, CycleReport, GraphOptions,
    HubOptions, HubReport, Level,
};
use katalogr_engine::base::resolve_commit;
use katalogr_engine::history::HISTORY_PREFIX;
use katalogr_query::{PositionRef, ViewQuery};
use katalogr_store::{ContextId, ContextManifest, Reader};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::Instant;

/// The report's own format. A breaking change raises it, which changes every view ID.
pub const REPORT_FORMAT: u32 = 1;

/// Seconds, when the user names no `--timeout`. Proposed: above the 321 s worst cold index in the
/// FEAT-094 spike, at load 2 to 13, with room for a seed.
pub const DIFF_TIMEOUT_DEFAULT: u64 = 600;

/// The format a stored entry holds.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DiffRender {
    Json,
    Markdown,
    Html,
}

impl DiffRender {
    pub(crate) fn name(self) -> &'static str {
        match self {
            DiffRender::Json => "json",
            DiffRender::Markdown => "markdown",
            DiffRender::Html => "html",
        }
    }

    pub(crate) fn from_name(s: &str) -> Option<DiffRender> {
        Some(match s {
            "json" => DiffRender::Json,
            "markdown" => DiffRender::Markdown,
            "html" => DiffRender::Html,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct DiffRequest {
    pub a: String,
    pub b: String,
    pub repo: Option<String>,
    pub limit: usize,
}

/// Commits, never the names the user typed, so two names of one commit give the same bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiffSide {
    pub commit: String,
    pub positions: Vec<PositionRef>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiffReport {
    pub report_format: u32,
    pub repo: String,
    pub a: DiffSide,
    pub b: DiffSide,
    pub facts_version: String,
    pub limit: u64,
    pub diff: SemanticDiff,
    pub insights: InsightShifts,
    /// Set when another repo's position differs between the sides: their rows may show as changes.
    pub other_repos_differ: Vec<String>,
}

/// A rendered page (Markdown or HTML) as stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffPage {
    pub text: String,
}

/// Hex SHA-256 over `config.yaml`, then every file under `profiles/`, sorted by relative path, each
/// as u32 big-endian length of path, path, u64 big-endian length of bytes, bytes. No absolute path.
pub(crate) fn project_rules_digest(p: &katalogr_store::ProjectDir) -> Result<String, DistError> {
    let root = p.project_root();
    let mut files: Vec<(String, std::path::PathBuf)> = Vec::new();
    let config = root.join("config.yaml");
    if config.is_file() {
        files.push(("config.yaml".to_string(), config.into_std_path_buf()));
    }
    let mut stack = vec![root.join("profiles").into_std_path_buf()];
    let mut found: Vec<(String, std::path::PathBuf)> = Vec::new();
    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(DistError::io(dir.display(), e)),
        };
        for entry in rd {
            let entry = entry.map_err(|e| DistError::io(dir.display(), e))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                let rel = path
                    .strip_prefix(root.as_std_path())
                    .map_err(|e| DistError::io(path.display(), e))?
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                found.push((rel, path));
            }
        }
    }
    found.sort();
    files.extend(found);
    let mut h = Sha256::new();
    for (rel, path) in files {
        let bytes = std::fs::read(&path).map_err(|e| DistError::io(path.display(), e))?;
        h.update((rel.len() as u32).to_be_bytes());
        h.update(rel.as_bytes());
        h.update((bytes.len() as u64).to_be_bytes());
        h.update(&bytes);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// What identifies a report: found from git and the base alone.
pub(crate) struct DiffKey {
    pub repo: String,
    pub a: Vec<PositionRef>,
    pub b: Vec<PositionRef>,
    pub facts: String,
    pub rules: String,
    pub storable: bool,
}

impl DiffKey {
    fn commit_of(&self, side: &[PositionRef]) -> String {
        side.iter()
            .find(|p| p.repo == self.repo)
            .and_then(|p| p.commit.clone())
            .unwrap_or_default()
    }
}

/// What a build learned beyond the report.
#[derive(Default)]
pub(crate) struct BuildNotes {
    /// False when the sides were not at the key's positions or facts: the report is returned, not stored.
    pub unstorable: bool,
    pub warnings: Vec<String>,
    pub watermark: u64,
}

struct OwnedInsights {
    hubs: HubReport,
    communities: CommunityReport,
    cycles: CycleReport,
    coverage: CoverageReport,
}

impl OwnedInsights {
    fn side(&self) -> InsightSide<'_> {
        InsightSide {
            hubs: &self.hubs,
            communities: &self.communities,
            cycles: &self.cycles,
            coverage: &self.coverage,
        }
    }
}

fn found<T>(r: Reply<T>, what: &str) -> Result<(T, Vec<String>), DistError> {
    match r.outcome {
        Outcome::Found(v) => Ok((v, r.report.warnings)),
        _ => Err(DistError::Refused {
            exit: 5,
            message: format!("no {what} could be read for a side of the report"),
        }),
    }
}

impl Katalogr {
    fn diff_key(&self, r: &DiffRequest, o: &Options) -> Result<DiffKey, DistError> {
        let repo = r
            .repo
            .clone()
            .or_else(|| self.repo.clone())
            .ok_or_else(|| {
                DistError::usage(
                    "not inside a marked repository: name the repo with --repo or run from a directory with a .katalogr marker",
                )
            })?;
        let root = self
            .roots
            .get(&repo)
            .ok_or_else(|| crate::unknown_root(&repo))?;
        let resolve = |rev: &str| {
            resolve_commit(root.as_std_path(), rev)
                .map_err(|e| DistError::usage(format!("{rev}: {e}")))
        };
        let (ca, cb) = (resolve(&r.a)?, resolve(&r.b)?);
        let deadline = Instant::now() + o.timeout()?;
        let dir = self.project.base();
        let engine = match self.reader_engine(dir.clone())? {
            Some(e) => e,
            None => self.engine_on(dir, deadline)?,
        };
        let snap = engine.cache().read()?;
        let facts = engine.facts_version(&snap)?;
        let mut storable = self.answers_enabled();
        let mut side = |commit: &str| -> Result<Vec<PositionRef>, DistError> {
            let mut out = Vec::new();
            for c in &self.project.config().repos {
                if c.name == repo {
                    out.push(PositionRef {
                        repo: repo.clone(),
                        commit: Some(commit.to_string()),
                        dirty: None,
                    });
                    continue;
                }
                let base = snap
                    .meta(&base_commit_key(&c.name))?
                    .and_then(|b| String::from_utf8(b).ok())
                    .filter(|s| !s.is_empty());
                if base.is_none() {
                    storable = false;
                }
                out.push(PositionRef {
                    repo: c.name.clone(),
                    commit: base,
                    dirty: None,
                });
            }
            out.sort();
            Ok(out)
        };
        let (a, b) = (side(&ca)?, side(&cb)?);
        drop(snap);
        Ok(DiffKey {
            repo,
            a,
            b,
            facts,
            rules: project_rules_digest(&self.project)?,
            storable,
        })
    }

    fn diff_query(key: &DiffKey, limit: u64, render: DiffRender) -> ViewQuery {
        crate::answers::StoredQuery::Diff(crate::answers::DiffQuery {
            repo: key.repo.clone(),
            a: key.a.clone(),
            b: key.b.clone(),
            limit,
            render,
        })
        .to_view_query()
    }

    /// One side's hubs, communities, cycles and coverage, each a stored answer for that commit.
    fn side_insights(&self, o: &Options) -> Result<(OwnedInsights, Vec<String>), DistError> {
        let s = self.insight_settings();
        let graph = |level, inferred| {
            let mut g = GraphOptions::new(level, inferred);
            g.exclude_kinds = s.exclude_kinds.clone();
            g
        };
        let o = Options {
            view: None,
            at: None,
            ..o.clone()
        };
        let (hubs, mut w) = found(
            self.hubs(
                &HubOptions {
                    graph: graph(Level::Symbol, true),
                    min_degree: s.hub_min_degree,
                    limit: 20,
                },
                &o,
            )?,
            "hubs",
        )?;
        let (communities, w2) = found(
            self.communities(
                &CommunityOptions {
                    graph: graph(Level::File, true),
                    seed: s.seed,
                    resolution: s.resolution,
                    min_size: s.community_min_size,
                    members: 5,
                    limit: 20,
                    of: None,
                },
                &o,
            )?,
            "communities",
        )?;
        let (cycles, w3) = found(
            self.cycles(
                &CycleOptions {
                    graph: graph(Level::File, false),
                    members: 10,
                    limit: 20,
                },
                &o,
            )?,
            "cycles",
        )?;
        let (coverage, w4) = found(
            self.coverage(
                &CoverageOptions {
                    repo: None,
                    suffix_top: 20,
                },
                &o,
            )?,
            "coverage",
        )?;
        w.extend(w2);
        w.extend(w3);
        w.extend(w4);
        Ok((
            OwnedInsights {
                hubs,
                communities,
                cycles,
                coverage,
            },
            w,
        ))
    }

    fn build_report(
        &self,
        key: &DiffKey,
        limit: u64,
        o: &Options,
        notes: &mut BuildNotes,
    ) -> Result<DiffReport, DistError> {
        let (commit_a, commit_b) = (key.commit_of(&key.a), key.commit_of(&key.b));
        let same = commit_a == commit_b;
        let (id_a, rep_a) = self.commit_side(&key.repo, &commit_a, None, o)?;
        let (id_b, rep_b) = if same {
            (id_a.clone(), rep_a.clone())
        } else {
            self.commit_side(&key.repo, &commit_b, Some(&id_a), o)?
        };
        notes.watermark = rep_b.watermark;
        notes.warnings.extend(rep_a.warnings.iter().cloned());
        notes.warnings.extend(rep_b.warnings.iter().cloned());
        let (side_a, side_b) = (
            self.with_context(ContextChoice::Named(id_a.clone())),
            self.with_context(ContextChoice::Named(id_b.clone())),
        );
        let deadline = Instant::now() + o.timeout()?;
        let limit_n = usize::try_from(limit).unwrap_or(usize::MAX);
        let (diff, facts_a, facts_b) = {
            let ea = side_a.engine_for(Consistency::StaleOk, deadline)?;
            let eb = if same {
                None
            } else {
                Some(side_b.engine_for(Consistency::StaleOk, deadline)?)
            };
            let (ra, rb) = (ea.cache().read()?, eb.as_ref().map(|e| e.cache().read()));
            let rb = rb.transpose()?;
            let facts_a = ea.facts_version(&ra)?;
            let facts_b = match (&eb, &rb) {
                (Some(e), Some(r)) => e.facts_version(r)?,
                _ => facts_a.clone(),
            };
            let opts = DiffOptions {
                limit: limit_n,
                exclude_path_prefixes: vec![HISTORY_PREFIX.to_string()],
            };
            let diff = match &rb {
                Some(rb) => semantic_diff(&ra, rb, &opts)?,
                None => semantic_diff(&ra, &ra, &opts)?,
            };
            (diff, facts_a, facts_b)
        };
        if facts_a != facts_b {
            return Err(DistError::Refused {
                exit: 5,
                message: "the two sides were extracted under different rules; run it again"
                    .to_string(),
            });
        }
        if facts_a != key.facts {
            notes.unstorable = true;
            notes
                .warnings
                .push("the sides were extracted under newer rules than the base; the report is not stored".to_string());
        }
        for (rep, want) in [(&rep_a, &key.a), (&rep_b, &key.b)] {
            if &rep.positions != want {
                notes.unstorable = true;
                notes.warnings.push(
                    "a side is not at the positions the report was keyed on; the report is not stored"
                        .to_string(),
                );
                break;
            }
        }
        let (ins_a, w_a) = side_a.side_insights(o)?;
        notes.warnings.extend(w_a);
        let mut diff = diff;
        let shifts = if same {
            insight_shifts(&ins_a.side(), &ins_a.side(), limit_n, &mut diff.partial)
        } else {
            let (ins_b, w_b) = side_b.side_insights(o)?;
            notes.warnings.extend(w_b);
            insight_shifts(&ins_a.side(), &ins_b.side(), limit_n, &mut diff.partial)
        };
        let other_repos_differ: Vec<String> = rep_a
            .positions
            .iter()
            .filter(|p| p.repo != key.repo)
            .filter(|p| {
                rep_b
                    .positions
                    .iter()
                    .find(|q| q.repo == p.repo)
                    .is_none_or(|q| q != *p)
            })
            .map(|p| p.repo.clone())
            .collect();
        notes.warnings.sort();
        notes.warnings.dedup();
        Ok(DiffReport {
            report_format: REPORT_FORMAT,
            repo: key.repo.clone(),
            a: DiffSide {
                commit: commit_a,
                positions: key.a.clone(),
            },
            b: DiffSide {
                commit: commit_b,
                positions: key.b.clone(),
            },
            facts_version: key.facts.clone(),
            limit,
            diff,
            insights: shifts,
            other_repos_differ,
        })
    }

    /// Through the store when it is on. Fills `Reply.partial` from the report.
    /// Errors: `DistError::Usage` for an unknown revision or a repo with no root;
    /// `DistError::Refused { exit: 5, .. }` for an undrained side or differing facts.
    pub fn diff_report(
        &self,
        r: &DiffRequest,
        o: &Options,
    ) -> Result<Reply<DiffReport>, DistError> {
        let key = self.diff_key(r, o)?;
        let limit = r.limit as u64;
        let q = Self::diff_query(&key, limit, DiffRender::Json);
        let req = AnswerRequest {
            view_id: katalogr_query::view_id(&key.b, &key.facts, &q),
            op: &q.op,
            args: q.args.clone(),
            inputs: BTreeMap::from([("rules".to_string(), key.rules.clone())]),
            storable: key.storable,
        };
        let mut notes = BuildNotes::default();
        let mut failure: Option<DistError> = None;
        let mut built = false;
        let mut store_warnings = Vec::new();
        let (out, pending) = self.read_through(
            &req,
            &mut store_warnings,
            || {
                built = true;
                self.run_compute_hook();
                match self.build_report(&key, limit, o, &mut notes) {
                    Ok(rep) => Ok(Outcome::Found(rep)),
                    Err(e) => {
                        failure = Some(e);
                        Ok(Outcome::NoMatch(String::new()))
                    }
                }
            },
            o.fresh,
        )?;
        if let Some(e) = failure {
            return Err(e);
        }
        if let Some(p) = pending
            && !notes.unstorable
        {
            self.commit_write(p, &mut store_warnings);
        }
        let mut warnings = notes.warnings;
        warnings.extend(store_warnings);
        warnings.extend(self.take_notes());
        let partial = match &out {
            Outcome::Found(rep) => rep.diff.partial.clone(),
            _ => Vec::new(),
        };
        Ok(Reply {
            workspace: None,
            outcome: out,
            partial,
            folded: Vec::new(),
            report: Report {
                watermark: if built { notes.watermark } else { 0 },
                pending: 0,
                stale: false,
                timed_out: false,
                queued: 0,
                warnings,
                positions: key.b.clone(),
                exact: true,
                view_id: Some(req.view_id.clone()),
                context: None,
                working_set: None,
            },
            diagnostic: None,
        })
    }
}

/// What checking one stored report found.
pub(crate) enum CheckOne {
    /// The entry names other positions, or a side's context is not on this machine: a check
    /// never starts an index.
    OtherPosition,
    OtherValidity,
    /// Compared with a fresh build: `None` when equal, else the first differing byte.
    Checked(Option<u64>),
}

impl Katalogr {
    /// `Markdown` or `Html`, stored under that format's own view ID. `Json` is a usage error.
    pub fn diff_page(
        &self,
        r: &DiffRequest,
        f: DiffRender,
        o: &Options,
    ) -> Result<Reply<DiffPage>, DistError> {
        if f == DiffRender::Json {
            return Err(DistError::usage(
                "a page is Markdown or HTML; the JSON report is `diff_report`",
            ));
        }
        let key = self.diff_key(r, o)?;
        let q = Self::diff_query(&key, r.limit as u64, f);
        let req = AnswerRequest {
            view_id: katalogr_query::view_id(&key.b, &key.facts, &q),
            op: &q.op,
            args: q.args.clone(),
            inputs: BTreeMap::from([("rules".to_string(), key.rules.clone())]),
            storable: key.storable,
        };
        let mut inner: Option<Reply<DiffReport>> = None;
        let mut failure: Option<DistError> = None;
        let mut warnings = Vec::new();
        let (out, pending) = self.read_through(
            &req,
            &mut warnings,
            || match self.diff_report(r, o) {
                Ok(rep) => {
                    let text = match &rep.outcome {
                        Outcome::Found(d) if f == DiffRender::Markdown => to_markdown(d),
                        Outcome::Found(d) => to_html(d),
                        _ => String::new(),
                    };
                    inner = Some(rep);
                    Ok(Outcome::Found(DiffPage { text }))
                }
                Err(e) => {
                    failure = Some(e);
                    Ok(Outcome::NoMatch(String::new()))
                }
            },
            o.fresh,
        )?;
        if let Some(e) = failure {
            return Err(e);
        }
        // A page built from a report that was not stored is not stored either.
        let storable_now = inner.as_ref().is_none_or(|i| {
            !i.report
                .warnings
                .iter()
                .any(|w| w.contains("is not stored"))
        });
        if let Some(p) = pending
            && storable_now
        {
            self.commit_write(p, &mut warnings);
        }
        let mut report = match inner {
            Some(i) => i.report,
            None => Report {
                watermark: 0,
                pending: 0,
                stale: false,
                timed_out: false,
                queued: 0,
                warnings: Vec::new(),
                positions: key.b.clone(),
                exact: true,
                view_id: None,
                context: None,
                working_set: None,
            },
        };
        report.warnings.extend(warnings);
        report.view_id = Some(req.view_id.clone());
        Ok(Reply {
            workspace: None,
            outcome: out,
            partial: Vec::new(),
            folded: Vec::new(),
            report,
            diagnostic: None,
        })
    }

    /// One stored report against a fresh build, when both sides' contexts are already here.
    /// `view_id` and `validity` are the listing's; `facts` is the base's.
    pub(crate) fn check_diff(
        &self,
        q: &DiffQuery,
        view_id: &str,
        stored_validity: &str,
        facts: &str,
        stored: &[u8],
        o: &Options,
    ) -> Result<CheckOne, DistError> {
        let key = DiffKey {
            repo: q.repo.clone(),
            a: q.a.clone(),
            b: q.b.clone(),
            facts: facts.to_string(),
            rules: project_rules_digest(&self.project)?,
            storable: false,
        };
        let vq = Self::diff_query(&key, q.limit, q.render);
        if katalogr_query::view_id(&key.b, &key.facts, &vq) != view_id {
            return Ok(CheckOne::OtherPosition);
        }
        let inputs = BTreeMap::from([("rules".to_string(), key.rules.clone())]);
        let same_validity = self
            .answers_build()
            .is_some_and(|b| crate::answers::validity(b, &inputs) == stored_validity);
        if !same_validity {
            return Ok(CheckOne::OtherValidity);
        }
        let here = |side: &[PositionRef]| {
            let commit = key.commit_of(side);
            self.project
                .layer(&ContextId::for_commit(&key.repo, &commit))
                .root()
                .join(ContextManifest::FILE)
                .is_file()
        };
        if !here(&key.a) || !here(&key.b) {
            return Ok(CheckOne::OtherPosition);
        }
        let fresh_opts = Options {
            fresh: true,
            ..o.clone()
        };
        let mut notes = BuildNotes::default();
        let report = self.build_report(&key, q.limit, &fresh_opts, &mut notes)?;
        let bytes = match q.render {
            DiffRender::Json => serde_json::to_vec(&report),
            DiffRender::Markdown => serde_json::to_vec(&DiffPage {
                text: to_markdown(&report),
            }),
            DiffRender::Html => serde_json::to_vec(&DiffPage {
                text: to_html(&report),
            }),
        }
        .map_err(|e| DistError::usage(format!("a report does not serialise: {e}")))?;
        Ok(CheckOne::Checked(
            (bytes != stored).then(|| first_difference(&bytes, stored)),
        ))
    }
}

/// The context id for `commit` of `repo`; for tests.
#[doc(hidden)]
pub fn commit_context_id(repo: &str, commit: &str) -> ContextId {
    ContextId::for_commit(repo, commit)
}
