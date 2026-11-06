//! Views (FEAT-032): a [`ViewSpec`] is answered by one read through the query layer and comes
//! back as a [`ViewGraph`] for the renderers.

use crate::view_store::ViewStore;
use crate::workspace::fingerprint;
use crate::workspace_join::{attach, peer_ends};
use crate::{
    Consistency, DistError, Katalogr, Options, Outcome, Reply, Report, on_target, unresolved,
};
use katalogr_analytics::{CycleOptions, GraphOptions, Level, structure};
use katalogr_query::{
    Direction, LinkKeys, Partial, PathStep, PeerEnd, PositionRef, Resolution, SymbolHit,
    ViewAnswer, ViewQuery, ViewQueryJson, cap,
};
use katalogr_render::{PELE_VERSION, ViewEdge, ViewGraph, ViewNode, ViewSpec, node_id, to_mermaid};
use katalogr_store::StoreError;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

/// `views.store_cap_mb` when the project does not set it (provisional, FEAT-088).
pub const DEFAULT_STORE_CAP_MB: u64 = 256;
/// The project setting that bounds the view store.
const STORE_CAP_KEY: &str = "views.store_cap_mb";

/// The bytes of a [`ViewAnswer`] and where they came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewBytes {
    /// Compact JSON, fields in declared order: what is stored and what is sent.
    pub body: Vec<u8>,
    /// True when the store answered and nothing was read from the index.
    pub from_store: bool,
}

/// What `view_fresh` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreshCheck {
    /// No stored entry to compare (or the answer is not exact, so it has no ID).
    NotStored,
    /// The stored bytes equal a fresh computation.
    Same,
    /// The stored bytes differ from a fresh computation of the same ID.
    Different { view_id: String },
}

/// The question as a view-ID query: the op is `view.<op>`, so it cannot collide with a plain
/// read of the same name.
pub(crate) fn to_view_query(spec: &ViewSpec) -> ViewQuery {
    ViewQuery {
        op: format!("view.{}", spec.op),
        args: spec.args.clone(),
    }
}

/// The one serialisation of an answer: compact, fields in declared order, `None` fields left
/// out. The CLI and the API both send these bytes.
pub fn answer_bytes(answer: &ViewAnswer) -> Vec<u8> {
    serde_json::to_vec(answer).expect("serialise view answer")
}

/// The views that read peers (FEAT-020). The others stay answers about this project alone.
pub(crate) const PEER_VIEW_OPS: &[&str] = &["contracts", "impact"];

/// The name of a peer's node in a view: `<project>//<canonical>`, so `group_of` gives
/// `<project>//<repo>` and two projects sharing a repo name cannot collide.
fn peer_name(project: &str, name: &str) -> String {
    format!("{project}//{name}")
}

/// A peer's ends as nodes, each joined to its contract. An opaque end is one node per project,
/// named `<project>//hidden`, labelled `hidden` and of kind `opaque`.
fn peer_nodes(b: &mut Builder, ends: &[PeerEnd]) {
    for e in ends {
        let contract = e.link.as_str();
        if !contract.starts_with("contract:") {
            continue;
        }
        b.node(contract, "contract", None);
        match &e.hit {
            Some(h) => {
                let name = peer_name(&e.project, &h.name);
                b.node(&name, &h.kind, Some(&h.path));
                b.edge(&name, contract, &e.role, false);
            }
            None => {
                let name = peer_name(&e.project, "hidden");
                b.node(&name, "opaque", None);
                b.edge(&name, contract, &e.role, false);
            }
        }
    }
}

/// Most members listed per cycle in a cycles view.
const CYCLE_MEMBERS: usize = 60;

/// Collects nodes and edges by canonical name, then sorts and dedups them.
#[derive(Default)]
struct Builder {
    nodes: BTreeMap<String, ViewNode>,
    edges: BTreeSet<(String, String, String, bool)>,
}

