//! `impact`: what a change reaches, across languages and repositories (FEAT-028).
//!
//! A breadth-first walk backward over the links that depend on the seeds, through contracts
//! (a provider reaches its consumers in one hop), grouped by link kind and confidence. The
//! walk reads one snapshot, writes nothing and names no language, format or protocol.

use crate::dangling::referrer;
use crate::doc_impact::DOC_LINK_KINDS;
use crate::resolve::{contract_node_hit, hit};
use crate::result::{ConfidenceSchema, Partial, SymbolHit, TestHit, cap};
use crate::tests::{
    DEFAULT_TEST_DEPTH, TestsOptions, canon, from_strength, role_of, strength, tests_for,
};
use katalogr_model::insight::FACT_TEST;
use katalogr_model::{AnyName, Confidence, Fact, Id, LinkKind, Name, parse_name};
use katalogr_store::{LinkKey, Reader, StoreError};
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Default hops the walk follows.
pub const DEFAULT_IMPACT_DEPTH: u32 = 3;
/// Default most nodes in `groups`.
pub const DEFAULT_IMPACT_LIMIT: usize = 500;
/// Link kinds never followed or reported: structure, and links that are not a dependency.
pub const IMPACT_SKIPPED_KINDS: &[&str] = &["contains", "refs-item", "touches", "verified-by"];

const PROVIDES: &str = "provides";
const CONSUMES: &str = "consumes";
const DECLARES: &str = "declares";
const ROLES: &[&str] = &[PROVIDES, CONSUMES, DECLARES];

/// The walk stops once this many times the limit of hits is settled.
const STOP_FACTOR: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImpactOptions {
    /// Hops, at least 1.
    pub depth: u32,
    /// Most nodes in `groups`, at least 1.
    pub limit: usize,
    /// Hops for the tests section.
    pub test_depth: u32,
}

impl Default for ImpactOptions {
    fn default() -> Self {
        ImpactOptions {
            depth: DEFAULT_IMPACT_DEPTH,
            limit: DEFAULT_IMPACT_LIMIT,
            test_depth: DEFAULT_TEST_DEPTH,
        }
    }
}

/// Where the walk starts. `precision` is one of the `SEED_*` names.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Seed {
    pub id: Id,
    pub precision: &'static str,
    pub wanted: Option<String>,
}

