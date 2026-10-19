//! Communities: Louvain clusters of the undirected usage graph (FEAT-030 section 6.4).

use crate::graph::{InsightGraph, common_dir, common_prefix};
use crate::louvain::{LouvainStats, modularity, partition_with_stats, split_disconnected};
use crate::options::CommunityOptions;
use crate::result::{Community, CommunityReport, Level, NodeHit};
use katalogr_store::{Reader, StoreError};
use std::cmp::Ordering;

/// Round to `dp` decimal places.
pub(crate) fn round(x: f64, dp: i32) -> f64 {
    let f = 10f64.powi(dp);
    (x * f).round() / f
}

/// The communities of the usage graph at the chosen level, with the modularity of the
/// partition. A community is connected; isolated nodes belong to none.
pub fn communities(
    tx: &impl Reader,
    opts: &CommunityOptions,
) -> Result<CommunityReport, StoreError> {
    communities_with_stats(tx, opts).map(|(report, _)| report)
}

/// Like [`communities`], also returning how much work the clustering took.
#[doc(hidden)]
pub fn communities_with_stats(
    tx: &impl Reader,
    opts: &CommunityOptions,
) -> Result<(CommunityReport, LouvainStats), StoreError> {
    let g = InsightGraph::load(tx, &opts.graph)?;
    let u = g.undirected();
    let (mut comm, stats) = partition_with_stats(&u, opts.seed, opts.resolution);
    let count = split_disconnected(&u, &mut comm);
    let q = round(modularity(&u, &comm, opts.resolution), 4);
    let ranks = g.node_ranks(tx)?;

    // Members of each community, isolated nodes left out.
    let mut members: Vec<Vec<u32>> = vec![Vec::new(); count];
    let mut isolated = 0u64;
    for v in 0..g.len() as u32 {
        if u.deg[v as usize] > 0.0 {
            members[comm[v as usize] as usize].push(v);
        } else {
            isolated += 1;
        }
    }
    let by_rank = |a: &u32, b: &u32| -> Ordering {
        ranks[*b as usize]
            .total_cmp(&ranks[*a as usize])
            .then_with(|| g.nodes[*a as usize].name.cmp(&g.nodes[*b as usize].name))
    };
    let mut kept: Vec<(u32, Vec<u32>)> = Vec::new();
    let mut unclustered = isolated;
    for (c, mut m) in members.into_iter().enumerate() {
        if m.is_empty() {
            continue;
        }
        if m.len() < opts.min_size {
            unclustered += m.len() as u64;
            continue;
        }
        m.sort_by(by_rank);
        kept.push((c as u32, m));
    }
    // Largest first, then by label (the best-ranked member).
    kept.sort_by(|a, b| {
        b.1.len().cmp(&a.1.len()).then_with(|| {
            g.nodes[a.1[0] as usize]
                .name
                .cmp(&g.nodes[b.1[0] as usize].name)
        })
    });
    let communities_total = kept.len() as u64;

    let mut note = None;
    let wanted = match &opts.of {
        None => None,
        Some(name) => match g.nodes.iter().position(|n| &n.name == name) {
            Some(i) => Some(i as u32),
            None => {
                note = Some(format!(
                    "no node named {name} at {} level",
                    level_name(g.level)
                ));
                kept.clear();
                None
            }
        },
    };

    let mut items = Vec::new();
    for (i, (_, m)) in kept.iter().enumerate() {
        if let Some(w) = wanted
            && !m.contains(&w)
        {
            continue;
        }
        if items.len() >= opts.limit {
            break;
        }
        items.push(describe(&g, &u, &comm, m, i as u32 + 1, opts.members));
    }
    // `of` names a node that is in no reported community (isolated or too small).
    if wanted.is_some() && items.is_empty() {
        note = Some("the node is in no reported community".to_string());
    }
    Ok((
        CommunityReport {
            level: g.level,
            seed: opts.seed,
            resolution: opts.resolution,
            modularity: q,
            communities_total,
            unclustered,
            note,
            items,
        },
        stats,
    ))
}

fn level_name(l: Level) -> &'static str {
    match l {
        Level::Symbol => "symbol",
        Level::File => "file",
        Level::Package => "package",
    }
}

/// One community: `m` is sorted by rank (best first).
fn describe(
    g: &InsightGraph,
    u: &crate::graph::Undirected,
    comm: &[u32],
    m: &[u32],
    id: u32,
    top: usize,
) -> Community {
    let c = comm[m[0] as usize];
    let mut internal = 0.0f64;
    let mut boundary = 0.0f64;
    let mut edge_out: Vec<(f64, u32)> = Vec::new();
    for &v in m {
        let mut leaving = 0.0f64;
        for &(w, x) in u.neighbours(v) {
            if comm[w as usize] == c {
                if w > v {
                    internal += x;
                }
            } else {
                leaving += x;
            }
        }
        internal += u.self_w[v as usize];
        boundary += leaving;
        if leaving > 0.0 {
            edge_out.push((leaving, v));
        }
    }
    // Bridges: the members with the most weight leaving the community, then by name.
    edge_out.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| g.nodes[a.1 as usize].name.cmp(&g.nodes[b.1 as usize].name))
    });
    let hit = |v: u32| -> NodeHit { g.nodes[v as usize].clone() };
    let files = if g.level == Level::Symbol {
        let mut f: Vec<u32> = m.iter().map(|v| g.file_of[*v as usize]).collect();
        f.sort_unstable();
        f.dedup();
        f.len() as u64
    } else {
        m.iter().map(|v| u64::from(g.file_count[*v as usize])).sum()
    };
    let dir = if g.level == Level::Package {
        // A package node holds its common directory already (none when its files span
        // repositories), so the directories are compared as they are.
        let dirs: Option<Vec<&str>> = m
            .iter()
            .map(|v| g.nodes[*v as usize].path.as_deref())
            .collect();
        dirs.and_then(common_prefix)
    } else if m.iter().all(|v| g.nodes[*v as usize].path.is_some()) {
        common_dir(m.iter().filter_map(|v| {
            let n = &g.nodes[*v as usize];
            Some((n.name.split(':').next()?, n.path.as_deref()?))
        }))
    } else {
        None
    };
    let total = internal + boundary;
    Community {
        id,
        size: m.len() as u64,
        label: hit(m[0]),
        dir,
        cohesion: round(if total > 0.0 { internal / total } else { 0.0 }, 3),
        internal_weight: round(internal, 2),
        boundary_weight: round(boundary, 2),
        top: m.iter().take(top).map(|v| hit(*v)).collect(),
        bridges: edge_out.iter().take(3).map(|(_, v)| hit(*v)).collect(),
        files,
    }
}