impl Builder {
    fn node(&mut self, name: &str, kind: &str, path: Option<&str>) {
        let id = node_id(name);
        self.nodes.entry(id.clone()).or_insert_with(|| ViewNode {
            id,
            name: name.to_string(),
            label: if kind == "opaque" {
                "hidden".to_string()
            } else {
                String::new()
            },
            kind: kind.to_string(),
            path: path.map(str::to_string),
            group: group_of(name, kind),
        });
    }

    fn edge(&mut self, from: &str, to: &str, kind: &str, inferred: bool) {
        self.edges
            .insert((node_id(from), node_id(to), kind.to_string(), inferred));
    }

    /// Sorted nodes and edges; only edges whose ends are both nodes are kept.
    fn finish(self, title: String, total: u64) -> ViewGraph {
        let nodes: Vec<ViewNode> = self.nodes.into_values().collect();
        let ids: BTreeSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        let edges: Vec<ViewEdge> = self
            .edges
            .iter()
            .filter(|(f, t, _, _)| ids.contains(f.as_str()) && ids.contains(t.as_str()))
            .map(|(from, to, kind, inferred)| ViewEdge {
                from: from.clone(),
                to: to.clone(),
                kind: kind.clone(),
                inferred: *inferred,
            })
            .collect();
        let shown = nodes.len() as u64;
        ViewGraph {
            title,
            nodes,
            edges,
            shown,
            total: total.max(shown),
        }
    }
}

/// The repository a node belongs to: the part of its canonical name before the first colon.
/// Packages and contracts belong to no repository.
fn group_of(name: &str, kind: &str) -> Option<String> {
    if kind == "package" || kind == "contract" || name.starts_with("contract:") {
        return None;
    }
    name.split_once(':').map(|(repo, _)| repo.to_string())
}

/// The kind a canonical name carries: the kind of its last segment, or `artifact`.
fn kind_of(name: &str) -> String {
    if name.starts_with("contract:") {
        return "contract".to_string();
    }
    match name.rsplit_once('#') {
        Some((_, tail)) => tail
            .rsplit('/')
            .next()
            .and_then(|seg| seg.split_once(':'))
            .map_or("symbol", |(k, _)| k)
            .to_string(),
        None if name.contains(':') => "artifact".to_string(),
        None => "package".to_string(),
    }
}

/// The query line of a spec: the op, its positional words, then its flags in name order.
fn title_of(spec: &ViewSpec) -> String {
    let positional: &[&str] = match spec.op.as_str() {
        "callers" | "impact" => &["symbol"],
        "path" => &["from", "to"],
        _ => &[],
    };
    let word = |w: &str| {
        if w.contains(' ') {
            format!("\"{w}\"")
        } else {
            w.to_string()
        }
    };
    let mut parts = vec![spec.op.clone()];
    for key in positional {
        if let Some(v) = spec.args.get(*key) {
            parts.push(word(v));
        }
    }
    for (k, v) in &spec.args {
        if !positional.contains(&k.as_str()) {
            parts.push(format!("--{k} {}", word(v)));
        }
    }
    parts.join(" ")
}

fn arg<'a>(spec: &'a ViewSpec, key: &str) -> Result<&'a str, DistError> {
    spec.args
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| DistError::usage(format!("view `{}` needs `{key}`", spec.op)))
}

fn number(spec: &ViewSpec, key: &str) -> Result<usize, DistError> {
    let v = arg(spec, key)?;
    v.parse::<usize>()
        .map_err(|_| DistError::usage(format!("`{key}` wants a whole number, got `{v}`")))
}

fn level(spec: &ViewSpec) -> Result<Level, DistError> {
    match arg(spec, "level")? {
        "file" => Ok(Level::File),
        "package" => Ok(Level::Package),
        other => Err(DistError::usage(format!("unknown level `{other}`"))),
    }
}

fn limit_partial(at: usize, shown: u64, total: u64) -> Vec<Partial> {
    if total > shown {
        vec![Partial::new(cap::LIMIT, at as u64, shown, Some(total))]
    } else {
        Vec::new()
    }
}

