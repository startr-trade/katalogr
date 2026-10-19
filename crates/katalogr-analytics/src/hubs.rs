//! Hubs: the nodes with the highest weighted degree (FEAT-030 section 6.2).

use crate::communities::round;
use crate::graph::InsightGraph;
use crate::options::HubOptions;
use crate::result::{HubEntry, HubReport};
use katalogr_store::{Reader, StoreError};

/// Mean and population standard deviation of the degrees that are above zero.
fn mean_and_sigma(degree: &[f64]) -> (f64, f64) {
    let linked: Vec<f64> = degree.iter().copied().filter(|d| *d > 0.0).collect();
    if linked.is_empty() {
        return (0.0, 0.0);
    }
    let n = linked.len() as f64;
    let mean = linked.iter().sum::<f64>() / n;
    let var = linked.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / n;
    (mean, var.sqrt())
}

/// The z-score of `degree`: zero when the degrees do not vary.
fn z_score(degree: f64, mean: f64, sigma: f64) -> f64 {
    if sigma > 0.0 {
        (degree - mean) / sigma
    } else {
        0.0
    }
}

/// Nodes whose weighted degree (in plus out) reaches `min_degree`, ordered by degree, then
/// rank, then name; the first `limit`.
pub fn hubs(tx: &(impl Reader + Sync), opts: &HubOptions) -> Result<HubReport, StoreError> {
    let g = InsightGraph::load(tx, &opts.graph)?;
    let n = g.len();
    let mut in_w = vec![0.0f64; n];
    let mut out_w = vec![0.0f64; n];
    for v in 0..n as u32 {
        out_w[v as usize] = g.out_edges(v).iter().map(|e| e.weight).sum();
        in_w[v as usize] = g.in_edges(v).iter().map(|e| e.weight).sum();
    }
    let degree: Vec<f64> = (0..n).map(|i| in_w[i] + out_w[i]).collect();
    let (mean, sigma) = mean_and_sigma(&degree);
    let mut picked: Vec<u32> = (0..n as u32)
        .filter(|v| degree[*v as usize] >= opts.min_degree && degree[*v as usize] > 0.0)
        .collect();
    let ranks = if picked.is_empty() {
        vec![0.0; n]
    } else {
        g.node_ranks(tx)?
    };
    picked.sort_by(|a, b| {
        degree[*b as usize]
            .total_cmp(&degree[*a as usize])
            .then_with(|| ranks[*b as usize].total_cmp(&ranks[*a as usize]))
            .then_with(|| g.nodes[*a as usize].name.cmp(&g.nodes[*b as usize].name))
    });
    let items = picked
        .into_iter()
        .take(opts.limit)
        .map(|v| {
            let i = v as usize;
            HubEntry {
                node: g.nodes[i].clone(),
                in_degree: round(in_w[i], 2),
                out_degree: round(out_w[i], 2),
                degree: round(degree[i], 2),
                z: round(z_score(degree[i], mean, sigma), 2),
                rank: round(ranks[i], 6),
                members: g.members[i],
            }
        })
        .collect();
    Ok(HubReport {
        level: g.level,
        nodes: n as u64,
        mean_degree: round(mean, 2),
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::GraphOptions;
    use crate::result::Level;
    use crate::testkit::Fx;

    #[test]
    fn z_score_and_order() {
        let (mean, sigma) = mean_and_sigma(&[1.0, 3.0, 0.0, 5.0]);
        assert_eq!(mean, 3.0);
        assert!((sigma - (8.0f64 / 3.0).sqrt()).abs() < 1e-12);
        assert_eq!(z_score(3.0, mean, sigma), 0.0);
        assert_eq!(z_score(4.0, 4.0, 0.0), 0.0);
        assert_eq!(mean_and_sigma(&[0.0, 0.0]), (0.0, 0.0));

        // A hub called from three symbols, a node with one call, and a tie broken by name.
        let mut fx = Fx::new();
        let hub = fx.sym("a.tx", "hub");
        let x = fx.sym("a.tx", "x");
        let y = fx.sym("a.tx", "y");
        let z = fx.sym("a.tx", "z");
        let lone = fx.sym("a.tx", "lone");
        let _isolated = fx.sym("a.tx", "isolated");
        for s in [x, y, z] {
            fx.link(s, "calls", hub);
        }
        fx.link(x, "calls", lone);
        let built = fx.build();
        let opts = HubOptions {
            graph: GraphOptions::new(Level::Symbol, false),
            min_degree: 1.0,
            limit: 10,
        };
        let r = hubs(&built.read(), &opts).unwrap();
        assert_eq!(r.nodes, 6);
        let names: Vec<&str> = r
            .items
            .iter()
            .map(|h| h.node.name.rsplit(':').next().unwrap())
            .collect();
        // hub 3, x 2, then lone/y/z with degree 1 (rank, then name).
        assert_eq!(names[0], "hub");
        assert_eq!(names[1], "x");
        assert_eq!(r.items.len(), 5, "the isolated node has degree 0");
        assert_eq!(
            (
                r.items[0].in_degree,
                r.items[0].out_degree,
                r.items[0].degree
            ),
            (3.0, 0.0, 3.0)
        );
        assert!(r.items[0].z > r.items[1].z && r.items[1].z > r.items[2].z);
        assert!(r.items[2].rank >= r.items[3].rank);
        // The minimum degree filters, and the limit truncates.
        let r = hubs(
            &built.read(),
            &HubOptions {
                min_degree: 2.0,
                ..opts.clone()
            },
        )
        .unwrap();
        assert_eq!(r.items.len(), 2);
        let r = hubs(&built.read(), &HubOptions { limit: 1, ..opts }).unwrap();
        assert_eq!(r.items.len(), 1);
    }
}
