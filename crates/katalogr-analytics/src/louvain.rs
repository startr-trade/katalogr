//! Louvain modularity clustering with a connectivity split (FEAT-030 section 6.4). Everything
//! is deterministic: the visiting order comes from a seeded permutation, ties keep the earlier
//! candidate, and sums run in a fixed order.

use crate::graph::Undirected;
use crate::options::DEFAULT_SEED;

/// Most local-moving passes per level.
pub(crate) const MAX_PASSES: usize = 56;
/// Most aggregation levels.
pub(crate) const MAX_LEVELS: usize = 12;
/// A move must gain more than this.
pub(crate) const EPS: f64 = 1e-12;

/// How much work a partition took.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LouvainStats {
    /// Levels run (each ends in a local-moving phase).
    pub levels: u32,
    /// Most passes any level needed.
    pub max_passes: u32,
}

/// The nodes `0..n` in a seeded random order: Fisher-Yates driven by xorshift64.
pub(crate) fn permutation(n: usize, seed: u64) -> Vec<u32> {
    let mut x = if seed == 0 { DEFAULT_SEED } else { seed };
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut p: Vec<u32> = (0..n as u32).collect();
    for i in (1..n).rev() {
        let j = (next() % (i as u64 + 1)) as usize;
        p.swap(i, j);
    }
    p
}

/// One local-moving phase. Returns the community of every node and the passes run.
fn local_moving(g: &Undirected, seed: u64, resolution: f64) -> (Vec<u32>, usize) {
    let n = g.len();
    let mut comm: Vec<u32> = (0..n as u32).collect();
    let mut tot: Vec<f64> = g.deg.clone();
    if g.m2 <= 0.0 {
        return (comm, 0);
    }
    let order = permutation(n, seed);
    let mut weight_to = vec![0.0f64; n];
    let mut touched: Vec<u32> = Vec::new();
    let mut passes = 0;
    for _ in 0..MAX_PASSES {
        passes += 1;
        let mut moved = false;
        for &i in &order {
            let ki = g.deg[i as usize];
            let old = comm[i as usize];
            for &(j, w) in g.neighbours(i) {
                let c = comm[j as usize];
                if weight_to[c as usize] == 0.0 {
                    touched.push(c);
                }
                weight_to[c as usize] += w;
            }
            tot[old as usize] -= ki;
            touched.sort_unstable();
            let gain = |c: u32, w: f64, tot: &[f64]| w - resolution * tot[c as usize] * ki / g.m2;
            let mut best = old;
            let mut best_gain = gain(old, weight_to[old as usize], &tot);
            for &c in &touched {
                if c == old {
                    continue;
                }
                let gc = gain(c, weight_to[c as usize], &tot);
                if gc > best_gain + EPS {
                    best = c;
                    best_gain = gc;
                }
            }
            tot[best as usize] += ki;
            comm[i as usize] = best;
            if best != old {
                moved = true;
            }
            for &c in &touched {
                weight_to[c as usize] = 0.0;
            }
            touched.clear();
        }
        if !moved {
            break;
        }
    }
    (comm, passes)
}

/// Renumber communities by first appearance in ascending node index.
fn renumber(comm: &mut [u32]) -> usize {
    let mut map = std::collections::HashMap::new();
    for c in comm.iter_mut() {
        let next = map.len() as u32;
        *c = *map.entry(*c).or_insert(next);
    }
    map.len()
}

/// The graph of communities: weights between them summed, internal weight as self-loops.
fn aggregate(g: &Undirected, comm: &[u32], count: usize) -> Undirected {
    let mut edges: Vec<(u32, u32, f64)> = Vec::new();
    for u in 0..g.len() as u32 {
        for &(v, w) in g.neighbours(u) {
            if v > u {
                let (a, b) = (comm[u as usize], comm[v as usize]);
                edges.push((a.min(b), a.max(b), w));
            }
        }
    }
    let mut self_w = vec![0.0f64; count];
    for (v, w) in g.self_w.iter().enumerate() {
        self_w[comm[v] as usize] += w;
    }
    Undirected::from_edges(count, edges, &self_w)
}

/// Louvain partition of `g` with its work statistics. The community of every node, numbered by
/// first appearance.
pub(crate) fn partition_with_stats(
    g: &Undirected,
    seed: u64,
    resolution: f64,
) -> (Vec<u32>, LouvainStats) {
    let n = g.len();
    let mut assign: Vec<u32> = (0..n as u32).collect();
    let mut stats = LouvainStats::default();
    let mut level_graph = g.clone();
    for _ in 0..MAX_LEVELS {
        let (mut comm, passes) = local_moving(&level_graph, seed, resolution);
        stats.levels += 1;
        stats.max_passes = stats.max_passes.max(passes as u32);
        let count = renumber(&mut comm);
        for a in assign.iter_mut() {
            *a = comm[*a as usize];
        }
        if count == level_graph.len() {
            break;
        }
        level_graph = aggregate(&level_graph, &comm, count);
    }
    renumber(&mut assign);
    (assign, stats)
}

/// Louvain partition of `g`; see [`partition_with_stats`].
#[cfg(test)]
pub(crate) fn partition(g: &Undirected, seed: u64, resolution: f64) -> Vec<u32> {
    partition_with_stats(g, seed, resolution).0
}