/// A path step as an edge between its ends in the direction of the link.
fn step_edge(b: &mut Builder, s: &PathStep) {
    b.node(&s.from, &kind_of(&s.from), None);
    b.node(&s.to, &kind_of(&s.to), None);
    let inferred = s.confidence == katalogr_model::Confidence::Inferred;
    match s.direction {
        Direction::Forward => b.edge(&s.from, &s.to, &s.kind, inferred),
        Direction::Backward => b.edge(&s.to, &s.from, &s.kind, inferred),
    }
}

fn with_partial<T>(mut reply: Reply<T>, partial: Vec<Partial>) -> Reply<T> {
    reply.partial = partial;
    reply
}

impl Katalogr {
    /// Runs the op through `query` and builds the graph. Outcome and Report as any other read:
    /// an ambiguous symbol or no match passes through as it does for `callers`.
    pub fn view_graph(
        &self,
        spec: &ViewSpec,
        opts: &Options,
    ) -> Result<Reply<ViewGraph>, DistError> {
        let title = title_of(spec);
        match spec.op.as_str() {
            "structure" => self.view_structure(spec, opts, title),
            "contracts" => self.view_contracts(spec, opts, title),
            "callers" => self.view_callers(spec, opts, title),
            "impact" => self.view_impact(spec, opts, title),
            "path" => self.view_path(spec, opts, title),
            "cycles" => self.view_cycles(spec, opts, title),
            other => Err(DistError::usage(format!("unknown view op `{other}`"))),
        }
    }

    fn view_structure(
        &self,
        spec: &ViewSpec,
        opts: &Options,
        title: String,
    ) -> Result<Reply<ViewGraph>, DistError> {
        let graph = GraphOptions::new(level(spec)?, arg(spec, "inferred")? == "true");
        let max = number(spec, "max-nodes")?;
        let mut partial = Vec::new();
        let reply = self.query(opts, |tx| {
            let report = structure::structure(tx, &graph, max)?;
            let mut b = Builder::default();
            for n in &report.nodes {
                b.node(&n.node.name, &n.node.kind, n.node.path.as_deref());
            }
            for e in &report.edges {
                b.edge(&e.from, &e.to, &e.kind, e.inferred);
            }
            let g = b.finish(title, report.nodes_total);
            partial = limit_partial(max, g.shown, g.total);
            Ok(Outcome::Found(g))
        })?;
        Ok(with_partial(reply, partial))
    }

    fn view_contracts(
        &self,
        spec: &ViewSpec,
        opts: &Options,
        title: String,
    ) -> Result<Reply<ViewGraph>, DistError> {
        let kind = Some(arg(spec, "kind")?).filter(|k| !k.is_empty());
        let max = number(spec, "max-nodes")?;
        let mut partial = Vec::new();
        let _extra = self.extra_scope();
        let mut peers = self.peers_for_read(opts)?;
        let mut keys = LinkKeys::default();
        let reply = self.query(opts, |tx| {
            let map = katalogr_query::contract_map(tx, kind, max)?;
            if !peers.is_empty() {
                keys.contracts = map
                    .items
                    .iter()
                    .map(|i| katalogr_model::Id::of(&i.contract))
                    .collect();
            }
            let mut b = Builder::default();
            for item in &map.items {
                b.node(&item.contract, "contract", None);
                let roles: [(&str, &Vec<SymbolHit>); 3] = [
                    ("provides", &item.providers),
                    ("consumes", &item.consumers),
                    ("declares", &item.declarers),
                ];
                for (role, ends) in roles {
                    for end in ends {
                        b.node(&end.name, &end.kind, Some(&end.path));
                        b.edge(&end.name, &item.contract, role, false);
                    }
                }
            }
            // The peers' ends of the same contracts, drawn beside the local ones.
            if !peers.is_empty() {
                peer_nodes(&mut b, &peer_ends(&mut peers, &keys));
            }
            // The cap counts contracts; the ends come with them.
            let mut g = b.finish(title, 0);
            g.shown = map.items.len() as u64;
            g.total = map.total.max(g.shown);
            partial = limit_partial(max, g.shown, g.total);
            Ok(Outcome::Found(g))
        })?;
        let mut reply = with_partial(reply, partial);
        attach(&mut reply, &peers, Vec::new(), Vec::new());
        Ok(reply)
    }

