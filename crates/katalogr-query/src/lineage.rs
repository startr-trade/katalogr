//! `lineage`: what a type is and where it goes (FEAT-101).
//!
//! A breadth-first walk by contract hops over one snapshot. It follows any link kind into the
//! subject, climbs `contains` to find the holder of a reference, passes through each contract
//! to the other side, and stops at code. It reads fields, related types and copies, and says
//! where the answer stops. The core names no language, format or protocol: a binding is shown
//! by its link kind, its node kind and the contracts it passes through.

use crate::dangling::referrer;
use crate::doc_impact::DOC_LINK_KINDS;
use crate::fold::{Folded, fold};
use crate::impact::ImpactContract;
use crate::resolve::{CONTRACT_KIND, hit};
use crate::result::{ConfidenceSchema, Fold, Partial, SymbolHit, cap};
use crate::tests::{canon, from_strength, strength};
use katalogr_model::insight::FACT_TEST;
use katalogr_model::structure::{FIELD_TYPE, MANY, REQUIRED};
use katalogr_model::{AnyName, Confidence, ExportKey, Fact, Id, LinkKind, Name, parse_name};
use katalogr_store::{LinkKey, Reader, StoreError, SymbolRow};
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Default contract hops the walk follows.
pub const DEFAULT_LINEAGE_DEPTH: u32 = 3;
/// Default most hits in all groups.
pub const DEFAULT_LINEAGE_LIMIT: usize = 200;
/// Default most hits listed per group.
pub const DEFAULT_LINEAGE_PER_GROUP: usize = 10;
/// Link kinds never followed or reported as bindings. `contains` is read for fields and holders.
pub const LINEAGE_SKIPPED_KINDS: &[&str] = &[
    "annotated-by",
    "changed-by",
    "contains",
    "documented-by",
    "refs-item",
    "touches",
    "verified-by",
];
const MAX_HOLDER_DEPTH: usize = 16;
/// The walk stops once this many times the limit of hits is settled.
const STOP_FACTOR: usize = 4;

const PROVIDES: &str = "provides";
const CONSUMES: &str = "consumes";
const DECLARES: &str = "declares";
const ROLES: [&str; 3] = [PROVIDES, CONSUMES, DECLARES];
const CONTAINS: &str = "contains";
const EXTENDS: &str = "extends";
const WIRE_PREFIX: &str = "wire_";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineageOptions {
    /// Contract hops, at least 1.
    pub depth: u32,
    /// Most hits in all groups, at least 1.
    pub limit: usize,
    /// Most hits listed per group, at least 1.
    pub per_group: usize,
    /// Extension id to code kinds, as `DeadCodeOptions::code_kinds`. The facade fills it.
    pub code_kinds: BTreeMap<String, BTreeSet<String>>,
    /// `FOLD_FROM`, or None under `--expand`, which also sets `per_group` to `limit`.
    pub fold_from: Option<u32>,
}