/// Make every community connected: each connected component of a community (found in
/// ascending node index) becomes its own community. Communities are renumbered by first
/// appearance. Returns how many there are.
pub(crate) fn split_disconnected(g: &Undirected, comm: &mut [u32]) -> usize {
    const NONE: u32 = u32::MAX;
    let n = g.len();
    let mut label = vec![NONE; n];
    let mut next = 0u32;
    let mut queue: Vec<u32> = Vec::new();
    for s in 0..n as u32 {
        if label[s as usize] != NONE {
            continue;
        }
        label[s as usize] = next;
        queue.push(s);
        while let Some(v) = queue.pop() {
            for &(w, _) in g.neighbours(v) {
                if label[w as usize] == NONE && comm[w as usize] == comm[s as usize] {
                    label[w as usize] = next;
                    queue.push(w);
                }
            }
        }
        next += 1;
    }
    comm.copy_from_slice(&label);
    next as usize
}

/// `sum over communities of in_c / m2 - resolution * (tot_c / m2)^2`.
pub(crate) fn modularity(g: &Undirected, comm: &[u32], resolution: f64) -> f64 {
    if g.m2 <= 0.0 {
        return 0.0;
    }
    let count = comm.iter().copied().max().map_or(0, |m| m as usize + 1);
    let mut inside = vec![0.0f64; count];
    let mut tot = vec![0.0f64; count];
    for v in 0..g.len() as u32 {
        let c = comm[v as usize];
        tot[c as usize] += g.deg[v as usize];
        inside[c as usize] += 2.0 * g.self_w[v as usize];
        for &(w, x) in g.neighbours(v) {
            if comm[w as usize] == c {
                inside[c as usize] += x;
            }
        }
    }
    (0..count)
        .map(|c| inside[c] / g.m2 - resolution * (tot[c] / g.m2).powi(2))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn graph(n: usize, edges: &[(u32, u32)]) -> Undirected {
        Undirected::from_edges(
            n,
            edges
                .iter()
                .map(|&(a, b)| (a.min(b), a.max(b), 1.0))
                .collect(),
            &vec![0.0; n],
        )
    }

    fn clique(base: u32, size: u32) -> Vec<(u32, u32)> {
        let mut e = Vec::new();
        for a in 0..size {
            for b in a + 1..size {
                e.push((base + a, base + b));
            }
        }
        e
    }

    #[test]
    fn two_cliques_split() {
        let mut edges = clique(0, 5);
        edges.extend(clique(5, 5));
        edges.push((4, 5));
        let g = graph(10, &edges);
        let mut comm = partition(&g, 1, 1.0);
        let count = split_disconnected(&g, &mut comm);
        assert_eq!(count, 2);
        assert!(comm[..5].iter().all(|c| *c == comm[0]));
        assert!(comm[5..].iter().all(|c| *c == comm[5]));
        assert_ne!(comm[0], comm[5]);
        let q = modularity(&g, &comm, 1.0);
        assert!(q > 0.4, "modularity {q}");
    }

    #[test]
    fn communities_connected() {
        // Two separate edges forced into one community are split in two.
        let g = graph(4, &[(0, 1), (2, 3)]);
        let mut comm = vec![0, 0, 0, 0];
        assert_eq!(split_disconnected(&g, &mut comm), 2);
        assert_eq!(comm, [0, 0, 1, 1]);
        // An isolated node is a community of its own.
        let g = graph(3, &[(0, 1)]);
        let mut comm = vec![0, 0, 0];
        assert_eq!(split_disconnected(&g, &mut comm), 2);
        assert_eq!(comm, [0, 0, 1]);
    }

    #[test]
    fn permutation_is_a_seeded_shuffle() {
        let p = permutation(50, 7);
        assert_eq!(p, permutation(50, 7));
        assert_ne!(p, permutation(50, 8));
        let mut sorted = p.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..50).collect::<Vec<u32>>());
        assert_eq!(permutation(5, 0), permutation(5, DEFAULT_SEED));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn seed_reproducible(
            n in 2usize..200,
            raw in proptest::collection::vec((0u32..200, 0u32..200, 1u32..5), 1..600),
            seed in any::<u64>(),
        ) {
            let edges: Vec<(u32, u32, f64)> = raw
                .iter()
                .map(|&(a, b, w)| (a % n as u32, b % n as u32, f64::from(w)))
                .filter(|(a, b, _)| a != b)
                .map(|(a, b, w)| (a.min(b), a.max(b), w))
                .collect();
            let zero = vec![0.0; n];
            let g1 = Undirected::from_edges(n, edges.clone(), &zero);
            let first = partition(&g1, seed, 1.0);
            prop_assert_eq!(&first, &partition(&g1, seed, 1.0));
            // The same edges inserted in another order give the same partition.
            let mut rev = edges;
            rev.reverse();
            let g2 = Undirected::from_edges(n, rev, &zero);
            prop_assert_eq!(&first, &partition(&g2, seed, 1.0));
            // And splitting keeps communities connected and the partition deterministic.
            let mut a = first.clone();
            let mut b = first;
            split_disconnected(&g1, &mut a);
            split_disconnected(&g2, &mut b);
            prop_assert_eq!(a, b);
        }
    }
}
