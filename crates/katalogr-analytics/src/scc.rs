//! Strongly connected components and witness cycles (FEAT-030 section 6.3).

use crate::graph::{InsightGraph, Strength};

/// Every strongly connected component of two or more nodes, found with an iterative Tarjan
/// (an explicit stack, so a component of 100,000 nodes cannot overflow the call stack). Nodes
/// of a component are in discovery order; components are in completion order. A single node
/// is a component only through a self-loop, which the graph never holds.
pub(crate) fn tarjan(g: &InsightGraph) -> Vec<Vec<u32>> {
    const NONE: u32 = u32::MAX;
    let n = g.len();
    let mut index = vec![NONE; n];
    let mut low = vec![0u32; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<u32> = Vec::new();
    let mut call: Vec<(u32, u32)> = Vec::new();
    let mut next_index = 0u32;
    let mut out: Vec<Vec<u32>> = Vec::new();
    for root in 0..n as u32 {
        if index[root as usize] != NONE {
            continue;
        }
        call.push((root, 0));
        while let Some(&(v, i)) = call.last() {
            let vi = v as usize;
            if i == 0 && index[vi] == NONE {
                index[vi] = next_index;
                low[vi] = next_index;
                next_index += 1;
                stack.push(v);
                on_stack[vi] = true;
            }
            let edges = g.out_edges(v);
            if (i as usize) < edges.len() {
                call.last_mut().expect("a frame").1 = i + 1;
                let w = edges[i as usize].to;
                let wi = w as usize;
                if index[wi] == NONE {
                    call.push((w, 0));
                } else if on_stack[wi] {
                    low[vi] = low[vi].min(index[wi]);
                }
            } else {
                call.pop();
                if let Some(&(parent, _)) = call.last() {
                    let pi = parent as usize;
                    low[pi] = low[pi].min(low[vi]);
                }
                if low[vi] == index[vi] {
                    let mut comp = Vec::new();
                    loop {
                        let w = stack.pop().expect("v is on the stack");
                        on_stack[w as usize] = false;
                        comp.push(w);
                        if w == v {
                            break;
                        }
                    }
                    if comp.len() >= 2 {
                        comp.reverse();
                        out.push(comp);
                    }
                }
            }
        }
    }
    out
}

/// One step of a witness: `(from, kind, to, strength)` with node indices and a kind index.
pub(crate) type Step = (u32, u16, u32, Strength);

/// The shortest cycle through the member of `comp` with the smallest canonical name: a
/// breadth-first search from it over the edges inside the component, visiting targets in
/// adjacency order, until an edge leads back. The steps start and end at that member.
pub(crate) fn witness(g: &InsightGraph, comp: &[u32]) -> Vec<Step> {
    let mut sorted: Vec<u32> = comp.to_vec();
    sorted.sort_unstable();
    let inside = |v: u32| sorted.binary_search(&v).is_ok();
    let Some(&start) = comp
        .iter()
        .min_by(|a, b| g.nodes[**a as usize].name.cmp(&g.nodes[**b as usize].name))
    else {
        return Vec::new();
    };
    // node -> (predecessor, the step that reached it)
    let mut reached: std::collections::HashMap<u32, Step> = std::collections::HashMap::new();
    let mut queue = std::collections::VecDeque::from([start]);
    while let Some(v) = queue.pop_front() {
        for e in g.out_edges(v) {
            if !inside(e.to) {
                continue;
            }
            if e.to == start {
                let mut path = vec![(v, e.kind, start, e.strength)];
                let mut at = v;
                while at != start {
                    let step = reached[&at];
                    path.push(step);
                    at = step.0;
                }
                path.reverse();
                return path;
            }
            if let std::collections::hash_map::Entry::Vacant(slot) = reached.entry(e.to) {
                slot.insert((v, e.kind, e.to, e.strength));
                queue.push_back(e.to);
            }
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("n{i:05}")).collect()
    }

    fn sorted(mut comps: Vec<Vec<u32>>) -> Vec<Vec<u32>> {
        for c in &mut comps {
            c.sort_unstable();
        }
        comps.sort();
        comps
    }

    #[test]
    fn tarjan_finds_components() {
        // 0 -> 1 -> 2 -> 0, and 3 <-> 4; 2 -> 3 and 5 hang off as a chain without a cycle.
        let pairs = [
            (0, 1),
            (1, 2),
            (2, 0),
            (3, 4),
            (4, 3),
            (2, 3),
            (4, 5),
            (5, 6),
        ];
        let g = InsightGraph::from_pairs(&names(7), &pairs);
        assert_eq!(sorted(tarjan(&g)), vec![vec![0, 1, 2], vec![3, 4]]);

        // A cycle through 10,000 nodes, in a chain, does not overflow the stack.
        let n = 10_000u32;
        let pairs: Vec<(u32, u32)> = (0..n).map(|i| (i, (i + 1) % n)).collect();
        let g = InsightGraph::from_pairs(&names(n as usize), &pairs);
        let comps = tarjan(&g);
        assert_eq!(comps.len(), 1);
        assert_eq!(comps[0].len(), n as usize);

        // No edges, no components.
        assert!(tarjan(&InsightGraph::from_pairs(&names(3), &[])).is_empty());
    }

    #[test]
    fn witness_is_the_shortest_cycle_through_the_first_name() {
        // n0 -> n1 -> n2 -> n3 -> n0 is long; n0 -> n2 -> n3 -> n0 is shorter.
        let pairs = [(0, 1), (1, 2), (2, 3), (3, 0), (0, 2)];
        let g = InsightGraph::from_pairs(&names(4), &pairs);
        let comps = tarjan(&g);
        let w = witness(&g, &comps[0]);
        let path: Vec<(u32, u32)> = w.iter().map(|s| (s.0, s.2)).collect();
        assert_eq!(path, [(0, 2), (2, 3), (3, 0)]);
    }
}