impl Default for LineageOptions {
    fn default() -> Self {
        LineageOptions {
            depth: DEFAULT_LINEAGE_DEPTH,
            limit: DEFAULT_LINEAGE_LIMIT,
            per_group: DEFAULT_LINEAGE_PER_GROUP,
            code_kinds: BTreeMap::new(),
            fold_from: Some(crate::fold::FOLD_FROM),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct LineageReport {
    pub subject: SymbolHit,
    /// The enclosing type, when the subject is a field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<SymbolHit>,
    /// Canonical names of the contracts the subject and its owner bind, sorted.
    pub subject_contracts: Vec<String>,
    /// Sorted by name.
    pub declared_in: Vec<SymbolHit>,
    /// Depth first, in name order. Of the first declarer by name; other copies pair into `copies`.
    pub fields: Vec<LineageField>,
    /// Fields of other copies that paired with none, sorted by name.
    pub unpaired_fields: Vec<SymbolHit>,
    /// Sorted by relation, then name.
    pub related: Vec<LineageRelated>,
    /// Non-code hits, sorted by depth, confidence (strongest first), kind, node kind.
    pub bindings: Vec<LineageGroup>,
    /// Code hits, in the same order.
    pub code: Vec<LineageGroup>,
    /// Each contract reached, with counts and `open`. Sorted by name.
    pub contracts: Vec<ImpactContract>,
    /// Boundary codes, sorted.
    pub boundaries: Vec<String>,
    /// All hits before the limit; absent when the walk stopped early.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// The repos of the subject and hits, sorted.
    pub repos: Vec<String>,
    /// Set by the facade.
    pub projects: Vec<String>,
    pub partial: Vec<Partial>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct LineageField {
    pub node: SymbolHit,
    /// 1 for a direct member.
    pub depth: u32,
    pub field_type: Option<String>,
    pub required: Option<bool>,
    pub many: Option<bool>,
    /// Symbols with a link into this field, kind not skipped, up to `per_group`, sorted by name.
    pub mapped_by: Vec<LineageMapping>,
    pub mapped_total: u64,
    /// The matching field in each other copy of the subject's contract, by name, sorted by name.
    pub copies: Vec<LineageFieldCopy>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct LineageFieldCopy {
    pub node: SymbolHit,
    /// `name` or `normalised-name`. Always Inferred.
    pub by: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct LineageMapping {
    pub node: SymbolHit,
    pub kind: String,
    #[schemars(with = "ConfidenceSchema")]
    pub confidence: Confidence,
    /// String-valued facts of `node` whose key starts `wire_`, sorted. Empty for most.
    pub facts: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct LineageRelated {
    /// `uses`, `used-by`, `extends` or `extended-by`.
    pub relation: String,
    pub node: SymbolHit,
    /// The field that carries the link, when one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub through_field: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct LineageGroup {
    /// Link kind or role that reached the hit.
    pub kind: String,
    pub node_kind: String,
    #[schemars(with = "ConfidenceSchema")]
    pub confidence: Confidence,
    /// Hits in this group before `per_group`.
    pub total: u64,
    pub hits: Vec<LineageHit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct LineageHit {
    pub node: SymbolHit,
    /// Contract hops from the subject.
    pub depth: u32,
    /// The nearest ancestor by `contains` that binds a contract, when it isn't `node`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<String>,
    /// The contract it was reached through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// Nodes between the hit and the subject, hit side first, contracts excluded.
    pub chain: Vec<String>,
    /// `test` for a test, as `role_of` says. Code groups only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LineageAnswer {
    pub report: LineageReport,
    pub folded: Vec<Fold>,
}

/// What a node reached by the walk is.
#[derive(Clone)]
struct Info {
    hit: SymbolHit,
    is_field: bool,
    code: bool,
    test: Option<String>,
}

/// The best path found to a node.
#[derive(Clone)]
struct Path {
    depth: u32,
    strength: u8,
    parent: Option<Id>,
    /// The link kind or role that reached the node (empty for a seed).
    kind: String,
    via: Option<String>,
    /// The single link of a direct step, for folding.
    link: Option<LinkKey>,
    /// A hit to report; a holder joins the walk without being reported.
    reported: bool,
}

/// One step from a node before it is settled.
struct Offer {
    to: Id,
    parent: Option<Id>,
    kind: String,
    via: Option<String>,
    links: Vec<LinkKey>,
    /// A direct in-link (same depth); else a step through a contract (one hop deeper).
    direct: bool,
}

struct Walk<'a, R: Reader> {
    tx: &'a R,
    opts: &'a LineageOptions,
    names: BTreeMap<Id, String>,
    infos: BTreeMap<Id, Option<Info>>,
    exts: BTreeMap<Id, String>,
    holders: BTreeMap<Id, Option<Id>>,
    ends: BTreeMap<Id, Vec<LinkKey>>,
}

fn is_field_row(row: &SymbolRow) -> bool {
    row.facts.contains_key(FIELD_TYPE)
}

fn repo_of(name: &str) -> Option<String> {
    match parse_name(name).ok()? {
        AnyName::Artifact(a) => Some(a.repo),
        AnyName::Symbol(s) => Some(s.artifact.repo),
        AnyName::Contract(_) => None,
    }
}

impl<'a, R: Reader> Walk<'a, R> {
    fn name(&mut self, id: Id) -> Result<String, StoreError> {
        if let Some(n) = self.names.get(&id) {
            return Ok(n.clone());
        }
        let n = canon(self.tx, id)?;
        self.names.insert(id, n.clone());
        Ok(n)
    }

    fn extension(&mut self, artifact: Id) -> Result<String, StoreError> {
        if let Some(e) = self.exts.get(&artifact) {
            return Ok(e.clone());
        }
        let e = self
            .tx
            .artifact(artifact)?
            .map_or(String::new(), |a| a.extension);
        self.exts.insert(artifact, e.clone());
        Ok(e)
    }

    fn is_code(&mut self, row: &SymbolRow) -> Result<bool, StoreError> {
        if self.opts.code_kinds.is_empty() {
            return Ok(false);
        }
        let ext = self.extension(row.artifact)?;
        Ok(self
            .opts
            .code_kinds
            .get(&ext)
            .is_some_and(|k| k.contains(&row.kind)))
    }

    /// What each id is, read once. An id that is neither symbol nor artifact has no info.
    fn load_infos(&mut self, ids: &[Id]) -> Result<(), StoreError> {
        let todo: Vec<Id> = ids
            .iter()
            .copied()
            .filter(|i| !self.infos.contains_key(i))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if todo.is_empty() {
            return Ok(());
        }
        let rows = self.tx.symbols_many(&todo)?;
        for (id, row) in todo.into_iter().zip(rows) {
            let info = match row {
                Some(r) => Some(Info {
                    is_field: is_field_row(&r),
                    code: self.is_code(&r)?,
                    test: match r.facts.get(FACT_TEST) {
                        Some(Fact::Str(s)) => Some(s.clone()),
                        _ => None,
                    },
                    hit: hit(&r),
                }),
                None => referrer(self.tx, id, None)?.map(|h| Info {
                    hit: h,
                    is_field: false,
                    code: false,
                    test: crate::tests::role_of(self.tx, id)
                        .ok()
                        .flatten()
                        .map(|(r, _)| r),
                }),
            };
            self.infos.insert(id, info);
        }
        Ok(())
    }

    /// The nearest ancestor by `contains` that provides, consumes or declares a contract.
    fn holder_of(&mut self, id: Id) -> Result<Option<Id>, StoreError> {
        if let Some(h) = self.holders.get(&id) {
            return Ok(*h);
        }
        let mut found = None;
        let mut at = id;
        for _ in 0..MAX_HOLDER_DEPTH {
            let Some(parent) = self.parent_of(at)? else {
                break;
            };
            if !self.contracts_of(parent)?.is_empty() {
                found = Some(parent);
                break;
            }
            at = parent;
        }
        self.holders.insert(id, found);
        Ok(found)
    }

    /// The source of the `contains` link into `id` (the smallest, when several).
    fn parent_of(&self, id: Id) -> Result<Option<Id>, StoreError> {
        Ok(self
            .tx
            .links_in(id)?
            .into_iter()
            .filter(|k| k.kind.0 == CONTAINS)
            .map(|k| k.from)
            .min())
    }

    /// The links from `id` to contract nodes that exist, by role.
    fn contracts_of(&self, id: Id) -> Result<Vec<LinkKey>, StoreError> {
        let mut out = Vec::new();
        for role in ROLES {
            for k in self.tx.link_keys_out(id, &LinkKind(role.to_string()))? {
                if self.tx.contract(k.to)?.is_some() {
                    out.push(k);
                }
            }
        }
        Ok(out)
    }

    /// `better(new, old)` for two paths to the same node at the same depth.
    fn better(&mut self, new: &Path, old: &Path) -> Result<bool, StoreError> {
        if new.strength != old.strength {
            return Ok(new.strength > old.strength);
        }
        let pn = match new.parent {
            Some(p) => self.name(p)?,
            None => String::new(),
        };
        let po = match old.parent {
            Some(p) => self.name(p)?,
            None => String::new(),
        };
        Ok((&pn, &new.kind, &new.via) < (&po, &old.kind, &old.via))
    }

    fn ends_of(&mut self, contracts: &[Id]) -> Result<(), StoreError> {
        let todo: Vec<Id> = contracts
            .iter()
            .copied()
            .filter(|c| !self.ends.contains_key(c))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if todo.is_empty() {
            return Ok(());
        }
        for (c, keys) in todo.iter().zip(self.tx.links_in_many(&todo)?) {
            let mut roles: Vec<LinkKey> = keys
                .into_iter()
                .filter(|k| ROLES.contains(&k.kind.0.as_str()))
                .collect();
            roles.sort_by(|a, b| (&a.kind, a.from).cmp(&(&b.kind, b.from)));
            self.ends.insert(*c, roles);
        }
        Ok(())
    }
}

/// The links a hit crossed, as the weakest confidence.
fn link_strength(tx: &impl Reader, keys: &[LinkKey]) -> Result<Vec<u8>, StoreError> {
    let briefs = tx.link_briefs_many(keys)?;
    briefs
        .into_iter()
        .map(|b| {
            b.map(|b| strength(b.confidence))
                .ok_or_else(|| StoreError::Row("link without a row".into()))
        })
        .collect()
}

/// A hit before grouping.
struct Pre {
    id: Id,
    node: SymbolHit,
    depth: u32,
    strength: u8,
    kind: String,
    holder: Option<String>,
    via: Option<String>,
    chain: Vec<String>,
    role: Option<String>,
    code: bool,
}

/// The walk over one snapshot. Pure and deterministic, with no write. Errors only from the store.
pub fn lineage(
    tx: &impl Reader,
    target: &SymbolHit,
    opts: &LineageOptions,
) -> Result<LineageAnswer, StoreError> {
    let depth = opts.depth.max(1);
    let limit = opts.limit.max(1);
    let per_group = opts.per_group.max(1);
    let mut w = Walk {
        tx,
        opts,
        names: BTreeMap::new(),
        infos: BTreeMap::new(),
        exts: BTreeMap::new(),
        holders: BTreeMap::new(),
        ends: BTreeMap::new(),
    };
    let target_id = Id::of(&target.name);

    // ---- the subject: its contracts, owner and declarers ----
    let contract_target = target.kind == CONTRACT_KIND && tx.contract(target_id)?.is_some();
    let subject_row = if contract_target {
        None
    } else {
        tx.symbol(target_id)?
    };
    let subject_is_field = subject_row.as_ref().is_some_and(is_field_row);
    let mut owner_id: Option<Id> = None;
    if subject_is_field {
        let mut at = target_id;
        for _ in 0..MAX_HOLDER_DEPTH {
            let Some(p) = w.parent_of(at)? else { break };
            match tx.symbol(p)? {
                Some(r) if is_field_row(&r) => at = p,
                Some(_) => {
                    owner_id = Some(p);
                    break;
                }
                None => break,
            }
        }
    }
    let owner = match owner_id {
        Some(o) => tx.symbol(o)?.map(|r| hit(&r)),
        None => None,
    };

    // The contracts bound by the subject and its owner, and those they declare.
    let mut bound: BTreeSet<Id> = BTreeSet::new();
    let mut declared: BTreeSet<Id> = BTreeSet::new();
    if contract_target {
        bound.insert(target_id);
        declared.insert(target_id);
    }
    for n in [(!contract_target).then_some(target_id), owner_id]
        .into_iter()
        .flatten()
    {
        for k in w.contracts_of(n)? {
            bound.insert(k.to);
            if k.kind.0 == DECLARES {
                declared.insert(k.to);
            }
        }
    }
    let mut subject_contracts: Vec<String> = Vec::new();
    for c in &bound {
        subject_contracts.push(w.name(*c)?);
    }
    subject_contracts.sort();
    let declared_vec: Vec<Id> = declared.iter().copied().collect();
    w.ends_of(&declared_vec)?;
    let mut declarers: BTreeSet<Id> = BTreeSet::new();
    for c in &declared_vec {
        declarers.extend(
            w.ends[c]
                .iter()
                .filter(|k| k.kind.0 == DECLARES)
                .map(|k| k.from),
        );
    }
    let declarer_ids: Vec<Id> = declarers.iter().copied().collect();
    let mut declared_in: Vec<SymbolHit> = Vec::new();
    for (_, row) in declarer_ids.iter().zip(tx.symbols_many(&declarer_ids)?) {
        if let Some(r) = row {
            declared_in.push(hit(&r));
        }
    }
    declared_in.sort_by(|a, b| a.name.cmp(&b.name));

    // ---- fields: of the first declarer by name, else the subject (or its owner) ----
    let field_root: Option<Id> = declared_in
        .first()
        .map(|h| Id::of(&h.name))
        .or(owner_id)
        .or_else(|| subject_row.as_ref().map(|_| target_id));
    let mut fields = match field_root {
        Some(root) => fields_of(tx, root)?,
        None => Vec::new(),
    };
    let mut partial: Vec<Partial> = Vec::new();
    let (mut cut_shown, mut cut_total, mut cut_any) = (0u64, 0u64, false);
    fill_mappings(
        tx,
        &mut fields,
        per_group,
        &mut cut_shown,
        &mut cut_total,
        &mut cut_any,
    )?;
    // Copies of one contract pair their fields by name, at read.
    let mut unpaired_fields: Vec<SymbolHit> = Vec::new();
    if declared_in.len() >= 2 && field_root == declared_in.first().map(|h| Id::of(&h.name)) {
        for other in &declared_in[1..] {
            let theirs = fields_of(tx, Id::of(&other.name))?;
            pair_fields(&mut fields, theirs, &mut unpaired_fields);
        }
        unpaired_fields.sort_by(|a, b| a.name.cmp(&b.name));
        for f in &mut fields {
            f.copies.sort_by(|a, b| a.node.name.cmp(&b.node.name));
            if f.copies.len() > per_group {
                cut_any = true;
                cut_total += f.copies.len() as u64;
                cut_shown += per_group as u64;
                f.copies.truncate(per_group);
            }
        }
        if unpaired_fields.len() > per_group {
            cut_any = true;
            cut_total += unpaired_fields.len() as u64;
            cut_shown += per_group as u64;
            unpaired_fields.truncate(per_group);
        }
    }

    // ---- seeds ----
    let mut seeds: BTreeSet<Id> = declarers.clone();
    if !contract_target {
        seeds.insert(target_id);
    }
    seeds.extend(owner_id);
    let mut settled: BTreeMap<Id, Path> = seeds
        .iter()
        .map(|&s| {
            (
                s,
                Path {
                    depth: 0,
                    strength: 3,
                    parent: None,
                    kind: String::new(),
                    via: None,
                    link: None,
                    reported: false,
                },
            )
        })
        .collect();
    let mut reached: BTreeSet<Id> = bound.clone();
    let mut related: BTreeSet<(String, String, Option<String>)> = BTreeSet::new();
    let mut related_nodes: BTreeMap<String, SymbolHit> = BTreeMap::new();

    // Related, outward: what the subject and its fields use and extend.
    {
        let mut sources: Vec<(Id, Option<String>)> = Vec::new();
        for n in [(!contract_target).then_some(target_id), owner_id]
            .into_iter()
            .flatten()
        {
            sources.push((n, None));
        }
        for f in &fields {
            sources.push((Id::of(&f.node.name), Some(leaf_of_name(&f.node.name))));
        }
        // The subject's own fields, when the fields listed are another copy's.
        if !contract_target && field_root != Some(target_id) {
            let listed: BTreeSet<Id> = fields.iter().map(|f| Id::of(&f.node.name)).collect();
            for f in fields_of(tx, target_id)? {
                let id = Id::of(&f.node.name);
                if !listed.contains(&id) {
                    sources.push((id, Some(leaf_of_name(&f.node.name))));
                }
            }
        }
        for (from, through) in sources {
            for (key, _) in tx.links_out(from, None)? {
                let relation = if key.kind.0 == EXTENDS {
                    "extends"
                } else if key.kind.0.starts_with("uses-") {
                    "uses"
                } else {
                    continue;
                };
                if let Some(row) = tx.symbol(key.to)? {
                    let h = hit(&row);
                    related.insert((relation.to_string(), h.name.clone(), through.clone()));
                    related_nodes.insert(h.name.clone(), h);
                }
            }
        }
    }

    // ---- the walk ----
    let mut level: BTreeSet<Id> = seeds.clone();
    let mut contract_seed_offers: Vec<Offer> = Vec::new();
    if contract_target {
        w.ends_of(&[target_id])?;
        let cname = w.name(target_id)?;
        for end in w.ends[&target_id].clone() {
            if seeds.contains(&end.from) {
                continue;
            }
            contract_seed_offers.push(Offer {
                to: end.from,
                parent: None,
                kind: end.kind.0.clone(),
                via: Some(cname.clone()),
                links: vec![end],
                direct: false,
            });
        }
    }
    let mut stopped = false;
    let mut deeper = false;
    for d in 0..=depth {
        let mut work: BTreeSet<Id> = level.clone();
        let mut next: BTreeMap<Id, Path> = BTreeMap::new();
        let mut pending: Vec<Offer> = if d == 0 {
            std::mem::take(&mut contract_seed_offers)
        } else {
            Vec::new()
        };
        loop {
            let wave: Vec<Id> = std::mem::take(&mut work).into_iter().collect();
            let mut offers = std::mem::take(&mut pending);
            offers.extend(expand(&mut w, &wave, &settled, &mut reached)?);
            if offers.is_empty() {
                break;
            }
            offers.sort_by(|a, b| {
                (a.to, a.parent, &a.kind, &a.via, a.direct)
                    .cmp(&(b.to, b.parent, &b.kind, &b.via, b.direct))
            });
            let ids: Vec<Id> = offers.iter().map(|o| o.to).collect();
            w.load_infos(&ids)?;
            let all_links: Vec<LinkKey> = offers
                .iter()
                .flat_map(|o| o.links.iter().cloned())
                .collect();
            let mut strengths = link_strength(tx, &all_links)?.into_iter();
            for o in offers {
                let mut s = match o.parent {
                    Some(p) => settled.get(&p).map_or(3, |p| p.strength),
                    None => 3,
                };
                for _ in &o.links {
                    s = s.min(strengths.next().unwrap_or(1));
                }
                let Some(info) = w.infos.get(&o.to).cloned().flatten() else {
                    continue;
                };
                let target_depth = if o.direct { d } else { d + 1 };
                if o.direct && d == 0 && (info.is_field || o.kind == EXTENDS) {
                    // A field that uses or extends the subject, or a type that extends it,
                    // is a related type; it is not walked.
                    if info.is_field || o.kind == EXTENDS {
                        let relation = if o.kind == EXTENDS {
                            "extended-by"
                        } else {
                            "used-by"
                        };
                        if info.is_field {
                            let owner_hit = {
                                let mut at = o.to;
                                let mut found = None;
                                for _ in 0..MAX_HOLDER_DEPTH {
                                    let Some(p) = w.parent_of(at)? else { break };
                                    match tx.symbol(p)? {
                                        Some(r) if is_field_row(&r) => at = p,
                                        Some(r) => {
                                            found = Some(hit(&r));
                                            break;
                                        }
                                        None => break,
                                    }
                                }
                                found
                            };
                            if let Some(oh) = owner_hit {
                                let through = leaf_of_name(&info.hit.name);
                                related.insert((
                                    relation.to_string(),
                                    oh.name.clone(),
                                    Some(through),
                                ));
                                related_nodes.insert(oh.name.clone(), oh);
                            }
                        } else {
                            related.insert((relation.to_string(), info.hit.name.clone(), None));
                            related_nodes.insert(info.hit.name.clone(), info.hit.clone());
                        }
                        continue;
                    }
                }
                if o.direct && info.is_field {
                    continue;
                }
                if target_depth > depth {
                    if info.test.is_none() && !settled.contains_key(&o.to) {
                        deeper = true;
                    }
                    continue;
                }
                let via = if o.direct {
                    o.via.clone().or_else(|| {
                        o.parent
                            .and_then(|p| settled.get(&p))
                            .and_then(|p| p.via.clone())
                    })
                } else {
                    o.via.clone()
                };
                let path = Path {
                    depth: target_depth,
                    strength: s,
                    parent: o.parent,
                    kind: o.kind.clone(),
                    via,
                    link: if o.direct && o.links.len() == 1 {
                        o.links.first().cloned()
                    } else {
                        None
                    },
                    reported: true,
                };
                if o.direct {
                    let accept = match settled.get(&o.to) {
                        None => true,
                        Some(old) if old.parent.is_none() && old.kind.is_empty() => false,
                        Some(old) if old.depth < d => false,
                        Some(old) if !old.reported => true,
                        Some(old) => w.better(&path, old)?,
                    };
                    if !accept
                        || (settled.contains_key(&o.to) && descends(&settled, o.parent, o.to))
                    {
                        continue;
                    }
                    let continues = !info.code && !DOC_LINK_KINDS.contains(&o.kind.as_str());
                    settled.insert(o.to, path);
                    if continues {
                        work.insert(o.to);
                        if let Some(h) = w.holder_of(o.to)?
                            && !settled.contains_key(&h)
                        {
                            settled.insert(
                                h,
                                Path {
                                    depth: d,
                                    strength: s,
                                    parent: Some(o.to),
                                    kind: CONTAINS.to_string(),
                                    via: None,
                                    link: None,
                                    reported: false,
                                },
                            );
                            work.insert(h);
                        }
                    }
                } else {
                    if settled.contains_key(&o.to) {
                        continue;
                    }
                    let better = match next.get(&o.to) {
                        None => true,
                        Some(old) => w.better(&path, old)?,
                    };
                    if better {
                        next.insert(o.to, path);
                    }
                }
            }
            if work.is_empty() {
                break;
            }
        }
        // The next level: the nodes reached through contracts.
        level = BTreeSet::new();
        for (id, path) in next {
            if settled.contains_key(&id) {
                continue;
            }
            let info = w.infos.get(&id).cloned().flatten();
            let continues = info.as_ref().is_some_and(|i| !i.code);
            let s = path.strength;
            settled.insert(id, path);
            if continues {
                level.insert(id);
                if let Some(h) = w.holder_of(id)?
                    && !settled.contains_key(&h)
                {
                    settled.insert(
                        h,
                        Path {
                            depth: d + 1,
                            strength: s,
                            parent: Some(id),
                            kind: CONTAINS.to_string(),
                            via: None,
                            link: None,
                            reported: false,
                        },
                    );
                    level.insert(h);
                }
            }
        }
        let reported = settled.values().filter(|p| p.reported).count();
        if reported > STOP_FACTOR * limit {
            stopped = true;
            break;
        }
        if level.is_empty() && d < depth {
            // Nothing deeper to expand; the probe below has nothing to find either.
            break;
        }
    }

    // ---- hits ----
    let hit_ids: Vec<Id> = settled
        .iter()
        .filter(|(_, p)| p.reported)
        .map(|(&id, _)| id)
        .collect();
    w.load_infos(&hit_ids)?;
    let mut pres: Vec<Pre> = Vec::new();
    for id in hit_ids {
        let Some(info) = w.infos.get(&id).cloned().flatten() else {
            continue;
        };
        let p = settled[&id].clone();
        let holder = match w.holder_of(id)? {
            Some(h) => Some(w.name(h)?),
            None => None,
        };
        let mut chain = Vec::new();
        // A better path found later at the same depth can make two nodes each other's parent
        // on a cyclic graph; the walk of parents visits each node once.
        let mut on_chain: BTreeSet<Id> = BTreeSet::from([id]);
        let mut at = p.parent;
        while let Some(a) = at {
            if !on_chain.insert(a) {
                break;
            }
            let Some(up) = settled.get(&a) else { break };
            if !seeds.contains(&a) {
                chain.push(w.name(a)?);
            }
            at = up.parent;
        }
        pres.push(Pre {
            id,
            node: info.hit.clone(),
            depth: p.depth,
            strength: p.strength,
            kind: p.kind.clone(),
            holder: holder.filter(|h| *h != info.hit.name),
            via: p.via.clone(),
            chain,
            role: if info.code { info.test.clone() } else { None },
            code: info.code,
        });
    }
    pres.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then(b.strength.cmp(&a.strength))
            .then_with(|| a.node.name.cmp(&b.node.name))
            .then_with(|| a.kind.cmp(&b.kind))
    });
    let total = pres.len();
    let limited = total > limit;
    pres.truncate(limit);
    if limited {
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
            pres.len() as u64,
            None,
        ));
    }

    // Folds: a hit that arrived by an inferred fan.
    let folded: Folded<Pre> = match opts.fold_from {
        None => Folded {
            hits: pres,
            folds: Vec::new(),
        },
        Some(from) => {
            let keyed: Vec<(Pre, Option<LinkKey>)> = pres
                .into_iter()
                .map(|p| {
                    let link = settled[&p.id].link.clone();
                    (p, link)
                })
                .collect();
            fold(
                tx,
                keyed,
                from,
                |h: &Pre, k: &ExportKey| (h.depth, h.kind.clone(), k.clone()),
                |h| h.node.clone(),
                |g, f| {
                    f.depth = Some(g.0);
                    f.kind = g.1.clone();
                },
            )?
        }
    };

    // Groups.
    type GroupKey = (u32, std::cmp::Reverse<u8>, String, String);
    let mut bindings: BTreeMap<GroupKey, Vec<LineageHit>> = BTreeMap::new();
    let mut code: BTreeMap<GroupKey, Vec<LineageHit>> = BTreeMap::new();
    let mut repos: BTreeSet<String> = BTreeSet::new();
    repos.extend(repo_of(&target.name));
    for h in &folded.hits {
        repos.extend(repo_of(&h.node.name));
    }
    // Group order is by the shallowest hit; hits are already in depth order.
    let mut group_depth: BTreeMap<(std::cmp::Reverse<u8>, String, String, bool), u32> =
        BTreeMap::new();
    for p in &folded.hits {
        let k = (
            std::cmp::Reverse(p.strength),
            p.kind.clone(),
            p.node.kind.clone(),
            p.code,
        );
        group_depth.entry(k).or_insert(p.depth);
    }
    for p in folded.hits {
        let gd = group_depth[&(
            std::cmp::Reverse(p.strength),
            p.kind.clone(),
            p.node.kind.clone(),
            p.code,
        )];
        let key: GroupKey = (
            gd,
            std::cmp::Reverse(p.strength),
            p.kind,
            p.node.kind.clone(),
        );
        let lh = LineageHit {
            node: p.node,
            depth: p.depth,
            holder: p.holder,
            via: p.via,
            chain: p.chain,
            role: p.role,
        };
        if p.code {
            code.entry(key).or_default().push(lh);
        } else {
            bindings.entry(key).or_default().push(lh);
        }
    }
    let mut finish = |m: BTreeMap<GroupKey, Vec<LineageHit>>| -> Vec<LineageGroup> {
        m.into_iter()
            .map(|((_, s, kind, node_kind), mut hits)| {
                let total = hits.len() as u64;
                if hits.len() > per_group {
                    cut_any = true;
                    cut_total += total;
                    cut_shown += per_group as u64;
                    hits.truncate(per_group);
                }
                LineageGroup {
                    kind,
                    node_kind,
                    confidence: from_strength(s.0),
                    total,
                    hits,
                }
            })
            .collect()
    };
    let bindings = finish(bindings);
    let code = finish(code);
    if cut_any {
        partial.push(Partial::new(
            cap::PER_GROUP,
            per_group as u64,
            cut_shown,
            Some(cut_total),
        ));
    }
    for g in bindings.iter().chain(&code) {
        for h in &g.hits {
            repos.extend(repo_of(&h.node.name));
        }
    }

    // Contracts reached.
    let mut contracts = Vec::new();
    for id in &reached {
        if let Some(c) = tx.contract(*id)? {
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

    // Related, sorted by relation then name.
    let related: Vec<LineageRelated> = related
        .into_iter()
        .filter_map(|(relation, name, through_field)| {
            Some(LineageRelated {
                relation,
                node: related_nodes.get(&name)?.clone(),
                through_field,
            })
        })
        .collect();

    // Boundaries.
    let mut boundaries: BTreeSet<&str> = BTreeSet::new();
    if subject_contracts.is_empty() {
        boundaries.insert("no-contract");
    }
    if fields.is_empty() {
        boundaries.insert("no-fields");
    }
    if owner.is_some() {
        boundaries.insert("owner-level");
    }
    if code.is_empty() {
        boundaries.insert("no-code");
    }
    if contracts.iter().any(|c| c.open.is_some()) {
        boundaries.insert("open-contract");
    }

    Ok(LineageAnswer {
        report: LineageReport {
            subject: target.clone(),
            owner,
            subject_contracts,
            declared_in,
            fields,
            unpaired_fields,
            related,
            bindings,
            code,
            contracts,
            boundaries: boundaries.into_iter().map(str::to_string).collect(),
            total: (!stopped).then_some(total as u64),
            repos: repos.into_iter().collect(),
            projects: Vec::new(),
            partial,
        },
        folded: folded.folds,
    })
}

fn leaf_of_name(name: &str) -> String {
    let body = name.split_once('#').map_or(name, |(_, b)| b);
    let last = body.rsplit('/').next().unwrap_or("");
    let last = match last.rsplit_once('~') {
        Some((l, n)) if !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()) => l,
        _ => last,
    };
    last.split_once(':').map_or(last, |(_, n)| n).to_string()
}

/// Whether `node` is `from` or one of its ancestors in the settled paths. Taking a better path to
/// a settled node through one of its own descendants would make a parent cycle.
fn descends(settled: &BTreeMap<Id, Path>, from: Option<Id>, node: Id) -> bool {
    let mut seen: BTreeSet<Id> = BTreeSet::new();
    let mut at = from;
    while let Some(a) = at {
        if a == node {
            return true;
        }
        if !seen.insert(a) {
            return false;
        }
        at = settled.get(&a).and_then(|p| p.parent);
    }
    false
}

/// One level of the walk from the nodes in `wave`: every step they can take.
fn expand<R: Reader>(
    w: &mut Walk<'_, R>,
    wave: &[Id],
    settled: &BTreeMap<Id, Path>,
    reached: &mut BTreeSet<Id>,
) -> Result<Vec<Offer>, StoreError> {
    if wave.is_empty() {
        return Ok(Vec::new());
    }
    let tx = w.tx;
    let ins = tx.links_in_many(wave)?;
    let mut own: Vec<Vec<LinkKey>> = vec![Vec::new(); wave.len()];
    for role in ROLES {
        for (i, keys) in tx
            .link_keys_out_many(wave, &LinkKind(role.to_string()))?
            .into_iter()
            .enumerate()
        {
            own[i].extend(keys);
        }
    }
    let mut contract_ids: BTreeSet<Id> = BTreeSet::new();
    for keys in &own {
        contract_ids.extend(keys.iter().map(|k| k.to));
    }
    let mut known: BTreeSet<Id> = BTreeSet::new();
    for c in contract_ids {
        if tx.contract(c)?.is_some() {
            known.insert(c);
        }
    }
    let known_vec: Vec<Id> = known.iter().copied().collect();
    w.ends_of(&known_vec)?;

    let mut out = Vec::new();
    for (i, &n) in wave.iter().enumerate() {
        let n_via = settled.get(&n).and_then(|p| p.via.clone());
        for key in &ins[i] {
            let kind = key.kind.0.as_str();
            if LINEAGE_SKIPPED_KINDS.contains(&kind) || ROLES.contains(&kind) || key.from == n {
                continue;
            }
            out.push(Offer {
                to: key.from,
                parent: Some(n),
                kind: key.kind.0.clone(),
                via: n_via.clone(),
                links: vec![key.clone()],
                direct: true,
            });
        }
        for own_key in own[i].iter().filter(|k| known.contains(&k.to)) {
            reached.insert(own_key.to);
            let cname = w.name(own_key.to)?;
            for end in w.ends[&own_key.to].clone() {
                if end.from == n {
                    continue;
                }
                out.push(Offer {
                    to: end.from,
                    parent: Some(n),
                    kind: end.kind.0.clone(),
                    via: Some(cname.clone()),
                    links: vec![own_key.clone(), end],
                    direct: false,
                });
            }
        }
    }
    Ok(out)
}

/// The fields below `root` through `contains`, depth first in name order. The reads are batched
/// one level at a time.
fn fields_of(tx: &impl Reader, root: Id) -> Result<Vec<LineageField>, StoreError> {
    let contains = LinkKind(CONTAINS.to_string());
    let mut children: BTreeMap<Id, Vec<SymbolRow>> = BTreeMap::new();
    let mut seen: BTreeSet<Id> = BTreeSet::from([root]);
    let mut frontier = vec![root];
    for _ in 0..MAX_HOLDER_DEPTH {
        if frontier.is_empty() {
            break;
        }
        let keys = tx.link_keys_out_many(&frontier, &contains)?;
        let mut ids: Vec<Id> = Vec::new();
        let mut owners: Vec<(Id, Vec<Id>)> = Vec::new();
        for (parent, ks) in frontier.iter().zip(keys) {
            let kids: Vec<Id> = ks
                .into_iter()
                .map(|k| k.to)
                .filter(|t| seen.insert(*t))
                .collect();
            ids.extend(&kids);
            owners.push((*parent, kids));
        }
        let mut rows = tx.symbols_many(&ids)?.into_iter();
        for (parent, kids) in owners {
            let mut v: Vec<SymbolRow> = Vec::new();
            for _ in kids {
                v.extend(rows.next().flatten());
            }
            v.sort_by_cached_key(|r| r.name.canonical());
            children.insert(parent, v);
        }
        frontier = ids;
    }
    let mut out = Vec::new();
    fn walk(
        id: Id,
        field_depth: u32,
        children: &BTreeMap<Id, Vec<SymbolRow>>,
        out: &mut Vec<LineageField>,
    ) {
        let Some(kids) = children.get(&id) else {
            return;
        };
        for row in kids {
            let own = row.name.id();
            if is_field_row(row) {
                let flag = |k: &str| match row.facts.get(k) {
                    Some(Fact::Bool(b)) => Some(*b),
                    _ => None,
                };
                out.push(LineageField {
                    node: hit(row),
                    depth: field_depth + 1,
                    field_type: match row.facts.get(FIELD_TYPE) {
                        Some(Fact::Str(s)) => Some(s.clone()),
                        _ => None,
                    },
                    required: flag(REQUIRED),
                    many: flag(MANY),
                    mapped_by: Vec::new(),
                    mapped_total: 0,
                    copies: Vec::new(),
                });
                walk(own, field_depth + 1, children, out);
            } else {
                walk(own, field_depth, children, out);
            }
        }
    }
    walk(root, 0, &children, &mut out);
    Ok(out)
}

/// `mapped_by`: the symbols with a link into each field, kind not skipped.
fn fill_mappings(
    tx: &impl Reader,
    fields: &mut [LineageField],
    per_group: usize,
    cut_shown: &mut u64,
    cut_total: &mut u64,
    cut_any: &mut bool,
) -> Result<(), StoreError> {
    if fields.is_empty() {
        return Ok(());
    }
    let ids: Vec<Id> = fields.iter().map(|f| Id::of(&f.node.name)).collect();
    let ins = tx.links_in_many(&ids)?;
    for (f, keys) in fields.iter_mut().zip(ins) {
        let keys: Vec<LinkKey> = keys
            .into_iter()
            .filter(|k| {
                !LINEAGE_SKIPPED_KINDS.contains(&k.kind.0.as_str())
                    && !ROLES.contains(&k.kind.0.as_str())
            })
            .collect();
        let srcs: Vec<Id> = keys.iter().map(|k| k.from).collect();
        let rows = tx.symbols_many(&srcs)?;
        let briefs = tx.link_briefs_many(&keys)?;
        let mut maps: Vec<LineageMapping> = Vec::new();
        for ((k, row), brief) in keys.iter().zip(rows).zip(briefs) {
            let (Some(row), Some(brief)) = (row, brief) else {
                continue;
            };
            let facts: BTreeMap<String, String> = row
                .facts
                .iter()
                .filter(|(n, _)| n.starts_with(WIRE_PREFIX))
                .filter_map(|(n, v)| match v {
                    Fact::Str(s) => Some((n.clone(), s.clone())),
                    _ => None,
                })
                .collect();
            maps.push(LineageMapping {
                node: hit(&row),
                kind: k.kind.0.clone(),
                confidence: brief.confidence,
                facts,
            });
        }
        maps.sort_by(|a, b| (&a.node.name, &a.kind).cmp(&(&b.node.name, &b.kind)));
        f.mapped_total = maps.len() as u64;
        if maps.len() > per_group {
            *cut_any = true;
            *cut_total += maps.len() as u64;
            *cut_shown += per_group as u64;
            maps.truncate(per_group);
        }
        f.mapped_by = maps;
    }
    Ok(())
}

/// The names from the first field below the root to this one, by `depth`.
fn field_paths(fields: &[LineageField]) -> Vec<Vec<String>> {
    let mut stack: Vec<String> = Vec::new();
    fields
        .iter()
        .map(|f| {
            stack.truncate(f.depth as usize - 1);
            stack.push(leaf_of_name(&f.node.name));
            stack.clone()
        })
        .collect()
}

/// A name compared across copies: lower case, without `_` and `-`.
fn normalised(path: &[String]) -> Vec<String> {
    path.iter()
        .map(|s| {
            s.chars()
                .filter(|c| *c != '_' && *c != '-')
                .flat_map(char::to_lowercase)
                .collect()
        })
        .collect()
}

/// Pair the fields of another copy with the base fields by path of names: exact first, then
/// normalised, only when the normalised path is unique among both sides' unpaired fields. A
/// pair goes into the base field's `copies` (Inferred, by name); the rest of `theirs` goes to
/// `unpaired`.
fn pair_fields(
    base: &mut [LineageField],
    theirs: Vec<LineageField>,
    unpaired: &mut Vec<SymbolHit>,
) {
    let bp = field_paths(base);
    let tp = field_paths(&theirs);
    let mut taken: Vec<bool> = vec![false; theirs.len()];
    let mut paired: Vec<bool> = vec![false; base.len()];
    for (i, p) in bp.iter().enumerate() {
        if let Some(j) = tp.iter().enumerate().position(|(j, q)| !taken[j] && q == p) {
            taken[j] = true;
            paired[i] = true;
            base[i].copies.push(LineageFieldCopy {
                node: theirs[j].node.clone(),
                by: "name".to_string(),
            });
        }
    }
    let count = |paths: &[Vec<String>], skip: &[bool]| {
        let mut m: BTreeMap<Vec<String>, usize> = BTreeMap::new();
        for (p, s) in paths.iter().zip(skip) {
            if !*s {
                *m.entry(normalised(p)).or_default() += 1;
            }
        }
        m
    };
    let (cb, ct) = (count(&bp, &paired), count(&tp, &taken));
    for (i, p) in bp.iter().enumerate() {
        if paired[i] {
            continue;
        }
        let n = normalised(p);
        if cb.get(&n) != Some(&1) || ct.get(&n) != Some(&1) {
            continue;
        }
        if let Some(j) = tp
            .iter()
            .enumerate()
            .position(|(j, q)| !taken[j] && normalised(q) == n)
        {
            taken[j] = true;
            paired[i] = true;
            base[i].copies.push(LineageFieldCopy {
                node: theirs[j].node.clone(),
                by: "normalised-name".to_string(),
            });
        }
    }
    for (f, t) in theirs.into_iter().zip(taken) {
        if !t {
            unpaired.push(f.node);
        }
    }
}
