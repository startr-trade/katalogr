//! `tests`: the tests that exercise or verify a symbol, computed at query time (FEAT-063).
//! A backward walk over `calls` links and contract consumption, plus the `verified-by` links
//! that name the symbol, collects every node carrying the neutral `test` fact.

use crate::rank::row_weight;
use crate::resolve::hit;
use crate::result::{SymbolHit, TestHit};
use crate::step::incoming_many;
use katalogr_model::insight::{FACT_TEST, TEST_CASE, VERIFIED_BY};
use katalogr_model::{Confidence, Fact, Id, LinkKind, Name};
use katalogr_store::{LinkKey, Reader, StoreError};
use std::collections::{BTreeMap, BTreeSet};

/// Default number of call hops `tests` follows.
pub const DEFAULT_TEST_DEPTH: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TestsOptions {
    /// Hops to follow, at least 1.
    pub depth: u32,
    /// Only report role `case`.
    pub cases_only: bool,
}

impl Default for TestsOptions {
    fn default() -> Self {
        TestsOptions {
            depth: DEFAULT_TEST_DEPTH,
            cases_only: false,
        }
    }
}

/// Extracted 3, Declared 2, Inferred 1.
fn strength(c: Confidence) -> u8 {
    match c {
        Confidence::Extracted => 3,
        Confidence::Declared => 2,
        Confidence::Inferred => 1,
    }
}

fn from_strength(s: u8) -> Confidence {
    match s {
        3 => Confidence::Extracted,
        2 => Confidence::Declared,
        _ => Confidence::Inferred,
    }
}

/// The state of the best path found to a node.
#[derive(Clone, Debug)]
struct Path {
    depth: u32,
    strength: u8,
    score: f64,
    seed: Id,
    parent: Option<Id>,
    via: Option<String>,
}

/// Canonical name of a symbol, artifact or contract; the hex id for anything else.
fn canon(tx: &impl Reader, id: Id) -> Result<String, StoreError> {
    if let Some(s) = tx.symbol(id)? {
        return Ok(s.name.canonical());
    }
    if let Some(a) = tx.artifact(id)? {
        return Ok(a.name.canonical());
    }
    if let Some(c) = tx.contract(id)? {
        return Ok(c.name.canonical());
    }
    Ok(format!("{id:?}"))
}

/// The role of a test node and its hit, when the node carries the `test` fact as a string.
fn role_of(tx: &impl Reader, id: Id) -> Result<Option<(String, SymbolHit)>, StoreError> {
    let role = |f: &std::collections::BTreeMap<String, Fact>| match f.get(FACT_TEST) {
        Some(Fact::Str(s)) => Some(s.clone()),
        _ => None,
    };
    if let Some(row) = tx.symbol(id)? {
        return Ok(role(&row.facts).map(|r| (r, hit(&row))));
    }
    if let Some(a) = tx.artifact(id)?
        && let Some(r) = role(&a.facts)
    {
        let h = crate::dangling::referrer(tx, id, None)?;
        return Ok(h.map(|h| (r, h)));
    }
    Ok(None)
}

/// Tests that exercise or verify `target`.
pub fn tests(
    tx: &impl Reader,
    target: &SymbolHit,
    opts: TestsOptions,
) -> Result<Vec<TestHit>, StoreError> {
    tests_for(tx, &[Id::of(&target.name)], opts)
}

fn exists(tx: &impl Reader, id: Id) -> Result<bool, StoreError> {
    Ok(tx.symbol(id)?.is_some() || tx.artifact(id)?.is_some() || tx.contract(id)?.is_some())
}

/// Whether `new` beats `old` for the same node: higher strength, then higher score, then the
/// smaller seed name, then the smaller parent name.
fn better(tx: &impl Reader, new: &Path, old: &Path) -> Result<bool, StoreError> {
    if new.strength != old.strength {
        return Ok(new.strength > old.strength);
    }
    if new.score != old.score {
        return Ok(new.score > old.score);
    }
    if new.seed != old.seed {
        return Ok(canon(tx, new.seed)? < canon(tx, old.seed)?);
    }
    match (new.parent, old.parent) {
        (Some(a), Some(b)) if a != b => Ok(canon(tx, a)? < canon(tx, b)?),
        _ => Ok(false),
    }
}