    fn view_callers(
        &self,
        spec: &ViewSpec,
        opts: &Options,
        title: String,
    ) -> Result<Reply<ViewGraph>, DistError> {
        let text = arg(spec, "symbol")?;
        let depth = number(spec, "depth")?.max(1);
        let max = number(spec, "max-nodes")?.max(1);
        let mut partial = Vec::new();
        let reply = self.query(opts, |tx| {
            on_target(tx, text, |root| {
                // Level by level, so each caller is joined to the symbol it calls.
                let mut order: Vec<SymbolHit> = vec![root.clone()];
                let mut seen: BTreeSet<String> = BTreeSet::from([root.name.clone()]);
                let mut edges: Vec<(String, String, String, bool)> = Vec::new();
                let mut frontier = vec![root.clone()];
                for _ in 0..depth {
                    let mut next = Vec::new();
                    for callee in &frontier {
                        for hit in katalogr_query::callers(tx, callee, 1)? {
                            let inferred = hit.confidence == katalogr_model::Confidence::Inferred;
                            let kind = if hit.via.is_some() {
                                "via-contract"
                            } else {
                                "calls"
                            };
                            edges.push((
                                hit.caller.name.clone(),
                                callee.name.clone(),
                                kind.to_string(),
                                inferred,
                            ));
                            if seen.insert(hit.caller.name.clone()) {
                                order.push(hit.caller.clone());
                                next.push(hit.caller);
                            }
                        }
                    }
                    frontier = next;
                }
                let total = order.len() as u64;
                order.truncate(max);
                let mut b = Builder::default();
                for n in &order {
                    b.node(&n.name, &n.kind, Some(&n.path));
                }
                for (f, t, k, i) in &edges {
                    b.edge(f, t, k, *i);
                }
                let g = b.finish(title, total);
                partial = limit_partial(max, g.shown, g.total);
                Ok::<_, StoreError>(g)
            })
        })?;
        Ok(with_partial(reply, partial))
    }

    /// The symbol's impact: the seed, its hits and its tests as nodes, each joined to the node
    /// it was reached from (`chain[0]`, else the seed). The cap is `max-nodes` on hits.
    fn view_impact(
        &self,
        spec: &ViewSpec,
        opts: &Options,
        title: String,
    ) -> Result<Reply<ViewGraph>, DistError> {
        let text = arg(spec, "symbol")?;
        let depth = number(spec, "depth")?.max(1) as u32;
        let max = number(spec, "max-nodes")?.max(1);
        let iopts = katalogr_query::ImpactOptions {
            depth,
            limit: max,
            ..Default::default()
        };
        let mut partial = Vec::new();
        let _extra = self.extra_scope();
        let mut peers = self.peers_for_read(opts)?;
        let mut rounds = None;
        let reply = self.query(opts, |tx| {
            on_target(tx, text, |root| {
                let seeds = katalogr_query::seeds_for_symbol(root);
                let mut report = katalogr_query::impact(tx, &seeds, &iopts)?;
                if !peers.is_empty() {
                    rounds = crate::impact::across_peers(&mut report, &mut peers, &iopts);
                }
                let mut b = Builder::default();
                for s in &report.seeds {
                    b.node(&s.node.name, &s.node.kind, Some(&s.node.path));
                }
                for g in &report.groups {
                    let inferred = g.confidence == katalogr_model::Confidence::Inferred;
                    for h in &g.hits {
                        b.node(&h.node.name, &h.node.kind, Some(&h.node.path));
                        let to = h.chain.first().unwrap_or(&h.seed);
                        b.edge(&h.node.name, to, &g.kind, inferred);
                    }
                }
                for t in &report.tests {
                    b.node(&t.test.name, &t.test.kind, Some(&t.test.path));
                    let to = t.chain.first().unwrap_or(&t.seed);
                    b.edge(&t.test.name, to, "tests", false);
                }
                // What each peer adds: its hits, joined to the contract they came through (or to
                // the node they were reached from inside the peer), and one hidden node when
                // something there is out of the user's reach.
                for p in &report.peers {
                    for g in &p.groups {
                        let inferred = g.confidence == katalogr_model::Confidence::Inferred;
                        for h in &g.hits {
                            let name = peer_name(&p.project, &h.node.name);
                            b.node(&name, &h.node.kind, Some(&h.node.path));
                            let to = match (&h.via, h.chain.first()) {
                                (_, Some(c)) => peer_name(&p.project, c),
                                (Some(v), None) => v.clone(),
                                (None, None) => h.seed.clone(),
                            };
                            if to.starts_with("contract:") {
                                b.node(&to, "contract", None);
                            }
                            b.edge(&name, &to, &g.kind, inferred);
                        }
                    }
                    peer_nodes(&mut b, &p.opaque);
                }
                let hits: u64 = report.groups.iter().map(|g| g.hits.len() as u64).sum();
                let others = (report.seeds.len() + report.tests.len()) as u64;
                let total = report.total.unwrap_or(hits) + others;
                let g = b.finish(title, total);
                partial = limit_partial(max, g.shown, g.total);
                Ok::<_, StoreError>(g)
            })
        })?;
        let mut reply = with_partial(reply, partial);
        reply.partial.extend(rounds);
        attach(&mut reply, &peers, Vec::new(), Vec::new());
        Ok(reply)
    }