pub const SEED_SYMBOL: &str = "symbol";
pub const SEED_FILE: &str = "file";
pub const SEED_CONTRACT: &str = "contract";
pub const SEED_CONTRACT_CONSUMERS: &str = "contract-consumers";
pub const SEED_ITEM: &str = "item";
pub const SEED_BROKEN_REF: &str = "broken-ref";

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ImpactReport {
    /// Seeds found, sorted by name then precision. Unknown ids are dropped.
    pub seeds: Vec<ImpactSeed>,
    /// Sorted by confidence (strongest first), then kind.
    pub groups: Vec<ImpactGroup>,
    /// All hits in `groups`, before the limit; absent when the walk stopped early.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// As `tests_for` sorts, at most `limit`.
    pub tests: Vec<TestHit>,
    /// Each contract the walk reached, sorted by name.
    pub contracts: Vec<ImpactContract>,
    /// The repos of the seeds and hits, sorted.
    pub repos: Vec<String>,
    /// The projects this answer covers, sorted. Set by the facade: `impact` leaves it empty.
    pub projects: Vec<String>,
    /// Caps that cut this answer; the facade copies it to `Reply::partial`.
    pub partial: Vec<Partial>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ImpactSeed {
    pub node: SymbolHit,
    pub precision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wanted: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ImpactGroup {
    pub kind: String,
    #[schemars(with = "ConfidenceSchema")]
    pub confidence: Confidence,
    pub hits: Vec<ImpactHit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ImpactHit {
    pub node: SymbolHit,
    pub depth: u32,
    /// Product of the row weights on the path, rounded to 4 decimals.
    pub score: f64,
    /// Canonical name of the nearest seed.
    pub seed: String,
    /// Canonical names between the hit and the seed, hit side first; contracts excluded.
    pub chain: Vec<String>,
    /// The contract (`contract:<kind>:<key>`) the path goes through, when it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ImpactContract {
    /// Canonical name.
    pub contract: String,
    pub providers: u64,
    pub consumers: u64,
    /// `no-provider` or `no-consumer` when impact may continue outside this project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<String>,
}

/// The best path found to a node.
#[derive(Clone, Debug)]
struct ImpactPath {
    depth: u32,
    strength: u8,
    score: f64,
    seed: Id,
    parent: Option<Id>,
    /// The link kind that reached the node (empty for a seed).
    kind: String,
    via: Option<String>,
}

/// One possible step from a frontier node, before it is scored.
struct Cand {
    to: Id,
    parent: Id,
    kind: String,
    /// The links the step crosses: their strengths and weights combine.
    links: Vec<LinkKey>,
    /// The contract the step goes through, when it does.
    via: Option<String>,
}

/// Whether `new` beats `old` for the same node: strength, score, then the smaller seed name,
/// parent name, kind and contract.
fn better(tx: &impl Reader, new: &ImpactPath, old: &ImpactPath) -> Result<bool, StoreError> {
    if new.strength != old.strength {
        return Ok(new.strength > old.strength);
    }
    if new.score != old.score {
        return Ok(new.score > old.score);
    }
    if new.seed != old.seed {
        return Ok(canon(tx, new.seed)? < canon(tx, old.seed)?);
    }
    if new.parent != old.parent
        && let (Some(a), Some(b)) = (new.parent, old.parent)
    {
        return Ok(canon(tx, a)? < canon(tx, b)?);
    }
    Ok((&new.kind, &new.via) < (&old.kind, &old.via))
}

fn offer(
    tx: &impl Reader,
    cand: &mut BTreeMap<Id, ImpactPath>,
    node: Id,
    path: ImpactPath,
) -> Result<(), StoreError> {
    match cand.get(&node) {
        Some(old) if !better(tx, &path, old)? => {}
        _ => {
            cand.insert(node, path);
        }
    }
    Ok(())
}

/// The hit of a node a seed or a hit can be: symbol, artifact (kind `artifact`) or contract.
fn node_hit(tx: &impl Reader, id: Id) -> Result<Option<SymbolHit>, StoreError> {
    if let Some(row) = tx.symbol(id)? {
        return Ok(Some(hit(&row)));
    }
    if let Some(h) = referrer(tx, id, None)? {
        return Ok(Some(h));
    }
    Ok(tx
        .contract(id)?
        .map(|c| contract_node_hit(&c.name.canonical())))
}

/// One level of the walk from `frontier` (nodes in `Id` order), in batched reads. Returns the
/// best path to every node newly reached; nodes in `settled` are never offered again.
fn step_level(
    tx: &impl Reader,
    frontier: &[Id],
    settled: &BTreeMap<Id, ImpactPath>,
    precisions: &BTreeMap<Id, BTreeSet<&'static str>>,
    reached: &mut BTreeSet<Id>,
) -> Result<BTreeMap<Id, ImpactPath>, StoreError> {
    let ins = tx.links_in_many(frontier)?;
    let provs = tx.link_keys_out_many(frontier, &LinkKind(PROVIDES.to_string()))?;
    let decls = tx.link_keys_out_many(frontier, &LinkKind(DECLARES.to_string()))?;

    // The contracts these nodes provide or declare, with their incoming links.
    let mut contract_ids: BTreeSet<Id> = BTreeSet::new();
    for keys in provs.iter().chain(&decls) {
        contract_ids.extend(keys.iter().map(|k| k.to));
    }
    let mut contract_name: BTreeMap<Id, String> = BTreeMap::new();
    let mut known: Vec<Id> = Vec::new();
    for id in contract_ids {
        if let Some(c) = tx.contract(id)? {
            contract_name.insert(id, c.name.canonical());
            known.push(id);
        }
    }
    let ends: BTreeMap<Id, Vec<LinkKey>> = known
        .iter()
        .copied()
        .zip(tx.links_in_many(&known)?)
        .collect();

    let mut cands: Vec<Cand> = Vec::new();
    for (i, &n) in frontier.iter().enumerate() {
        // An ordinary dependency: whoever links to the node.
        for key in &ins[i] {
            let kind = key.kind.0.as_str();
            if IMPACT_SKIPPED_KINDS.contains(&kind) || ROLES.contains(&kind) {
                continue;
            }
            cands.push(Cand {
                to: key.from,
                parent: n,
                kind: key.kind.0.clone(),
                links: vec![key.clone()],
                via: None,
            });
        }
        // N provides C: C's consumers. N declares C: C's providers and consumers.
        for (keys, declares) in [(&provs[i], false), (&decls[i], true)] {
            for own in keys {
                let Some(name) = contract_name.get(&own.to) else {
                    continue;
                };
                reached.insert(own.to);
                for end in &ends[&own.to] {
                    let role = end.kind.0.as_str();
                    if role == CONSUMES || (declares && role == PROVIDES) {
                        cands.push(Cand {
                            to: end.from,
                            parent: n,
                            kind: role.to_string(),
                            links: vec![own.clone(), end.clone()],
                            via: Some(name.clone()),
                        });
                    }
                }
            }
        }
        // N is a seeded contract: its ends.
        if let Some(p) = precisions.get(&n)
            && let Some(c) = tx.contract(n)?
        {
            let name = c.name.canonical();
            let whole = p.contains(SEED_CONTRACT);
            if whole || p.contains(SEED_CONTRACT_CONSUMERS) {
                reached.insert(n);
                for key in &ins[i] {
                    let role = key.kind.0.as_str();
                    if ROLES.contains(&role) && (whole || role == CONSUMES) {
                        cands.push(Cand {
                            to: key.from,
                            parent: n,
                            kind: role.to_string(),
                            links: vec![key.clone()],
                            via: Some(name.clone()),
                        });
                    }
                }
            }
        }
    }
    cands.retain(|c| !settled.contains_key(&c.to));

    let keys: Vec<LinkKey> = cands.iter().flat_map(|c| c.links.iter().cloned()).collect();
    let mut briefs = tx.link_briefs_many(&keys)?.into_iter();
    let mut out: BTreeMap<Id, ImpactPath> = BTreeMap::new();
    for c in cands {
        let from = &settled[&c.parent];
        let mut s = from.strength;
        let mut w = from.score;
        for _ in &c.links {
            let b = briefs
                .next()
                .flatten()
                .ok_or_else(|| StoreError::Row("link without a row".into()))?;
            s = s.min(strength(b.confidence));
            w *= b.weight;
        }
        let path = ImpactPath {
            depth: from.depth + 1,
            strength: s,
            score: w,
            seed: from.seed,
            parent: Some(c.parent),
            kind: c.kind,
            via: c.via.or_else(|| from.via.clone()),
        };
        offer(tx, &mut out, c.to, path)?;
    }
    Ok(out)
}

/// The test role of each node, in order: from the symbol brief, else from the artifact.
fn test_flags(tx: &impl Reader, ids: &[Id]) -> Result<Vec<bool>, StoreError> {
    let briefs = tx.symbol_briefs_many(ids)?;
    let mut out = Vec::with_capacity(ids.len());
    for (id, brief) in ids.iter().zip(briefs) {
        out.push(match brief {
            Some(b) => matches!(b.facts.get(FACT_TEST), Some(Fact::Str(_))),
            None => role_of(tx, *id)?.is_some(),
        });
    }
    Ok(out)
}

/// The repository a canonical name belongs to; contracts belong to none.
fn repo_of(name: &str) -> Option<String> {
    match parse_name(name).ok()? {
        AnyName::Artifact(a) => Some(a.repo),
        AnyName::Symbol(s) => Some(s.artifact.repo),
        AnyName::Contract(_) => None,
    }
}

/// The walk over one snapshot. Pure and deterministic, with no write. Errors only from the store.
pub fn impact(
    tx: &impl Reader,
    seeds: &[Seed],
    opts: &ImpactOptions,
) -> Result<ImpactReport, StoreError> {
    let depth = opts.depth.max(1);
    let limit = opts.limit.max(1);

    // Seeds that exist, deduplicated.
    let sorted: BTreeSet<&Seed> = seeds.iter().collect();
    let mut seed_list: Vec<ImpactSeed> = Vec::new();
    let mut precisions: BTreeMap<Id, BTreeSet<&'static str>> = BTreeMap::new();
    for s in sorted {
        let Some(node) = node_hit(tx, s.id)? else {
            continue;
        };
        precisions.entry(s.id).or_default().insert(s.precision);
        seed_list.push(ImpactSeed {
            node,
            precision: s.precision.to_string(),
            wanted: s.wanted.clone(),
        });
    }
    seed_list.sort_by(|a, b| (&a.node.name, &a.precision).cmp(&(&b.node.name, &b.precision)));
    let seed_ids: Vec<Id> = precisions.keys().copied().collect();

    let mut settled: BTreeMap<Id, ImpactPath> = seed_ids
        .iter()
        .map(|&s| {
            (
                s,
                ImpactPath {
                    depth: 0,
                    strength: 3,
                    score: 1.0,
                    seed: s,
                    parent: None,
                    kind: String::new(),
                    via: None,
                },
            )
        })
        .collect();

    let mut reached: BTreeSet<Id> = BTreeSet::new();
    let mut frontier = seed_ids.clone();
    let mut stopped = false;
    let mut deeper = false;
    for d in 1..=depth + 1 {
        if frontier.is_empty() {
            break;
        }
        let cand = step_level(tx, &frontier, &settled, &precisions, &mut reached)?;
        if d > depth {
            // The probe level: only whether the depth cut the answer.
            let ids: Vec<Id> = cand.keys().copied().collect();
            deeper = test_flags(tx, &ids)?.iter().any(|t| !t);
            break;
        }
        // A document ends a path: it is reported, never expanded.
        frontier = cand
            .iter()
            .filter(|(_, p)| !DOC_LINK_KINDS.contains(&p.kind.as_str()))
            .map(|(&id, _)| id)
            .collect();
        settled.extend(cand);
        if settled.len() - seed_ids.len() > STOP_FACTOR * limit {
            stopped = true;
            break;
        }
    }

    // Hits: every settled node but the seeds; tests leave the groups.
    let hit_ids: Vec<Id> = settled
        .iter()
        .filter(|(_, p)| p.depth > 0)
        .map(|(&id, _)| id)
        .collect();
    let flags = test_flags(tx, &hit_ids)?;
    let candidates: Vec<Id> = hit_ids
        .iter()
        .zip(&flags)
        .filter(|(_, t)| !**t)
        .map(|(&id, _)| id)
        .collect();
    let mut hits: Vec<(Id, SymbolHit)> = Vec::new();
    for (id, row) in candidates.iter().zip(tx.symbols_many(&candidates)?) {
        let node = match row {
            Some(r) => Some(hit(&r)),
            None => referrer(tx, *id, None)?,
        };
        if let Some(n) = node {
            hits.push((*id, n));
        }
    }
    hits.sort_by(|(ia, a), (ib, b)| {
        let (pa, pb) = (&settled[ia], &settled[ib]);
        pa.depth
            .cmp(&pb.depth)
            .then(pb.strength.cmp(&pa.strength))
            .then(pb.score.total_cmp(&pa.score))
            .then_with(|| a.name.cmp(&b.name))
    });
    let total = hits.len();
    let cut = total > limit;
    hits.truncate(limit);

    // Groups keyed by (confidence, kind).
    let mut grouped: BTreeMap<(std::cmp::Reverse<u8>, String), Vec<ImpactHit>> = BTreeMap::new();
    let mut repos: BTreeSet<String> = BTreeSet::new();
    for s in &seed_list {
        repos.extend(repo_of(&s.node.name));
    }
    let mut names: BTreeMap<Id, String> = BTreeMap::new();
    let mut name_of = |id: Id| -> Result<String, StoreError> {
        if let Some(n) = names.get(&id) {
            return Ok(n.clone());
        }
        let n = canon(tx, id)?;
        names.insert(id, n.clone());
        Ok(n)
    };
    for (id, node) in &hits {
        let p = &settled[id];
        let mut chain = Vec::new();
        let mut at = p.parent;
        while let Some(a) = at {
            let up = &settled[&a];
            if up.parent.is_some() {
                chain.push(name_of(a)?);
            }
            at = up.parent;
        }
        repos.extend(repo_of(&node.name));
        grouped
            .entry((std::cmp::Reverse(p.strength), p.kind.clone()))
            .or_default()
            .push(ImpactHit {
                node: node.clone(),
                depth: p.depth,
                score: (p.score * 1e4).round() / 1e4,
                seed: name_of(p.seed)?,
                chain,
                via: p.via.clone(),
            });
    }
    let groups: Vec<ImpactGroup> = grouped
        .into_iter()
        .map(|((s, kind), hits)| ImpactGroup {
            kind,
            confidence: from_strength(s.0),
            hits,
        })
        .collect();

    let mut partial = Vec::new();
    if cut {
        partial.push(Partial::new(
            cap::LIMIT,
            limit as u64,
            limit as u64,
            (!stopped).then_some(total as u64),
        ));
    }
    if deeper {
        partial.push(Partial::new(
            cap::DEPTH,
            u64::from(depth),
            hits.len() as u64,
            None,
        ));
    }

    let mut tests = tests_for(
        tx,
        &seed_ids,
        TestsOptions {
            depth: opts.test_depth,
            cases_only: false,
        },
    )?;
    if tests.len() > limit {
        partial.push(Partial::new(
            cap::LIMIT,
            limit as u64,
            limit as u64,
            Some(tests.len() as u64),
        ));
        tests.truncate(limit);
    }

    let mut contracts = Vec::new();
    for id in reached {
        if let Some(c) = tx.contract(id)? {
            let open = if c.consumers == 0 {
                Some("no-consumer")
            } else if c.providers == 0 {
                Some("no-provider")
            } else {
                None
            };
            contracts.push(ImpactContract {
                contract: c.name.canonical(),
                providers: u64::from(c.providers),
                consumers: u64::from(c.consumers),
                open: open.map(str::to_string),
            });
        }
    }
    contracts.sort_by(|a, b| a.contract.cmp(&b.contract));

    Ok(ImpactReport {
        seeds: seed_list,
        groups,
        total: (!stopped).then_some(total as u64),
        tests,
        contracts,
        repos: repos.into_iter().collect(),
        projects: Vec::new(),
        partial,
    })
}