fn offer(
    tx: &impl Reader,
    cand: &mut BTreeMap<Id, Path>,
    node: Id,
    path: Path,
) -> Result<(), StoreError> {
    match cand.get(&node) {
        Some(old) if !better(tx, &path, old)? => {}
        _ => {
            cand.insert(node, path);
        }
    }
    Ok(())
}

/// Impact API: tests reaching any of `seeds` (symbol, artifact or contract ids; unknown ids
/// are ignored). Each test appears once, with its best path to its nearest seed. Sorted by
/// depth, confidence (strongest first), score (highest first), then name.
pub fn tests_for(
    tx: &impl Reader,
    seeds: &[Id],
    opts: TestsOptions,
) -> Result<Vec<TestHit>, StoreError> {
    let seeds: BTreeSet<Id> = seeds.iter().copied().collect();
    let mut settled: BTreeMap<Id, Path> = BTreeMap::new();
    for &s in &seeds {
        if exists(tx, s)? {
            settled.insert(
                s,
                Path {
                    depth: 0,
                    strength: 3,
                    score: 1.0,
                    seed: s,
                    parent: None,
                    via: None,
                },
            );
        }
    }
    let mut frontier: Vec<Id> = settled.keys().copied().collect();
    let verified = LinkKind(VERIFIED_BY.to_string());
    let has_contracts = !tx.contracts()?.is_empty();
    for d in 1..=opts.depth.max(1) {
        let mut cand: BTreeMap<Id, Path> = BTreeMap::new();
        if d == 1 {
            // A requirement names its tests: the test is one hop from the requirement.
            for &n in &frontier {
                let seed = settled[&n].seed;
                for (key, row) in tx.links_out(n, Some(&verified))? {
                    if settled.contains_key(&key.to) {
                        continue;
                    }
                    let Some(c) = row.entries.first().map(|e| e.confidence) else {
                        continue;
                    };
                    let path = Path {
                        depth: 1,
                        strength: strength(c),
                        score: row_weight(&row),
                        seed,
                        parent: Some(n),
                        via: None,
                    };
                    offer(tx, &mut cand, key.to, path)?;
                }
            }
        }
        let steps = incoming_many(tx, &frontier, has_contracts)?;
        // Score every link of the level in one batch.
        let mut keys: Vec<LinkKey> = Vec::new();
        for st in steps.iter().flatten() {
            if settled.contains_key(&st.from) {
                continue;
            }
            keys.push(st.key.clone());
            if let Some((_, Some(p))) = &st.via {
                keys.push(p.clone());
            }
        }
        let mut briefs = tx.link_briefs_many(&keys)?.into_iter();
        let mut next_brief = || -> Result<(u8, f64), StoreError> {
            let b = briefs
                .next()
                .flatten()
                .ok_or_else(|| StoreError::Row("link without a row".into()))?;
            Ok((strength(b.confidence), b.weight))
        };
        for (&n, steps) in frontier.iter().zip(steps) {
            let from = settled[&n].clone();
            for step in steps {
                if settled.contains_key(&step.from) {
                    continue;
                }
                let (mut s, mut w) = next_brief()?;
                let mut via = from.via.clone();
                if let Some((contract, provides)) = &step.via {
                    if provides.is_some() {
                        let (ps, pw) = next_brief()?;
                        s = s.min(ps);
                        w *= pw;
                    }
                    via = via.or_else(|| Some(contract.clone()));
                }
                let path = Path {
                    depth: d,
                    strength: s.min(from.strength),
                    score: from.score * w,
                    seed: from.seed,
                    parent: Some(n),
                    via,
                };
                offer(tx, &mut cand, step.from, path)?;
            }
        }
        frontier = cand.keys().copied().collect();
        settled.extend(cand);
        if frontier.is_empty() {
            break;
        }
    }

    // Roles of every settled node: briefs in one batch; full rows only for the tests.
    let ids: Vec<Id> = settled.keys().copied().collect();
    let briefs = tx.symbol_briefs_many(&ids)?;
    let mut found: Vec<(Id, String, SymbolHit)> = Vec::new();
    for (id, brief) in ids.iter().zip(briefs) {
        let r = match brief {
            Some(b) => match b.facts.get(FACT_TEST) {
                Some(Fact::Str(r)) => tx.symbol(*id)?.map(|row| (r.clone(), hit(&row))),
                _ => None,
            },
            None => role_of(tx, *id)?,
        };
        if let Some((role, test)) = r
            && (!opts.cases_only || role == TEST_CASE)
        {
            found.push((*id, role, test));
        }
    }
    let found_ids: Vec<Id> = found.iter().map(|f| f.0).collect();
    let incoming_links = tx.links_in_many(&found_ids)?;
    let mut names: std::collections::HashMap<Id, String> = std::collections::HashMap::new();
    let mut name_of =
        |tx: &dyn Fn(Id) -> Result<String, StoreError>, id: Id| -> Result<String, StoreError> {
            if let Some(n) = names.get(&id) {
                return Ok(n.clone());
            }
            let n = tx(id)?;
            names.insert(id, n.clone());
            Ok(n)
        };
    let canon_fn = |id: Id| canon(tx, id);
    let mut ranked: Vec<(u32, u8, f64, TestHit)> = Vec::new();
    for ((id, role, test), links) in found.into_iter().zip(incoming_links) {
        let path = &settled[&id];
        let mut chain = Vec::new();
        let mut at = path.parent;
        while let Some(p) = at {
            let parent = &settled[&p];
            if parent.parent.is_some() {
                chain.push(name_of(&canon_fn, p)?);
            }
            at = parent.parent;
        }
        let mut requirements = BTreeSet::new();
        for key in links.iter().filter(|k| k.kind.0 == VERIFIED_BY) {
            requirements.insert(name_of(&canon_fn, key.from)?);
        }
        let hit = TestHit {
            test,
            role,
            depth: path.depth,
            confidence: from_strength(path.strength),
            score: (path.score * 1e4).round() / 1e4,
            seed: name_of(&canon_fn, path.seed)?,
            chain,
            via: path.via.clone(),
            requirements: requirements.into_iter().collect(),
        };
        ranked.push((path.depth, path.strength, path.score, hit));
    }
    ranked.sort_by(|a, b| {
        (a.0, std::cmp::Reverse(a.1))
            .cmp(&(b.0, std::cmp::Reverse(b.1)))
            .then(b.2.total_cmp(&a.2))
            .then_with(|| a.3.test.name.cmp(&b.3.test.name))
    });
    Ok(ranked.into_iter().map(|r| r.3).collect())
}