    fn view_path(
        &self,
        spec: &ViewSpec,
        opts: &Options,
        title: String,
    ) -> Result<Reply<ViewGraph>, DistError> {
        let (a, bb) = (arg(spec, "from")?, arg(spec, "to")?);
        let max_len = number(spec, "max-len")? as u32;
        self.query(opts, |tx| {
            let first = match katalogr_query::resolve(tx, a)? {
                Resolution::One(h) => h,
                other => return Ok(unresolved(other, a)),
            };
            let second = match katalogr_query::resolve(tx, bb)? {
                Resolution::One(h) => h,
                other => return Ok(unresolved(other, bb)),
            };
            Ok(match katalogr_query::path(tx, &first, &second, max_len)? {
                Some(steps) => {
                    let mut b = Builder::default();
                    b.node(&first.name, &first.kind, Some(&first.path));
                    b.node(&second.name, &second.kind, Some(&second.path));
                    for s in &steps {
                        step_edge(&mut b, s);
                    }
                    let n = b.nodes.len() as u64;
                    Outcome::Found(b.finish(title, n))
                }
                None => Outcome::NoMatch(format!(
                    "no path from {} to {} within {max_len} steps; {} (searched up to {max_len} links)",
                    first.name,
                    second.name,
                    katalogr_query::PARTIAL_NOTE
                )),
            })
        })
    }

    fn view_cycles(
        &self,
        spec: &ViewSpec,
        opts: &Options,
        title: String,
    ) -> Result<Reply<ViewGraph>, DistError> {
        let limit = number(spec, "limit")?;
        let copts = CycleOptions {
            graph: GraphOptions::new(level(spec)?, false),
            members: CYCLE_MEMBERS,
            limit,
        };
        let mut partial = Vec::new();
        let reply = self.query(opts, |tx| {
            let report = katalogr_analytics::cycles(tx, &copts)?;
            let mut b = Builder::default();
            let mut in_items = 0u64;
            for c in &report.items {
                in_items += c.members_total;
                for m in &c.members {
                    b.node(&m.name, &m.kind, m.path.as_deref());
                }
                for s in &c.witness {
                    step_edge(&mut b, s);
                }
            }
            // Cycles beyond the limit have at least two members each.
            let hidden = report
                .cycles_total
                .saturating_sub(report.items.len() as u64);
            let g = b.finish(title, in_items + hidden * 2);
            partial = limit_partial(limit, report.items.len() as u64, report.cycles_total);
            Ok(Outcome::Found(g))
        })?;
        Ok(with_partial(reply, partial))
    }
}

