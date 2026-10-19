//! The insight graph: a directed, weighted graph of symbols, files or packages built from one
//! snapshot's usage links, in compressed sparse row form (FEAT-030 sections 4 and 6.1).

use crate::options::{CONTRACT_EDGE_CAP, CONTRACT_EDGE_KIND, GraphOptions};
use crate::result::{Level, NodeHit};
use katalogr_model::insight::FACT_PACKAGE;
use katalogr_model::{Confidence, Fact, Id, Name};
use katalogr_store::{LinkRow, Reader, StoreError, row_weight};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// How much an edge is trusted. Ordered: weak is less than strong.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Strength {
    /// Inferred, or its reference resolved to several targets.
    Weak,
    Strong,
}

impl Strength {
    /// The confidence a witness step of this strength is reported with.
    pub(crate) fn confidence(self) -> Confidence {
        match self {
            Strength::Strong => Confidence::Extracted,
            Strength::Weak => Confidence::Inferred,
        }
    }
}

/// An entry is strong when it is not inferred and its fan-out is at most one; a row is strong
/// when any entry is.
pub(crate) fn row_strength(row: &LinkRow) -> Strength {
    let strong = row
        .entries
        .iter()
        .any(|e| e.confidence != Confidence::Inferred && e.fanout.is_none_or(|n| n <= 1));
    if strong {
        Strength::Strong
    } else {
        Strength::Weak
    }
}

/// One merged edge. In the forward lists `to` is the target; in the reverse lists it is the
/// source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Edge {
    pub to: u32,
    pub weight: f64,
    pub strength: Strength,
    /// Index into [`InsightGraph::kinds`].
    pub kind: u16,
}

/// Nodes at one [`Level`] and the merged usage edges between them.
pub(crate) struct InsightGraph {
    pub level: Level,
    pub nodes: Vec<NodeHit>,
    /// Sorted link kinds present on the edges.
    pub kinds: Vec<String>,
    out_start: Vec<u32>,
    out: Vec<Edge>,
    in_start: Vec<u32>,
    inn: Vec<Edge>,
    /// Symbols in each node (1 at symbol level).
    pub members: Vec<u64>,
    /// Files in each node (1 at symbol and file level).
    pub file_count: Vec<u32>,
    /// Symbol level only: the index of each node's file among the distinct files.
    pub file_of: Vec<u32>,
    /// File and package level: canonical symbol name to the node holding it.
    sym_node: HashMap<String, u32>,
    /// Contracts too large to derive edges from.
    pub skipped_contracts: u64,
    /// Package level: files without a `package` fact. Counted for diagnostics; no report
    /// field carries it.
    #[allow(dead_code)]
    pub ungrouped_files: u64,
}

struct RawEdge {
    a: u32,
    b: u32,
    kind: u16,
    weight: f64,
    strength: Strength,
}

/// Forward and reverse adjacency from `(a, b, edge)` triples sorted by `(a, b)`.
fn build_csr(n: usize, merged: &[(u32, u32, Edge)]) -> (Vec<u32>, Vec<Edge>, Vec<u32>, Vec<Edge>) {
    let mut out_start = vec![0u32; n + 1];
    let mut in_start = vec![0u32; n + 1];
    for (a, b, _) in merged {
        out_start[*a as usize + 1] += 1;
        in_start[*b as usize + 1] += 1;
    }
    for i in 0..n {
        out_start[i + 1] += out_start[i];
        in_start[i + 1] += in_start[i];
    }
    let out: Vec<Edge> = merged.iter().map(|(_, _, e)| *e).collect();
    let mut inn = vec![
        Edge {
            to: 0,
            weight: 0.0,
            strength: Strength::Weak,
            kind: 0
        };
        merged.len()
    ];
    let mut fill = in_start.clone();
    for (a, b, e) in merged {
        let slot = fill[*b as usize] as usize;
        fill[*b as usize] += 1;
        inn[slot] = Edge { to: *a, ..*e };
    }
    (out_start, out, in_start, inn)
}

