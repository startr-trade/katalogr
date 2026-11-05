//! Views (FEAT-032): a [`ViewSpec`] is answered by one read through the query layer and comes
//! back as a [`ViewGraph`] for the renderers.

use crate::{DistError, Katalogr, Options, Outcome, Reply, on_target, unresolved};
use katalogr_analytics::{CycleOptions, GraphOptions, Level, structure};
use katalogr_query::{Direction, Partial, PathStep, Resolution, SymbolHit, cap};
use katalogr_render::{ViewEdge, ViewGraph, ViewNode, ViewSpec, node_id};
use katalogr_store::StoreError;
use std::collections::{BTreeMap, BTreeSet};

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
            label: String::new(),
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
        "callers" => &["symbol"],
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
        let reply = self.query(opts, |tx| {
            let map = katalogr_query::contract_map(tx, kind, max)?;
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
            // The cap counts contracts; the ends come with them.
            let mut g = b.finish(title, 0);
            g.shown = map.items.len() as u64;
            g.total = map.total.max(g.shown);
            partial = limit_partial(max, g.shown, g.total);
            Ok(Outcome::Found(g))
        })?;
        Ok(with_partial(reply, partial))
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
