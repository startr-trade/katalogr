//! Index coverage (FEAT-062): how many references are resolved, inferred or unresolved, how many
//! contracts dangle, how many files failed, and which files no extension claims, per repository,
//! extension and language.

use crate::metrics::{Better, MetricSample, metric_key, round6};
use crate::result::Itemized;
use katalogr_model::{ExtractError, Id, RelPath};
use katalogr_store::{Reader, RefId, StoreError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Everything coverage needs that is not in the snapshot; built by dist.
pub struct CoverageInput<'a> {
    /// Language of an indexed file: (extension id, path) -> label. Dist asks the registry
    /// (`Extractor::language`); an unknown extension id maps to the id itself.
    pub language: &'a dyn Fn(&str, &RelPath) -> String,
    /// Which extension claims a path that has no artifact (for failed and unsupported files):
    /// `Some((extension id, language))`, or None when nothing claims it.
    pub claim: &'a dyn Fn(&RelPath) -> Option<(String, String)>,
    /// Per repo: unclaimed files from the census. Repos missing here were not walked.
    pub unclaimed: BTreeMap<String, Vec<RelPath>>,
    /// Repos whose root is unknown on this machine (reported, not walked).
    pub roots_missing: Vec<String>,
    /// Artifacts whose path starts with one of these are not files (history commits).
    pub exclude_prefixes: Vec<String>,
    /// Only this repo, when set.
    pub repo: Option<String>,
    /// How many suffixes to list per repo for unclaimed files.
    pub suffix_top: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CoverageCounts {
    pub files_indexed: u64,
    /// Indexed files whose extraction recorded parse errors (the fragment was still published).
    pub files_parse_errors: u64,
    /// Number of parse-error entries over those files.
    pub parse_errors: u64,
    /// Claimed files an extension refused as unsupported (no artifact).
    pub files_unsupported: u64,
    /// Claimed files whose extraction failed outright (no artifact).
    pub files_failed: u64,
    /// Files no extension claims (repo totals only).
    pub files_unclaimed: u64,
    pub refs_total: u64,
    /// One target, with the reference's own confidence not inferred.
    pub refs_resolved: u64,
    /// Inferred: several targets, or the reference was inferred.
    pub refs_inferred: u64,
    /// No target (`refs_unresolved` table), plus `refs_unlinked`.
    pub refs_unresolved: u64,
    /// Neither linked nor recorded unresolved (should be 0; non-zero means a stitcher gap).
    pub refs_unlinked: u64,
    /// Distinct dangling contracts bound by the group's files.
    pub contracts_dangling: u64,
    /// files_parse_errors / files_indexed; None when files_indexed = 0.
    pub parse_error_rate: Option<f64>,
    /// refs_resolved / refs_total; None when refs_total = 0. Likewise the next two.
    pub resolution_rate: Option<f64>,
    pub inferred_rate: Option<f64>,
    pub unresolved_rate: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CoverageRow {
    /// For `groups`: all three; for a rollup: only its own key, the others None.
    pub repo: Option<String>,
    pub extension: Option<String>,
    pub language: Option<String>,
    pub counts: CoverageCounts,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct KindCounts {
    pub kind: String,
    pub total: u64,
    pub resolved: u64,
    pub inferred: u64,
    pub unresolved: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ContractKindCount {
    pub kind: String,
    pub dangling: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SuffixCount {
    pub suffix: String,
    pub files: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UnclaimedRow {
    pub repo: String,
    pub files: u64,
    pub suffixes: Vec<SuffixCount>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CoverageReport {
    pub totals: CoverageCounts,
    pub by_repo: Vec<CoverageRow>,
    pub by_extension: Vec<CoverageRow>,
    pub by_language: Vec<CoverageRow>,
    /// Finest grain: (repo, extension, language).
    pub groups: Vec<CoverageRow>,
    pub by_link_kind: Vec<KindCounts>,
    pub dangling_contracts_by_kind: Vec<ContractKindCount>,
    pub unclaimed: Vec<UnclaimedRow>,
    pub roots_missing: Vec<String>,
    /// The `coverage.*` samples, for quality metrics and gates.
    pub metrics: Vec<MetricSample>,
}

impl Itemized for CoverageReport {
    type Item = CoverageRow;
    fn items_mut(&mut self) -> &mut Vec<CoverageRow> {
        &mut self.groups
    }
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RefClass {
    Resolved,
    Inferred,
}

/// The class of every linked reference: one walk over the ref-origin entries; an entry that
/// came from a reference is resolved when it is not inferred and its reference had at most one
/// target, else inferred; the better class wins per `(from, kind, ref index)`.
fn link_classes(tx: &impl Reader) -> Result<BTreeMap<(Id, String, u32), RefClass>, StoreError> {
    classes_from(&mut |f| tx.for_each_ref_entry(f))
}

/// [`link_classes`] over the entries `walk` delivers as `(from, kind, ref index, resolved)`.
pub(crate) fn classes_from(
    walk: &mut dyn FnMut(&mut dyn FnMut(Id, &str, u32, bool)) -> Result<(), StoreError>,
) -> Result<BTreeMap<(Id, String, u32), RefClass>, StoreError> {
    let mut out: BTreeMap<(Id, String, u32), RefClass> = BTreeMap::new();
    walk(&mut |from, kind, i, resolved| {
        let class = if resolved {
            RefClass::Resolved
        } else {
            RefClass::Inferred
        };
        out.entry((from, kind.to_string(), i))
            .and_modify(|c| *c = (*c).min(class))
            .or_insert(class);
    })?;
    Ok(out)
}

/// The classes the old way: one expanded `links_all` scan. The reference of the tests.
#[cfg(test)]
pub(crate) fn link_classes_for_test(
    tx: &impl Reader,
) -> BTreeMap<(Id, String, u32), RefClass> {
    use katalogr_store::LinkOrigin;
    let mut out: BTreeMap<(Id, String, u32), RefClass> = BTreeMap::new();
    for (key, row) in tx.links_all().unwrap() {
        for e in &row.entries {
            let LinkOrigin::Ref(i) = e.origin else {
                continue;
            };
            let class = if e.confidence != katalogr_model::Confidence::Inferred
                && e.fanout.is_none_or(|n| n <= 1)
            {
                RefClass::Resolved
            } else {
                RefClass::Inferred
            };
            out.entry((key.from, key.kind.0.clone(), i))
                .and_modify(|c| *c = (*c).min(class))
                .or_insert(class);
        }
    }
    out
}

/// Text after the last `.` of the file name, lowercased, with a leading `.`; `(none)` without a
/// dot. A name that is only a leading-dot name (`.gitignore`) is its own suffix.
fn suffix_of(path: &RelPath) -> String {
    let name = path.as_str().rsplit('/').next().unwrap_or("");
    match name.rfind('.') {
        None => "(none)".to_string(),
        Some(0) => name.to_lowercase(),
        Some(i) => name[i..].to_lowercase(),
    }
}

type GroupKey = (String, String, String);

#[derive(Default, Clone)]
struct Acc {
    c: CoverageCounts,
    dangling: BTreeSet<Id>,
}

impl Acc {
    fn add(&mut self, other: &Acc) {
        let (a, b) = (&mut self.c, &other.c);
        a.files_indexed += b.files_indexed;
        a.files_parse_errors += b.files_parse_errors;
        a.parse_errors += b.parse_errors;
        a.files_unsupported += b.files_unsupported;
        a.files_failed += b.files_failed;
        a.files_unclaimed += b.files_unclaimed;
        a.refs_total += b.refs_total;
        a.refs_resolved += b.refs_resolved;
        a.refs_inferred += b.refs_inferred;
        a.refs_unresolved += b.refs_unresolved;
        a.refs_unlinked += b.refs_unlinked;
        self.dangling.extend(other.dangling.iter().copied());
    }

    fn finish(mut self) -> CoverageCounts {
        let c = &mut self.c;
        c.contracts_dangling = self.dangling.len() as u64;
        let rate = |n: u64, d: u64| (d > 0).then(|| round6(n as f64 / d as f64));
        c.parse_error_rate = rate(c.files_parse_errors, c.files_indexed);
        c.resolution_rate = rate(c.refs_resolved, c.refs_total);
        c.inferred_rate = rate(c.refs_inferred, c.refs_total);
        c.unresolved_rate = rate(c.refs_unresolved, c.refs_total);
        self.c
    }
}

#[derive(Copy, Clone)]
enum Class {
    Resolved,
    Inferred,
    Unresolved,
}

/// Coverage of one snapshot. Read-only; errors only from reads.
pub fn coverage(tx: &impl Reader, input: &CoverageInput) -> Result<CoverageReport, StoreError> {
    let excluded = |path: &RelPath| {
        input
            .exclude_prefixes
            .iter()
            .any(|p| path.as_str().starts_with(p.as_str()))
    };
    let wanted_repo = |repo: &str| input.repo.as_deref().is_none_or(|r| r == repo);

    let mut groups: BTreeMap<GroupKey, Acc> = BTreeMap::new();
    let mut kept: Vec<(Id, GroupKey)> = Vec::new();
    let mut with_artifact: BTreeSet<(String, RelPath)> = BTreeSet::new();
    let mut rows = BTreeMap::new();
    for (id, row) in tx.artifacts()? {
        let name = &row.name;
        if excluded(&name.path) || !wanted_repo(&name.repo) {
            continue;
        }
        let key = (
            name.repo.clone(),
            row.extension.clone(),
            (input.language)(&row.extension, &name.path),
        );
        groups.entry(key.clone()).or_default().c.files_indexed += 1;
        with_artifact.insert((name.repo.clone(), name.path.clone()));
        kept.push((id, key));
        rows.insert(id, row);
    }

    let classes = link_classes(tx)?;
    let unresolved: BTreeSet<RefId> = tx
        .unresolved_refs(None)?
        .into_iter()
        .map(|(r, _)| r)
        .collect();

    let mut by_kind: BTreeMap<String, [u64; 4]> = BTreeMap::new();
    for (id, key) in &kept {
        let row = &rows[id];
        let acc = groups.get_mut(key).expect("group exists");
        for (i, r) in row.refs.iter().enumerate() {
            let i = i as u32;
            acc.c.refs_total += 1;
            let class = if unresolved.contains(&RefId {
                artifact: *id,
                index: i,
            }) {
                Class::Unresolved
            } else if let Some(c) = classes.get(&(r.from, r.kind.0.clone(), i)) {
                match c {
                    RefClass::Resolved => Class::Resolved,
                    RefClass::Inferred => Class::Inferred,
                }
            } else {
                acc.c.refs_unlinked += 1;
                Class::Unresolved
            };
            let k = by_kind.entry(r.kind.0.clone()).or_default();
            k[0] += 1;
            match class {
                Class::Resolved => {
                    acc.c.refs_resolved += 1;
                    k[1] += 1;
                }
                Class::Inferred => {
                    acc.c.refs_inferred += 1;
                    k[2] += 1;
                }
                Class::Unresolved => {
                    acc.c.refs_unresolved += 1;
                    k[3] += 1;
                }
            }
        }
    }

    // Dangling contracts: exactly one of providers, consumers is present.
    let dangling: BTreeMap<Id, String> = tx
        .contracts()?
        .into_iter()
        .filter(|(_, c)| (c.providers > 0) != (c.consumers > 0))
        .map(|(id, c)| (id, c.name.kind))
        .collect();
    let mut bound_all: BTreeSet<Id> = BTreeSet::new();
    if !dangling.is_empty() {
        for (id, key) in &kept {
            for b in tx.bindings_of(*id)? {
                if dangling.contains_key(&b.contract) {
                    groups
                        .get_mut(key)
                        .expect("group exists")
                        .dangling
                        .insert(b.contract);
                    bound_all.insert(b.contract);
                }
            }
        }
    }
    let mut dangling_kinds: BTreeMap<String, u64> = BTreeMap::new();
    for id in &bound_all {
        *dangling_kinds.entry(dangling[id].clone()).or_default() += 1;
    }

    // Index errors: parse errors on indexed files; unsupported and failed files without one.
    for ((repo, path), errs) in tx.index_errors_all()? {
        if excluded(&path) || !wanted_repo(&repo) || errs.is_empty() {
            continue;
        }
        if with_artifact.contains(&(repo.clone(), path.clone())) {
            // The group of that artifact.
            let key = kept
                .iter()
                .find(|(id, _)| {
                    let n = &rows[id].name;
                    n.repo == repo && n.path == path
                })
                .map(|(_, k)| k.clone())
                .expect("artifact kept");
            let c = &mut groups.get_mut(&key).expect("group exists").c;
            c.files_parse_errors += 1;
            c.parse_errors += errs.len() as u64;
        } else {
            let (ext, lang) = (input.claim)(&path)
                .unwrap_or_else(|| ("(none)".to_string(), "(none)".to_string()));
            let c = &mut groups.entry((repo, ext, lang)).or_default().c;
            if errs
                .iter()
                .all(|e| ExtractError::is_unsupported_message(&e.message))
            {
                c.files_unsupported += 1;
            } else {
                c.files_failed += 1;
            }
        }
    }

    // Unclaimed files.
    let mut unclaimed_rows: Vec<UnclaimedRow> = Vec::new();
    let mut repo_unclaimed: BTreeMap<String, u64> = BTreeMap::new();
    for (repo, files) in &input.unclaimed {
        if !wanted_repo(repo) {
            continue;
        }
        let mut suffixes: BTreeMap<String, u64> = BTreeMap::new();
        for f in files {
            *suffixes.entry(suffix_of(f)).or_default() += 1;
        }
        let mut suffixes: Vec<SuffixCount> = suffixes
            .into_iter()
            .map(|(suffix, files)| SuffixCount { suffix, files })
            .collect();
        suffixes.sort_by(|a, b| b.files.cmp(&a.files).then_with(|| a.suffix.cmp(&b.suffix)));
        suffixes.truncate(input.suffix_top);
        repo_unclaimed.insert(repo.clone(), files.len() as u64);
        unclaimed_rows.push(UnclaimedRow {
            repo: repo.clone(),
            files: files.len() as u64,
            suffixes,
        });
    }
    unclaimed_rows.sort_by(|a, b| a.repo.cmp(&b.repo));

    // Rollups.
    let mut totals = Acc::default();
    let mut by_repo: BTreeMap<String, Acc> = BTreeMap::new();
    let mut by_ext: BTreeMap<String, Acc> = BTreeMap::new();
    let mut by_lang: BTreeMap<String, Acc> = BTreeMap::new();
    for ((repo, ext, lang), acc) in &groups {
        totals.add(acc);
        by_repo.entry(repo.clone()).or_default().add(acc);
        by_ext.entry(ext.clone()).or_default().add(acc);
        by_lang.entry(lang.clone()).or_default().add(acc);
    }
    for (repo, n) in &repo_unclaimed {
        by_repo.entry(repo.clone()).or_default().c.files_unclaimed += n;
        totals.c.files_unclaimed += n;
    }

    let row = |repo: Option<&String>, ext: Option<&String>, lang: Option<&String>, acc: Acc| {
        CoverageRow {
            repo: repo.cloned(),
            extension: ext.cloned(),
            language: lang.cloned(),
            counts: acc.finish(),
        }
    };
    let mut metrics = Vec::new();
    sample(&mut metrics, &totals.clone().finish(), None, true);
    let by_repo: Vec<CoverageRow> = by_repo
        .into_iter()
        .map(|(k, a)| row(Some(&k), None, None, a))
        .collect();
    let by_extension: Vec<CoverageRow> = by_ext
        .into_iter()
        .map(|(k, a)| row(None, Some(&k), None, a))
        .collect();
    let by_language: Vec<CoverageRow> = by_lang
        .into_iter()
        .map(|(k, a)| row(None, None, Some(&k), a))
        .collect();
    for r in &by_repo {
        sample(
            &mut metrics,
            &r.counts,
            Some(("repo", r.repo.as_deref())),
            true,
        );
    }
    for r in &by_extension {
        sample(
            &mut metrics,
            &r.counts,
            Some(("extension", r.extension.as_deref())),
            false,
        );
    }
    for r in &by_language {
        sample(
            &mut metrics,
            &r.counts,
            Some(("language", r.language.as_deref())),
            false,
        );
    }
    metrics.sort_by_key(metric_key);

    let groups: Vec<CoverageRow> = groups
        .into_iter()
        .map(|((r, e, l), a)| row(Some(&r), Some(&e), Some(&l), a))
        .collect();
    let mut roots_missing: Vec<String> = input
        .roots_missing
        .iter()
        .filter(|r| wanted_repo(r))
        .cloned()
        .collect();
    roots_missing.sort();
    roots_missing.dedup();

    Ok(CoverageReport {
        totals: totals.finish(),
        by_repo,
        by_extension,
        by_language,
        groups,
        by_link_kind: by_kind
            .into_iter()
            .map(|(kind, k)| KindCounts {
                kind,
                total: k[0],
                resolved: k[1],
                inferred: k[2],
                unresolved: k[3],
            })
            .collect(),
        dangling_contracts_by_kind: dangling_kinds
            .into_iter()
            .map(|(kind, dangling)| ContractKindCount { kind, dangling })
            .collect(),
        unclaimed: unclaimed_rows,
        roots_missing,
        metrics,
    })
}

/// The `coverage.*` samples of one scope. `label` is `(name, value)` of the rollup, none for the
/// totals; `with_unclaimed` is true for the totals and repositories.
fn sample(
    out: &mut Vec<MetricSample>,
    c: &CoverageCounts,
    label: Option<(&str, Option<&str>)>,
    with_unclaimed: bool,
) {
    let mut push = |name: &str, value: f64, better: Better| {
        let mut s = MetricSample::new(name, value, better);
        if let Some((k, Some(v))) = label {
            s = s.label(k, v);
        }
        out.push(s);
    };
    push(
        "coverage.files.indexed",
        c.files_indexed as f64,
        Better::Info,
    );
    if with_unclaimed {
        push(
            "coverage.files.unclaimed",
            c.files_unclaimed as f64,
            Better::Info,
        );
    }
    push(
        "coverage.files.unsupported",
        c.files_unsupported as f64,
        Better::Lower,
    );
    push(
        "coverage.files.failed",
        c.files_failed as f64,
        Better::Lower,
    );
    if let Some(v) = c.parse_error_rate {
        push("coverage.parse_error_rate", v, Better::Lower);
    }
    push("coverage.refs.total", c.refs_total as f64, Better::Info);
    if let Some(v) = c.resolution_rate {
        push("coverage.resolution_rate", v, Better::Higher);
    }
    if let Some(v) = c.inferred_rate {
        push("coverage.inferred_rate", v, Better::Lower);
    }
    if let Some(v) = c.unresolved_rate {
        push("coverage.unresolved_rate", v, Better::Lower);
    }
    push(
        "coverage.contracts.dangling",
        c.contracts_dangling as f64,
        Better::Lower,
    );
}
