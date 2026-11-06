//! How hubs, cycles, communities and coverage moved between two snapshots (FEAT-095 R-6).
//!
//! Pure and total over the four insight reports of each side: no reader, no failure. Every list
//! is sorted and capped, and a cut adds one `limit` entry to `partial`.

use crate::context_diff::MetricDelta;
use crate::metrics::metric_key;
use crate::{CommunityReport, CoverageReport, CycleReport, HubReport};
use katalogr_query::result::{Partial, cap};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// One side's four reports.
pub struct InsightSide<'a> {
    pub hubs: &'a HubReport,
    pub communities: &'a CommunityReport,
    pub cycles: &'a CycleReport,
    pub coverage: &'a CoverageReport,
}

/// A hub whose degree or rank moved. A side where it is not listed counts as 0.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HubShift {
    pub name: String,
    pub kind: String,
    pub path: Option<String>,
    pub degree_a: f64,
    pub degree_b: f64,
    pub rank_a: f64,
    pub rank_b: f64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CycleShift {
    pub total_a: u64,
    pub total_b: u64,
    pub cyclic_nodes_a: u64,
    pub cyclic_nodes_b: u64,
    /// Each cycle by its sorted member names, in the report's cap.
    pub added: Vec<Vec<String>>,
    pub removed: Vec<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommunitySummary {
    pub label: String,
    pub dir: Option<String>,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommunityResize {
    pub label: String,
    pub size_a: u64,
    pub size_b: u64,
}

/// Communities matched by label name, because ids are not stable between runs. A community
/// whose label changes shows as one disappeared and one appeared.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CommunityShift {
    pub total_a: u64,
    pub total_b: u64,
    pub modularity_a: f64,
    pub modularity_b: f64,
    pub appeared: Vec<CommunitySummary>,
    pub disappeared: Vec<CommunitySummary>,
    pub resized: Vec<CommunityResize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InsightShifts {
    pub hubs: Vec<HubShift>,
    pub cycles: CycleShift,
    pub communities: CommunityShift,
    pub coverage: Vec<MetricDelta>,
}

fn cut<T>(mut v: Vec<T>, limit: usize, partial: &mut Vec<Partial>) -> Vec<T> {
    if v.len() > limit {
        partial.push(Partial::new(
            cap::LIMIT,
            limit as u64,
            limit as u64,
            Some(v.len() as u64),
        ));
        v.truncate(limit);
    }
    v
}

type HubEntryRef<'a> = (&'a str, &'a Option<String>, f64, f64);

fn hub_index(r: &HubReport) -> BTreeMap<&str, HubEntryRef<'_>> {
    r.items
        .iter()
        .map(|e| {
            (
                e.node.name.as_str(),
                (e.node.kind.as_str(), &e.node.path, e.degree, e.rank),
            )
        })
        .collect()
}

fn hub_shifts(a: &HubReport, b: &HubReport) -> Vec<HubShift> {
    let (ia, ib) = (hub_index(a), hub_index(b));
    let names: BTreeSet<&str> = ia.keys().chain(ib.keys()).copied().collect();
    let mut out: Vec<HubShift> = names
        .into_iter()
        .filter_map(|n| {
            let (ea, eb) = (ia.get(n), ib.get(n));
            let (da, ra) = ea.map_or((0.0, 0.0), |e| (e.2, e.3));
            let (db, rb) = eb.map_or((0.0, 0.0), |e| (e.2, e.3));
            if da == db && ra == rb {
                return None;
            }
            let src = eb.or(ea).expect("a name comes from one side");
            Some(HubShift {
                name: n.to_string(),
                kind: src.0.to_string(),
                path: src.1.clone(),
                degree_a: da,
                degree_b: db,
                rank_a: ra,
                rank_b: rb,
            })
        })
        .collect();
    out.sort_by(|x, y| {
        let (dx, dy) = (
            (x.degree_b - x.degree_a).abs(),
            (y.degree_b - y.degree_a).abs(),
        );
        dy.total_cmp(&dx).then_with(|| x.name.cmp(&y.name))
    });
    out
}

fn cycle_keys(r: &CycleReport) -> BTreeSet<Vec<String>> {
    r.items
        .iter()
        .map(|c| {
            let mut m: Vec<String> = c.members.iter().map(|n| n.name.clone()).collect();
            m.sort();
            m
        })
        .collect()
}

type Sizes<'a> = BTreeMap<&'a str, (u64, &'a Option<String>)>;

fn community_sizes(r: &CommunityReport) -> Sizes<'_> {
    let mut out: Sizes<'_> = BTreeMap::new();
    for c in &r.items {
        let e = out.entry(c.label.name.as_str()).or_insert((0, &c.dir));
        e.0 += c.size;
    }
    out
}

/// Pure and total. Each list is sorted and capped at `limit`, and a cut adds to `partial`.
pub fn insight_shifts(
    a: &InsightSide,
    b: &InsightSide,
    limit: usize,
    partial: &mut Vec<Partial>,
) -> InsightShifts {
    let hubs = cut(hub_shifts(a.hubs, b.hubs), limit, partial);

    let (ka, kb) = (cycle_keys(a.cycles), cycle_keys(b.cycles));
    let added: Vec<Vec<String>> = kb.difference(&ka).cloned().collect();
    let removed: Vec<Vec<String>> = ka.difference(&kb).cloned().collect();
    let cycles = CycleShift {
        total_a: a.cycles.cycles_total,
        total_b: b.cycles.cycles_total,
        cyclic_nodes_a: a.cycles.cyclic_nodes,
        cyclic_nodes_b: b.cycles.cyclic_nodes,
        added: cut(added, limit, partial),
        removed: cut(removed, limit, partial),
    };

    let (sa, sb) = (
        community_sizes(a.communities),
        community_sizes(b.communities),
    );
    let summary = |label: &str, e: &(u64, &Option<String>)| CommunitySummary {
        label: label.to_string(),
        dir: e.1.clone(),
        size: e.0,
    };
    let appeared: Vec<CommunitySummary> = sb
        .iter()
        .filter(|(l, _)| !sa.contains_key(*l))
        .map(|(l, e)| summary(l, e))
        .collect();
    let disappeared: Vec<CommunitySummary> = sa
        .iter()
        .filter(|(l, _)| !sb.contains_key(*l))
        .map(|(l, e)| summary(l, e))
        .collect();
    let resized: Vec<CommunityResize> = sa
        .iter()
        .filter_map(|(l, ea)| {
            let eb = sb.get(l)?;
            (ea.0 != eb.0).then(|| CommunityResize {
                label: (*l).to_string(),
                size_a: ea.0,
                size_b: eb.0,
            })
        })
        .collect();
    let communities = CommunityShift {
        total_a: a.communities.communities_total,
        total_b: b.communities.communities_total,
        modularity_a: a.communities.modularity,
        modularity_b: b.communities.modularity,
        appeared: cut(appeared, limit, partial),
        disappeared: cut(disappeared, limit, partial),
        resized: cut(resized, limit, partial),
    };

    let samples = |r: &CoverageReport| -> BTreeMap<String, (f64, crate::metrics::Better)> {
        r.metrics
            .iter()
            .filter(|m| m.name.starts_with("coverage."))
            .map(|m| (metric_key(m), (m.value, m.better)))
            .collect()
    };
    let (ca, cb) = (samples(a.coverage), samples(b.coverage));
    let keys: BTreeSet<&String> = ca.keys().chain(cb.keys()).collect();
    let deltas: Vec<MetricDelta> = keys
        .into_iter()
        .filter_map(|k| {
            let (va, vb) = (ca.get(k), cb.get(k));
            let (x, y) = (va.map_or(0.0, |v| v.0), vb.map_or(0.0, |v| v.0));
            if x == y {
                return None;
            }
            Some(MetricDelta {
                name: k.clone(),
                a: x,
                b: y,
                delta: y - x,
                better: vb.or(va).expect("a key comes from one side").1,
            })
        })
        .collect();
    let coverage = cut(deltas, limit, partial);

    InsightShifts {
        hubs,
        cycles,
        communities,
        coverage,
    }
}