/// The longest directory that is a prefix (by whole components) of every directory in `dirs`;
/// none when there is none, or nothing to compare.
pub(crate) fn common_prefix<'a>(dirs: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut common: Option<Vec<&str>> = None;
    for dir in dirs {
        let parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
        common = Some(match common {
            None => parts,
            Some(c) => c
                .iter()
                .zip(&parts)
                .take_while(|(x, y)| x == y)
                .map(|(x, _)| *x)
                .collect(),
        });
    }
    common.filter(|c| !c.is_empty()).map(|c| c.join("/"))
}

/// Longest directory that holds every `(repo, path)` of `files`; none when the repositories
/// differ or nothing is shared.
pub(crate) fn common_dir<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<String> {
    let mut repo: Option<&str> = None;
    let mut dirs: Vec<&str> = Vec::new();
    for (r, path) in files {
        match repo {
            None => repo = Some(r),
            Some(x) if x != r => return None,
            _ => {}
        }
        dirs.push(path.rsplit_once('/').map_or("", |(d, _)| d));
    }
    common_prefix(dirs)
}

impl InsightGraph {
    /// One read of the symbols, artifacts, links and contracts of the snapshot.
    pub(crate) fn load(tx: &impl Reader, opts: &GraphOptions) -> Result<InsightGraph, StoreError> {
        let symbols = tx.symbols()?;
        let artifacts = tx.artifacts()?;
        let links = tx.links_all()?;
        let contracts = tx.contracts()?;

        let package_of = |facts: &BTreeMap<String, Fact>| match facts.get(FACT_PACKAGE) {
            Some(Fact::Str(p)) => Some(p.clone()),
            _ => None,
        };

        // Nodes, and the node of every symbol and artifact.
        let mut nodes: Vec<NodeHit> = Vec::new();
        let mut members: Vec<u64> = Vec::new();
        let mut file_count: Vec<u32>;
        let mut file_of: Vec<u32> = Vec::new();
        let mut node_of: HashMap<Id, u32> = HashMap::new();
        let mut sym_node: HashMap<String, u32> = HashMap::new();
        let mut ungrouped_files = 0u64;
        match opts.level {
            Level::Symbol => {
                let mut file_index: HashMap<Id, u32> = HashMap::new();
                for (i, (id, row)) in symbols.iter().enumerate() {
                    node_of.insert(*id, i as u32);
                    nodes.push(NodeHit {
                        level: Level::Symbol,
                        name: row.name.canonical(),
                        kind: row.kind.clone(),
                        path: Some(row.name.artifact.path.as_str().to_string()),
                        line: Some(row.span.start_line),
                    });
                    let next = file_index.len() as u32;
                    file_of.push(*file_index.entry(row.artifact).or_insert(next));
                }
                members = vec![1; nodes.len()];
                file_count = vec![1; nodes.len()];
            }
            Level::File => {
                for (i, (id, row)) in artifacts.iter().enumerate() {
                    node_of.insert(*id, i as u32);
                    nodes.push(NodeHit {
                        level: Level::File,
                        name: row.name.canonical(),
                        kind: "artifact".to_string(),
                        path: Some(row.name.path.as_str().to_string()),
                        line: None,
                    });
                    members.push(row.symbols.len() as u64);
                }
                file_count = vec![1; nodes.len()];
                for (_, row) in &symbols {
                    if let Some(&n) = node_of.get(&row.artifact) {
                        sym_node.insert(row.name.canonical(), n);
                    }
                }
            }
            Level::Package => {
                let mut names: BTreeSet<String> = BTreeSet::new();
                for (_, row) in &artifacts {
                    match package_of(&row.facts) {
                        Some(p) => {
                            names.insert(p);
                        }
                        None => ungrouped_files += 1,
                    }
                }
                let index: HashMap<&str, u32> = names
                    .iter()
                    .enumerate()
                    .map(|(i, n)| (n.as_str(), i as u32))
                    .collect();
                let mut dirs: Vec<Vec<(&str, &str)>> = vec![Vec::new(); names.len()];
                members = vec![0; names.len()];
                file_count = vec![0; names.len()];
                for (id, row) in &artifacts {
                    let Some(p) = package_of(&row.facts) else {
                        continue;
                    };
                    let n = index[p.as_str()];
                    node_of.insert(*id, n);
                    dirs[n as usize].push((row.name.repo.as_str(), row.name.path.as_str()));
                    members[n as usize] += row.symbols.len() as u64;
                    file_count[n as usize] += 1;
                }
                for (i, name) in names.iter().enumerate() {
                    nodes.push(NodeHit {
                        level: Level::Package,
                        name: name.clone(),
                        kind: "package".to_string(),
                        path: common_dir(dirs[i].iter().copied()),
                        line: None,
                    });
                }
                for (_, row) in &symbols {
                    if let Some(&n) = node_of.get(&row.artifact) {
                        sym_node.insert(row.name.canonical(), n);
                    }
                }
            }
        }
        if opts.level != Level::Symbol {
            // Symbols (and, at package level, artifacts) map to the node of their file.
            for (id, row) in &symbols {
                if let Some(&n) = node_of.get(&row.artifact) {
                    node_of.insert(*id, n);
                }
            }
        }

        // The kind table: every kind that can appear on an edge, sorted.
        let mut kind_set: BTreeSet<&str> = BTreeSet::new();
        kind_set.insert(CONTRACT_EDGE_KIND);
        for (key, _) in &links {
            let k = key.kind.0.as_str();
            if !opts.exclude_kinds.iter().any(|x| x == k) {
                kind_set.insert(k);
            }
        }
        let kinds: Vec<String> = kind_set.iter().map(|k| k.to_string()).collect();
        let kind_index: HashMap<&str, u16> = kinds
            .iter()
            .enumerate()
            .map(|(i, k)| (k.as_str(), i as u16))
            .collect();

        // Usage edges, and the consumers and providers of each contract.
        let contract_ids: BTreeSet<Id> = contracts.iter().map(|(id, _)| *id).collect();
        type Side = Vec<(u32, f64, Strength)>;
        let mut consumers: BTreeMap<Id, Side> = BTreeMap::new();
        let mut providers: BTreeMap<Id, Side> = BTreeMap::new();
        let mut raw: Vec<RawEdge> = Vec::new();
        for (key, row) in &links {
            let kind = key.kind.0.as_str();
            let strength = row_strength(row);
            if strength == Strength::Weak && !opts.include_inferred {
                continue;
            }
            let weight = row_weight(row);
            if weight <= 0.0 {
                continue;
            }
            let Some(&a) = node_of.get(&key.from) else {
                continue;
            };
            if contract_ids.contains(&key.to) {
                match kind {
                    "consumes" => consumers
                        .entry(key.to)
                        .or_default()
                        .push((a, weight, strength)),
                    "provides" => providers
                        .entry(key.to)
                        .or_default()
                        .push((a, weight, strength)),
                    _ => {}
                }
                continue;
            }
            if opts.exclude_kinds.iter().any(|x| x == kind) {
                continue;
            }
            let Some(&b) = node_of.get(&key.to) else {
                continue;
            };
            if a == b {
                continue;
            }
            raw.push(RawEdge {
                a,
                b,
                kind: kind_index[kind],
                weight,
                strength,
            });
        }
        let mut skipped_contracts = 0u64;
        let via = kind_index[CONTRACT_EDGE_KIND];
        for (cid, cs) in &consumers {
            let Some(ps) = providers.get(cid) else {
                continue;
            };
            if cs.len() * ps.len() > CONTRACT_EDGE_CAP {
                skipped_contracts += 1;
                continue;
            }
            for &(a, wa, sa) in cs {
                for &(b, wb, sb) in ps {
                    if a != b {
                        raw.push(RawEdge {
                            a,
                            b,
                            kind: via,
                            weight: wa.min(wb),
                            strength: sa.min(sb),
                        });
                    }
                }
            }
        }

        // Merge duplicates in a fixed order, then build the two adjacency lists.
        raw.sort_by_key(|e| (e.a, e.b, e.kind));
        let mut merged: Vec<(u32, u32, Edge)> = Vec::new();
        for e in raw {
            match merged.last_mut() {
                Some((a, b, m)) if *a == e.a && *b == e.b => {
                    m.weight += e.weight;
                    m.strength = m.strength.max(e.strength);
                }
                _ => merged.push((
                    e.a,
                    e.b,
                    Edge {
                        to: e.b,
                        weight: e.weight,
                        strength: e.strength,
                        kind: e.kind,
                    },
                )),
            }
        }
        let (out_start, out, in_start, inn) = build_csr(nodes.len(), &merged);
        Ok(InsightGraph {
            level: opts.level,
            nodes,
            kinds,
            out_start,
            out,
            in_start,
            inn,
            members,
            file_count,
            file_of,
            sym_node,
            skipped_contracts,
            ungrouped_files,
        })
    }