impl Katalogr {
    /// The store of this project, bounded by `views.store_cap_mb`.
    pub(crate) fn view_store(&self) -> ViewStore {
        let mb = self
            .project
            .config()
            .settings
            .get(STORE_CAP_KEY)
            .and_then(serde_yaml_ng::Value::as_u64)
            .unwrap_or(DEFAULT_STORE_CAP_MB);
        ViewStore::open(&self.project, mb.saturating_mul(1 << 20))
    }

    /// The ID this question has right now, with no query, and the positions it covers. `None`
    /// when a position cannot be read, or when `--at` names a position in a project of several
    /// repos (the other repos' positions are then known only by reading).
    pub fn view_preflight(
        &self,
        spec: &ViewSpec,
        opts: &Options,
    ) -> Result<Option<(String, Vec<PositionRef>)>, DistError> {
        let deadline = Instant::now() + opts.timeout()?;
        let Ok(engine) = self.engine_for(Consistency::StaleOk, deadline) else {
            return Ok(None);
        };
        let positions: Vec<PositionRef> = match opts.at.as_deref() {
            Some(text) => {
                let Ok(repo) = self.at_repo() else {
                    return Ok(None);
                };
                let Ok(wanted) = PositionRef::parse(&repo, text) else {
                    return Ok(None);
                };
                if engine.roots().len() != 1 || !engine.roots().contains_key(&repo) {
                    return Ok(None);
                }
                vec![wanted]
            }
            None => {
                let mut live = Vec::new();
                for repo in engine.roots().keys() {
                    match engine.live_position(repo) {
                        Ok(p) => live.push(p),
                        Err(_) => return Ok(None),
                    }
                }
                live
            }
        };
        if positions.is_empty() {
            return Ok(None);
        }
        let Ok(snap) = engine.cache().read() else {
            return Ok(None);
        };
        let Ok(facts) = engine.facts_version(&snap) else {
            return Ok(None);
        };
        let mut query = to_view_query(spec);
        // A view that reads peers is a question about their positions too; one of them not
        // exact has no ID to know in advance.
        if PEER_VIEW_OPS.contains(&spec.op.as_str()) && !opts.no_peers {
            let Ok(peers) = self.open_peers(deadline) else {
                return Ok(None);
            };
            if peers
                .iter()
                .any(|p| p.state.state != "ok" || p.state.stale || !p.state.repos_absent.is_empty())
            {
                return Ok(None);
            }
            if !peers.is_empty() {
                query
                    .args
                    .insert("workspace".to_string(), fingerprint(&peers));
            }
        }
        let id = katalogr_query::view_id(&positions, &facts, &query);
        Ok(Some((id, positions)))
    }

    /// Stored bytes on a hit, with no query and no wait for the queue: the ID fixes the content
    /// whatever the index has caught up to. Otherwise computes, and stores when exact.
    pub fn view(&self, spec: &ViewSpec, opts: &Options) -> Result<Reply<ViewBytes>, DistError> {
        let store = self.view_store();
        let pre = self.view_preflight(spec, opts).ok().flatten();
        if let Some((id, positions)) = &pre
            && let Some(body) = store.get(id)
        {
            return Ok(Reply {
                workspace: None,
                outcome: Outcome::Found(ViewBytes {
                    body,
                    from_store: true,
                }),
                report: Report {
                    watermark: 0,
                    pending: 0,
                    stale: false,
                    timed_out: false,
                    queued: 0,
                    warnings: self.take_notes(),
                    positions: positions.clone(),
                    working_set: None,
                    exact: true,
                    view_id: Some(id.clone()),
                    context: None,
                },
                diagnostic: None,
                partial: Vec::new(),
                folded: Vec::new(),
            });
        }
        self.view_compute(
            spec,
            opts,
            pre.as_ref().map(|(id, _)| id.as_str()),
            Some(&store),
        )
    }

