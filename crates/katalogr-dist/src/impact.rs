//! `impact`: what a symbol, file, change range or board item reaches (FEAT-028).
//!
//! The facade resolves the input to seeds in one snapshot and runs the query crate's walk. Git
//! runs before the snapshot, so a commit landing between them can only make the answer inexact.

use crate::workspace::{MAX_IMPACT_ROUNDS, OpenPeer};
use crate::workspace_join::{attach, mark_unavailable, snapshots};
use crate::{DistError, Katalogr, Options, Outcome, Reply, unresolved};
use katalogr_model::{AnyName, Confidence, Id, Name, RelPath, parse_name};
use katalogr_query::{
    ImpactContract, ImpactGroup, ImpactHit, ImpactOptions, ImpactReport, Partial, PeerEnd,
    PeerImpact, Resolution, SEED_CONTRACT_CONSUMERS, Seed, TestHit, cap, filter_report,
    peer_contract_ends, seeds_for_changes, seeds_for_file, seeds_for_item, seeds_for_symbol,
};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

/// What an `impact` question starts from.
pub enum ImpactInput<'a> {
    /// A symbol or contract, as `callers` resolves its text.
    Symbol(&'a str),
    /// A canonical artifact name (text with `:`), else a path relative to the marked repo.
    File(&'a str),
    /// A board item code.
    Item(&'a str),
    /// `base..head`, both resolved to commits.
    Range(&'a str),
    /// Changes since a revision or clean position; `None`: the merge-base with the default
    /// branch, up to the working tree.
    Since(Option<&'a str>),
}

/// How the input becomes seeds inside the snapshot.
enum Plan {
    Symbol(String),
    File { repo: String, path: RelPath },
    Item(String),
    Changes(crate::changes::ChangeRange),
}

impl Katalogr {
    /// Resolves the seeds and reads one snapshot. Ambiguous and no match pass through as for
    /// `callers`. A range's head that no context holds is `PositionUnavailable`; `impact`
    /// never queues or indexes to reach a position.
    pub fn impact(
        &self,
        input: &ImpactInput<'_>,
        iopts: &ImpactOptions,
        opts: &Options,
    ) -> Result<Reply<ImpactReport>, DistError> {
        let _extra = self.extra_scope();
        let mut opts = opts.clone();
        let mut base: Option<String> = None;
        let plan = match input {
            ImpactInput::Symbol(t) => Plan::Symbol((*t).to_string()),
            ImpactInput::Item(c) => Plan::Item((*c).to_string()),
            ImpactInput::File(t) => {
                let (repo, path) = if t.contains(':') {
                    match parse_name(t) {
                        Ok(AnyName::Artifact(a)) => (a.repo, a.path),
                        _ => {
                            return Err(DistError::usage(format!(
                                "--file {t}: not an artifact name (<repo>:<path>)"
                            )));
                        }
                    }
                } else {
                    let (repo, _) = self.current()?;
                    let path = RelPath::new(t)
                        .map_err(|e| DistError::usage(format!("--file {t}: {e}")))?;
                    (repo.to_string(), path)
                };
                Plan::File { repo, path }
            }
            ImpactInput::Range(text) => {
                let (range, head) = self.range_changes(text)?;
                opts.at = Some(head);
                base = Some(range.from.clone());
                Plan::Changes(range)
            }
            ImpactInput::Since(since) => {
                let deadline = Instant::now() + opts.timeout()?;
                let range = self.change_range(*since, &opts, deadline)?;
                base = Some(range.from.clone());
                Plan::Changes(range)
            }
        };
        if let Some(b) = base {
            // A revision name can move while the positions stay: the commit names the answer.
            self.view_extra
                .lock()
                .expect("view_extra lock")
                .push(("base".to_string(), b));
        }
        let project = self
            .project
            .base()
            .root()
            .file_name()
            .unwrap_or("")
            .to_string();
        let mut peers = self.peers_for_read(&opts)?;
        let mut reply = self.query(&opts, |tx| {
            let seeds: Vec<Seed> = match &plan {
                Plan::Symbol(text) => match katalogr_query::resolve(tx, text)? {
                    Resolution::One(t) => seeds_for_symbol(&t),
                    other => return Ok(unresolved(other, text)),
                },
                Plan::File { repo, path } => match seeds_for_file(tx, repo, path)? {
                    Some(s) => s,
                    None => {
                        return Ok(Outcome::NoMatch(format!(
                            "no indexed file {repo}:{}",
                            path.as_str()
                        )));
                    }
                },
                Plan::Item(code) => match seeds_for_item(tx, code)? {
                    Some(s) => s,
                    None => return Ok(Outcome::NoMatch(format!("no item {code:?} in the index"))),
                },
                Plan::Changes(r) => seeds_for_changes(tx, &r.repo, &r.changes)?,
            };
            let mut report = katalogr_query::impact(tx, &seeds, iopts)?;
            report.projects = vec![project.clone()];
            Ok(Outcome::Found(report))
        })?;
        if let Outcome::Found(r) = &reply.outcome {
            reply.partial = r.partial.clone();
        }
        if !peers.is_empty()
            && let Outcome::Found(report) = &mut reply.outcome
        {
            if let Some(p) = across_peers(report, &mut peers, iopts) {
                reply.partial.push(p);
            }
            attach(&mut reply, &peers, Vec::new(), Vec::new());
        }
        Ok(reply)
    }
}

/// What one peer's rounds added so far.
#[derive(Default)]
struct Acc {
    /// Node name to its group kind, confidence and hit; the smaller depth wins.
    hits: BTreeMap<String, (String, Confidence, ImpactHit)>,
    tests: BTreeMap<String, TestHit>,
    contracts: BTreeMap<String, ImpactContract>,
    opaque: BTreeSet<PeerEnd>,
    /// False once a round gave no total (the walk stopped early).
    total_known: bool,
    answered: bool,
}

fn rank(c: Confidence) -> u8 {
    match c {
        Confidence::Extracted => 3,
        Confidence::Declared => 2,
        Confidence::Inferred => 1,
    }
}

fn contract_id(name: &str) -> Option<Id> {
    match parse_name(name) {
        Ok(AnyName::Contract(c)) => Some(c.id()),
        _ => None,
    }
}

/// The seed contracts through which `after` lost something `before` held: a hit or test that is
/// gone, or one whose chain lost a name.
fn removed_seeds(before: &ImpactReport, after: &ImpactReport) -> BTreeSet<String> {
    let kept: BTreeMap<&str, usize> = after
        .groups
        .iter()
        .flat_map(|g| &g.hits)
        .map(|h| (h.node.name.as_str(), h.chain.len()))
        .collect();
    let kept_tests: BTreeMap<&str, usize> = after
        .tests
        .iter()
        .map(|t| (t.test.name.as_str(), t.chain.len()))
        .collect();
    let mut out = BTreeSet::new();
    for h in before.groups.iter().flat_map(|g| &g.hits) {
        if kept.get(h.node.name.as_str()) != Some(&h.chain.len()) {
            out.insert(h.seed.clone());
        }
    }
    for t in &before.tests {
        if kept_tests.get(t.test.name.as_str()) != Some(&t.chain.len()) {
            out.insert(t.seed.clone());
        }
    }
    out
}

/// R-5. Continue `report` into each peer from the contracts it reached: round `r` seeds every
/// peer with the queued contracts at `contract-consumers`, filters what the user cannot read, and
/// queues the contracts newly reached there for the next round. Never changes the local groups.
/// Returns the partial that says the rounds ended before the walk did.
pub(crate) fn across_peers(
    report: &mut ImpactReport,
    peers: &mut [OpenPeer],
    iopts: &ImpactOptions,
) -> Option<Partial> {
    let mut queue: Vec<Id> = report
        .contracts
        .iter()
        .filter_map(|c| contract_id(&c.contract))
        .collect();
    queue.sort();
    queue.dedup();
    let mut queued: BTreeSet<Id> = queue.iter().copied().collect();
    let (snaps, mut failed) = snapshots(peers);
    let mut accs: Vec<Acc> = peers
        .iter()
        .map(|_| Acc {
            total_known: true,
            ..Acc::default()
        })
        .collect();
    let mut rounds = 0u32;
    for round in 1..=MAX_IMPACT_ROUNDS {
        if queue.is_empty() {
            break;
        }
        rounds = round;
        let seeds: Vec<Seed> = queue
            .iter()
            .map(|id| Seed {
                id: *id,
                precision: SEED_CONTRACT_CONSUMERS,
                wanted: None,
            })
            .collect();
        let mut next: Vec<Id> = Vec::new();
        for (i, (p, snap)) in peers.iter().zip(&snaps).enumerate() {
            let Some(tx) = snap else { continue };
            let Ok(raw) = katalogr_query::impact(tx, &seeds, iopts) else {
                failed.insert(i);
                continue;
            };
            let (rep, removed) = filter_report(raw.clone(), &p.spec.project, &p.access);
            let acc = &mut accs[i];
            acc.answered = true;
            acc.total_known &= rep.total.is_some();
            if removed {
                for seed in removed_seeds(&raw, &rep) {
                    acc.opaque.insert(PeerEnd {
                        project: p.spec.project.clone(),
                        link: seed,
                        role: "consumes".to_string(),
                        hit: None,
                        opaque: true,
                    });
                }
            }
            for g in rep.groups {
                for h in g.hits {
                    let better = acc
                        .hits
                        .get(&h.node.name)
                        .is_none_or(|(_, _, old)| h.depth < old.depth);
                    if better {
                        acc.hits
                            .insert(h.node.name.clone(), (g.kind.clone(), g.confidence, h));
                    }
                }
            }
            for t in rep.tests {
                let better = acc
                    .tests
                    .get(&t.test.name)
                    .is_none_or(|old| t.depth < old.depth);
                if better {
                    acc.tests.insert(t.test.name.clone(), t);
                }
            }
            for c in rep.contracts {
                let Some(id) = contract_id(&c.contract) else {
                    continue;
                };
                // Counts come from the ends the user can read; a contract reached only through
                // ones they cannot is not named.
                let Ok(ends) = peer_contract_ends(tx, &p.spec.project, &p.access, id) else {
                    failed.insert(i);
                    continue;
                };
                let count =
                    |role: &str| ends.iter().filter(|e| !e.opaque && e.role == role).count() as u64;
                let (providers, consumers) = (count("provides"), count("consumes"));
                if providers + consumers + count("declares") == 0 && !queued.contains(&id) {
                    continue;
                }
                let open = if consumers == 0 {
                    Some("no-consumer")
                } else if providers == 0 {
                    Some("no-provider")
                } else {
                    None
                };
                acc.contracts.insert(
                    c.contract.clone(),
                    ImpactContract {
                        contract: c.contract,
                        providers,
                        consumers,
                        open: open.map(str::to_string),
                    },
                );
                if queued.insert(id) {
                    next.push(id);
                }
            }
        }
        next.sort();
        next.dedup();
        queue = next;
    }
    let leftover = rounds == MAX_IMPACT_ROUNDS && !queue.is_empty();
    drop(snaps);
    for i in &failed {
        mark_unavailable(&mut peers[*i]);
    }
    for (i, (p, acc)) in peers.iter().zip(accs).enumerate() {
        if !acc.answered || failed.contains(&i) {
            continue;
        }
        report.projects.push(p.spec.project.clone());
        report.peers.push(into_peer_impact(&p.spec.project, acc));
    }
    report.projects.sort();
    report.projects.dedup();
    report.peers.sort_by(|a, b| a.project.cmp(&b.project));
    leftover.then(|| {
        Partial::new(
            cap::PEERS,
            u64::from(MAX_IMPACT_ROUNDS),
            u64::from(rounds),
            None,
        )
    })
}

fn into_peer_impact(project: &str, acc: Acc) -> PeerImpact {
    let count = acc.hits.len() as u64;
    let mut grouped: BTreeMap<(std::cmp::Reverse<u8>, String), (Confidence, Vec<ImpactHit>)> =
        BTreeMap::new();
    for (_, (kind, conf, hit)) in acc.hits {
        grouped
            .entry((std::cmp::Reverse(rank(conf)), kind))
            .or_insert_with(|| (conf, Vec::new()))
            .1
            .push(hit);
    }
    let groups = grouped
        .into_iter()
        .map(|((_, kind), (confidence, mut hits))| {
            hits.sort_by(|a, b| {
                a.depth
                    .cmp(&b.depth)
                    .then(b.score.total_cmp(&a.score))
                    .then_with(|| a.node.name.cmp(&b.node.name))
            });
            ImpactGroup {
                kind,
                confidence,
                hits,
            }
        })
        .collect();
    let mut tests: Vec<TestHit> = acc.tests.into_values().collect();
    tests.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then(b.score.total_cmp(&a.score))
            .then_with(|| a.test.name.cmp(&b.test.name))
    });
    PeerImpact {
        project: project.to_string(),
        groups,
        total: acc.total_known.then_some(count),
        tests,
        contracts: acc.contracts.into_values().collect(),
        opaque: acc.opaque.into_iter().collect(),
    }
}