    /// A graph of the named symbol-level nodes and strong `calls` edges of weight one, for tests.
    #[cfg(test)]
    pub(crate) fn from_pairs(names: &[String], pairs: &[(u32, u32)]) -> InsightGraph {
        let mut merged: Vec<(u32, u32, Edge)> = pairs
            .iter()
            .map(|&(a, b)| {
                (
                    a,
                    b,
                    Edge {
                        to: b,
                        weight: 1.0,
                        strength: Strength::Strong,
                        kind: 0,
                    },
                )
            })
            .collect();
        merged.sort_by_key(|e| (e.0, e.1));
        merged.dedup_by_key(|e| (e.0, e.1));
        let n = names.len();
        let (out_start, out, in_start, inn) = build_csr(n, &merged);
        InsightGraph {
            level: Level::Symbol,
            nodes: names
                .iter()
                .map(|name| NodeHit {
                    level: Level::Symbol,
                    name: name.clone(),
                    kind: "fn".into(),
                    path: None,
                    line: None,
                })
                .collect(),
            kinds: vec!["calls".to_string()],
            out_start,
            out,
            in_start,
            inn,
            members: vec![1; n],
            file_count: vec![1; n],
            file_of: (0..n as u32).collect(),
            sym_node: HashMap::new(),
            skipped_contracts: 0,
            ungrouped_files: 0,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.nodes.len()
    }

    /// The edges leaving `v`, sorted by target.
    pub(crate) fn out_edges(&self, v: u32) -> &[Edge] {
        &self.out[self.out_start[v as usize] as usize..self.out_start[v as usize + 1] as usize]
    }

    /// The edges arriving at `v`, as edges whose `to` is the source, sorted by source.
    pub(crate) fn in_edges(&self, v: u32) -> &[Edge] {
        &self.inn[self.in_start[v as usize] as usize..self.in_start[v as usize + 1] as usize]
    }

    /// Edges in total.
    #[cfg(test)]
    pub(crate) fn edge_count(&self) -> usize {
        self.out.len()
    }

    /// PageRank of every node: the symbol's score at symbol level, the sum over the symbols of
    /// the node otherwise. Uses the cached rank when it is current.
    pub(crate) fn node_ranks(&self, tx: &impl Reader) -> Result<Vec<f64>, StoreError> {
        let ranked = katalogr_query::rank_cached_or_live(tx)?;
        let mut out = vec![0.0f64; self.len()];
        if self.level == Level::Symbol {
            let index: HashMap<&str, usize> = self
                .nodes
                .iter()
                .enumerate()
                .map(|(i, n)| (n.name.as_str(), i))
                .collect();
            for (hit, score) in &ranked {
                if let Some(&i) = index.get(hit.name.as_str()) {
                    out[i] = *score;
                }
            }
        } else {
            for (hit, score) in &ranked {
                if let Some(n) = self.node_of_symbol(&hit.name) {
                    out[n as usize] += *score;
                }
            }
        }
        Ok(out)
    }

    /// The node that holds the symbol with canonical name `name` (file and package level).
    pub(crate) fn node_of_symbol(&self, name: &str) -> Option<u32> {
        match self.level {
            Level::Symbol => None,
            _ => self.sym_node.get(name).copied(),
        }
    }

    /// The undirected graph: the weights of both directions added.
    pub(crate) fn undirected(&self) -> Undirected {
        let mut edges: Vec<(u32, u32, f64)> = Vec::with_capacity(self.out.len());
        for v in 0..self.len() as u32 {
            for e in self.out_edges(v) {
                edges.push((v.min(e.to), v.max(e.to), e.weight));
            }
        }
        Undirected::from_edges(self.len(), edges, &vec![0.0; self.len()])
    }
}

/// An undirected weighted graph in adjacency form. `self_w` holds the weight of each node's
/// self-loop (zero for the insight graph; aggregation fills it).
#[derive(Clone, Debug)]
pub(crate) struct Undirected {
    pub start: Vec<u32>,
    pub adj: Vec<(u32, f64)>,
    pub self_w: Vec<f64>,
    /// Weighted degree: neighbours' weights plus twice the self-loop.
    pub deg: Vec<f64>,
    /// Twice the total weight: the sum of the degrees.
    pub m2: f64,
}

impl Undirected {
    /// Build from `(a, b, w)` triples with `a <= b` (parallel edges are added; `a == b` is a
    /// self-loop added to `self_w`). Neighbours are sorted.
    pub(crate) fn from_edges(
        n: usize,
        mut edges: Vec<(u32, u32, f64)>,
        self_w: &[f64],
    ) -> Undirected {
        edges.sort_by_key(|e| (e.0, e.1));
        let mut self_w = self_w.to_vec();
        let mut merged: Vec<(u32, u32, f64)> = Vec::new();
        for (a, b, w) in edges {
            if a == b {
                self_w[a as usize] += w;
                continue;
            }
            match merged.last_mut() {
                Some((x, y, m)) if *x == a && *y == b => *m += w,
                _ => merged.push((a, b, w)),
            }
        }
        let mut start = vec![0u32; n + 1];
        for (a, b, _) in &merged {
            start[*a as usize + 1] += 1;
            start[*b as usize + 1] += 1;
        }
        for i in 0..n {
            start[i + 1] += start[i];
        }
        let mut adj = vec![(0u32, 0.0f64); merged.len() * 2];
        let mut fill = start.clone();
        // `merged` is sorted by (a, b); for each node the neighbours below it arrive first (as
        // `b` of earlier `a`), then those above it, so every list ends up sorted.
        let mut by_b: Vec<&(u32, u32, f64)> = merged.iter().collect();
        by_b.sort_by_key(|e| (e.1, e.0));
        for (a, b, w) in by_b {
            let slot = fill[*b as usize] as usize;
            fill[*b as usize] += 1;
            adj[slot] = (*a, *w);
        }
        for (a, b, w) in &merged {
            let slot = fill[*a as usize] as usize;
            fill[*a as usize] += 1;
            adj[slot] = (*b, *w);
        }
        let mut deg = vec![0.0f64; n];
        for v in 0..n {
            let s: f64 = adj[start[v] as usize..start[v + 1] as usize]
                .iter()
                .map(|(_, w)| *w)
                .sum();
            deg[v] = s + 2.0 * self_w[v];
        }
        let m2 = deg.iter().sum();
        Undirected {
            start,
            adj,
            self_w,
            deg,
            m2,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.deg.len()
    }

    pub(crate) fn neighbours(&self, v: u32) -> &[(u32, f64)] {
        &self.adj[self.start[v as usize] as usize..self.start[v as usize + 1] as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::GraphOptions;
    use crate::testkit::Fx;

    fn opts(level: Level, inferred: bool) -> GraphOptions {
        GraphOptions::new(level, inferred)
    }

    /// `(from name, to name, weight)` of every edge, by node names.
    fn edges(g: &InsightGraph) -> Vec<(String, String, f64, String)> {
        let mut out = Vec::new();
        for v in 0..g.len() as u32 {
            for e in g.out_edges(v) {
                out.push((
                    g.nodes[v as usize].name.clone(),
                    g.nodes[e.to as usize].name.clone(),
                    e.weight,
                    g.kinds[e.kind as usize].clone(),
                ));
            }
        }
        out
    }

    #[test]
    fn levels_aggregate_edges() {
        let mut fx = Fx::new();
        fx.file("f1.tx", Some("p1"));
        fx.file("f2.tx", Some("p2"));
        let a = fx.sym("f1.tx", "a");
        let b = fx.sym("f1.tx", "b");
        let c = fx.sym("f2.tx", "c");
        fx.link(a, "calls", b);
        fx.link(a, "calls", c);
        fx.link(b, "calls", c);
        fx.link(c, "calls", a);
        // A structural link is never an edge.
        fx.link(a, "contains", b);
        let built = fx.build();
        let tx = built.read();

        let g = InsightGraph::load(&tx, &opts(Level::Symbol, false)).unwrap();
        assert_eq!(g.len(), 3);
        assert_eq!(g.edge_count(), 4);
        assert_eq!(g.kinds, ["calls", "via-contract"]);

        let g = InsightGraph::load(&tx, &opts(Level::File, false)).unwrap();
        assert_eq!(
            edges(&g),
            [
                (
                    "r:f1.tx".to_string(),
                    "r:f2.tx".to_string(),
                    2.0,
                    "calls".to_string()
                ),
                (
                    "r:f2.tx".to_string(),
                    "r:f1.tx".to_string(),
                    1.0,
                    "calls".to_string()
                ),
            ]
        );
        assert_eq!(g.members, [2, 1]);
        assert_eq!(g.node_of_symbol("r:f1.tx#fn:a"), Some(0));

        let g = InsightGraph::load(&tx, &opts(Level::Package, false)).unwrap();
        assert_eq!(
            edges(&g),
            [
                ("p1".to_string(), "p2".to_string(), 2.0, "calls".to_string()),
                ("p2".to_string(), "p1".to_string(), 1.0, "calls".to_string()),
            ]
        );
        assert_eq!(g.nodes[0].kind, "package");
        assert_eq!(g.ungrouped_files, 0);
        assert_eq!(g.file_count, [1, 1]);

        // The undirected view adds both directions: f1 - f2 has weight 3.
        let u = InsightGraph::load(&tx, &opts(Level::File, false))
            .unwrap()
            .undirected();
        assert_eq!(u.neighbours(0), [(1, 3.0)]);
        assert_eq!(u.neighbours(1), [(0, 3.0)]);
        assert_eq!(u.m2, 6.0);
    }

    #[test]
    fn contract_edges_derived() {
        let mut fx = Fx::new();
        let c1 = fx.sym("a.tx", "consumer");
        let p1 = fx.sym("b.tx", "provider1");
        let p2 = fx.sym("c.tx", "provider2");
        fx.bind(c1, false, "channel", "orders");
        fx.bind(p1, true, "channel", "orders");
        fx.bind(p2, true, "channel", "orders");
        let built = fx.build();
        let tx = built.read();
        let g = InsightGraph::load(&tx, &opts(Level::File, false)).unwrap();
        assert_eq!(
            edges(&g),
            [
                (
                    "r:a.tx".to_string(),
                    "r:b.tx".to_string(),
                    1.0,
                    "via-contract".to_string()
                ),
                (
                    "r:a.tx".to_string(),
                    "r:c.tx".to_string(),
                    1.0,
                    "via-contract".to_string()
                ),
            ]
        );
        assert_eq!(g.skipped_contracts, 0);

        // A contract with too many consumer-provider pairs is skipped and counted.
        let mut fx = Fx::new();
        for i in 0..101 {
            let s = fx.sym("c.tx", &format!("c{i}"));
            fx.bind(s, false, "channel", "big");
        }
        for i in 0..100 {
            let s = fx.sym("p.tx", &format!("p{i}"));
            fx.bind(s, true, "channel", "big");
        }
        let built = fx.build();
        let g = InsightGraph::load(&built.read(), &opts(Level::File, false)).unwrap();
        assert_eq!(g.edge_count(), 0);
        assert_eq!(g.skipped_contracts, 1);
    }

    #[test]
    fn weak_edges_filtered() {
        use katalogr_model::Confidence;
        let mut fx = Fx::new();
        let a = fx.sym("a.tx", "a");
        let b = fx.sym("a.tx", "b");
        let c = fx.sym("a.tx", "c");
        let d = fx.sym("a.tx", "d");
        fx.link(a, "calls", b);
        fx.link_with(a, "calls", c, Confidence::Inferred, None);
        fx.link_with(a, "calls", d, Confidence::Extracted, Some(3));
        let built = fx.build();
        let tx = built.read();
        let strict = InsightGraph::load(&tx, &opts(Level::Symbol, false)).unwrap();
        assert_eq!(strict.edge_count(), 1);
        let loose = InsightGraph::load(&tx, &opts(Level::Symbol, true)).unwrap();
        assert_eq!(loose.edge_count(), 3);
        let weak = (0..loose.len() as u32)
            .flat_map(|v| loose.out_edges(v).iter())
            .filter(|e| e.strength == Strength::Weak)
            .count();
        assert_eq!(weak, 2);
    }
}