    /// Recomputes, compares with the stored entry and says whether they are equal. Nothing is
    /// stored.
    pub fn view_fresh(&self, spec: &ViewSpec, opts: &Options) -> Result<FreshCheck, DistError> {
        let reply = self.view_compute(spec, opts, None, None)?;
        match reply.outcome {
            Outcome::Found(fresh) => {
                let Some(id) = reply.report.view_id else {
                    return Ok(FreshCheck::NotStored);
                };
                Ok(match self.view_store().get(&id) {
                    None => FreshCheck::NotStored,
                    Some(stored) if stored == fresh.body => FreshCheck::Same,
                    Some(_) => FreshCheck::Different { view_id: id },
                })
            }
            Outcome::NoMatch(m) | Outcome::StaleFile(m) => Err(DistError::usage(m)),
            Outcome::Ambiguous(c) => Err(DistError::usage(format!(
                "ambiguous: {} candidates",
                c.len()
            ))),
            Outcome::PositionUnavailable { wanted, .. } => Err(DistError::usage(format!(
                "no context holds position {wanted}"
            ))),
        }
    }

    /// The read: `view_graph` through the normal path, then the answer's bytes, stored when
    /// exact and a store is given. `preflight_id` is what step 1 expected.
    fn view_compute(
        &self,
        spec: &ViewSpec,
        opts: &Options,
        preflight_id: Option<&str>,
        store: Option<&ViewStore>,
    ) -> Result<Reply<ViewBytes>, DistError> {
        let o = Options {
            view: Some(to_view_query(spec)),
            ..opts.clone()
        };
        let reply = self.view_graph(spec, &o)?;
        let Reply {
            outcome,
            mut report,
            diagnostic,
            partial,
            folded,
            workspace,
        } = reply;
        let outcome = match outcome {
            Outcome::Found(graph) => {
                let view_id = report.view_id.clone().filter(|_| report.exact);
                let exact = view_id.is_some();
                let answer = ViewAnswer {
                    schema: katalogr_query::RESULT_SCHEMA_VERSION,
                    view_format: katalogr_query::VIEW_FORMAT,
                    view_id: view_id.clone(),
                    exact,
                    positions: report.positions.clone(),
                    query: ViewQueryJson {
                        op: spec.op.clone(),
                        args: spec.args.clone(),
                    },
                    mermaid: to_mermaid(&graph),
                    graph: serde_json::to_value(&graph).expect("serialise view graph"),
                    pele: PELE_VERSION.to_string(),
                    watermark: (!exact).then_some(report.watermark),
                    pending: (!exact).then_some(report.pending),
                    stale: (!exact).then_some(report.stale),
                    peers: workspace
                        .as_ref()
                        .map(|w| w.peers.clone())
                        .unwrap_or_default(),
                };
                let body = answer_bytes(&answer);
                if let Some(id) = &view_id {
                    if preflight_id.is_some_and(|p| p != id) {
                        report.warnings.push(format!(
                            "view id {id} differs from the one computed before the read ({}); stored under {id}",
                            preflight_id.unwrap_or_default()
                        ));
                    }
                    if let Some(store) = store
                        && let Err(e) = store.put(id, &body)
                    {
                        report.warnings.push(format!("view not stored: {e}"));
                    }
                }
                Outcome::Found(ViewBytes {
                    body,
                    from_store: false,
                })
            }
            Outcome::Ambiguous(c) => Outcome::Ambiguous(c),
            Outcome::NoMatch(m) => Outcome::NoMatch(m),
            Outcome::StaleFile(m) => Outcome::StaleFile(m),
            Outcome::PositionUnavailable { wanted, available } => {
                Outcome::PositionUnavailable { wanted, available }
            }
        };
        Ok(Reply {
            workspace,
            outcome,
            report,
            diagnostic,
            partial,
            folded,
        })
    }
}