#[cfg(test)]
mod explain_contract_fixture {
    use crate::result::SymbolHit;
    use crate::{Explain, explain};
    use camino::Utf8PathBuf;
    use katalogr_model::{
        ArtifactName, Confidence, ContractName, Fragment, Id, LinkKind, Name, Provenance, RelPath,
        Segment, Span, SymbolName,
    };
    use katalogr_store::{
        Cache, ContractRow, Kindex, LinkEntry, LinkKey, LinkOrigin, LinkRow, ProjectConfig,
        ProjectName, RepoConfig, Restitch, StoreError, SymbolRow, WriteTx,
    };
    use std::time::{Duration, Instant};

    struct NoShards;

    impl Restitch for NoShards {
        fn replace_fragment(&self, _: &mut WriteTx, _: Fragment) -> Result<(), StoreError> {
            Ok(())
        }
        fn remove_artifact(&self, _: &mut WriteTx, _: &ArtifactName) -> Result<(), StoreError> {
            Ok(())
        }
    }

    const SPAN: Span = Span {
        start_line: 3,
        start_col: 1,
        end_line: 3,
        end_col: 9,
    };

    /// A cache holding `contract:channel:orders` with the given provider, consumer and declarer
    /// counts; returns it with the contract's hit.
    pub(super) fn build(
        providers: u32,
        consumers: u32,
        declarers: u32,
    ) -> (tempfile::TempDir, Cache, SymbolHit) {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let dir = Kindex::init(&root)
            .unwrap()
            .create_project(
                &ProjectName::new("ec").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let cache =
            Cache::open_or_rebuild(&dir, &NoShards, Instant::now() + Duration::from_secs(60))
                .unwrap();
        let path = RelPath::new("f.tx").unwrap();
        let artifact = ArtifactName {
            repo: "r".into(),
            path: path.clone(),
        };
        let provenance = Provenance {
            repo: "r".into(),
            path,
            revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
        };
        let cname = ContractName {
            kind: "channel".into(),
            key: "orders".into(),
        };
        let cid = Id::of(&cname.canonical());
        let mut tx = cache.write().unwrap();
        tx.put_contract(
            cid,
            &ContractRow {
                name: cname.clone(),
                providers,
                consumers,
                declarers,
            },
        )
        .unwrap();
        for (role, count) in [
            ("provides", providers),
            ("consumes", consumers),
            ("declares", declarers),
        ] {
            for i in 0..count {
                let n = SymbolName {
                    artifact: artifact.clone(),
                    segments: vec![Segment {
                        kind: "fn".into(),
                        name: format!("{role}_{i}"),
                    }],
                    ordinal: 1,
                };
                let id = n.id();
                tx.put_symbol(
                    id,
                    &SymbolRow {
                        name: n,
                        artifact: artifact.id(),
                        kind: "fn".into(),
                        span: SPAN,
                        facts: Default::default(),
                    },
                )
                .unwrap();
                tx.put_link(
                    &LinkKey {
                        from: id,
                        kind: LinkKind(role.into()),
                        to: cid,
                    },
                    &LinkRow {
                        entries: vec![LinkEntry {
                            origin: LinkOrigin::Intra,
                            confidence: Confidence::Extracted,
                            span: Some(SPAN),
                            provenance: provenance.clone(),
                            fanout: None,
                            produced_by: None,
                        }],
                    },
                )
                .unwrap();
            }
        }
        tx.commit().unwrap();
        let hit = SymbolHit {
            name: cname.canonical(),
            kind: "contract".into(),
            path: String::new(),
            span: Span {
                start_line: 1,
                start_col: 1,
                end_line: 1,
                end_col: 1,
            },
        };
        (tmp, cache, hit)
    }

    pub(super) fn run(cache: &Cache, hit: &SymbolHit, per_side: usize) -> Explain {
        let r = cache.read().unwrap();
        explain(&r, hit, per_side).unwrap()
    }
}

#[cfg(test)]
#[test]
fn explain_contract_groups_by_role() {
    let (_tmp, cache, hit) = explain_contract_fixture::build(2, 1, 1);
    let x = explain_contract_fixture::run(&cache, &hit, 10);
    let c = x.contract.expect("contract block");
    assert_eq!((c.kind.as_str(), c.key.as_str()), ("channel", "orders"));
    assert_eq!(
        (c.providers_total, c.consumers_total, c.declarers_total),
        (2, 1, 1)
    );
    let names = |v: &[crate::ContractEnd]| v.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
    assert_eq!(
        names(&c.providers),
        ["r:f.tx#fn:provides_0", "r:f.tx#fn:provides_1"]
    );
    assert_eq!(names(&c.consumers), ["r:f.tx#fn:consumes_0"]);
    assert_eq!(names(&c.declarers), ["r:f.tx#fn:declares_0"]);
    assert_eq!(c.providers[0].path, "f.tx");
    assert_eq!(c.providers[0].span.start_line, 3);
    assert_eq!(x.outgoing_total, 0);
    assert_eq!(x.incoming_total, 4);
}

#[cfg(test)]
#[test]
fn explain_contract_totals_exceed_cap() {
    let (_tmp, cache, hit) = explain_contract_fixture::build(1, 3, 0);
    let x = explain_contract_fixture::run(&cache, &hit, 1);
    let c = x.contract.expect("contract block");
    assert_eq!(c.consumers.len(), 1);
    assert_eq!(c.consumers[0].name, "r:f.tx#fn:consumes_0");
    assert_eq!(c.consumers_total, 3);
    assert_eq!(c.providers_total, 1);
    assert_eq!(x.incoming.len(), 1);
    assert_eq!(x.incoming_total, 4);
}

#[cfg(test)]
#[test]
fn explain_symbol_json_has_no_contract_key() {
    let (_tmp, cache, hit) = explain_contract_fixture::build(1, 0, 0);
    let mut x = explain_contract_fixture::run(&cache, &hit, 10);
    assert!(serde_json::to_value(&x).unwrap().get("contract").is_some());
    x.contract = None;
    assert!(serde_json::to_value(&x).unwrap().get("contract").is_none());
}
