//! Median, nearest-rank percentiles and a five-number summary.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub n: u32,
    pub min: f64,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
}

impl Summary {
    /// All zero, n = 0: the summary of nothing.
    pub fn empty() -> Summary {
        Summary {
            n: 0,
            min: 0.0,
            p50: 0.0,
            p95: 0.0,
            max: 0.0,
        }
    }

    /// Coefficient of variation is computed separately; see [`cv`].
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }
}

fn sorted(xs: &[f64]) -> Vec<f64> {
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    v
}

/// Median: mean of the two middle values for even n. Panics on empty (callers check).
pub fn median(xs: &[f64]) -> f64 {
    assert!(!xs.is_empty(), "median of nothing");
    let v = sorted(xs);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Nearest rank: `sorted[ceil(p/100 * n) - 1]`, p in (0, 100].
pub fn nearest_rank(sorted: &[f64], p: f64) -> f64 {
    assert!(!sorted.is_empty(), "percentile of nothing");
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// p50 is the median; p95 is nearest rank. An empty slice gives [`Summary::empty`].
pub fn summary(xs: &[f64]) -> Summary {
    if xs.is_empty() {
        return Summary::empty();
    }
    let v = sorted(xs);
    Summary {
        n: v.len() as u32,
        min: v[0],
        p50: median(xs),
        p95: nearest_rank(&v, 95.0),
        max: v[v.len() - 1],
    }
}

/// Coefficient of variation (population standard deviation over the mean); 0 for n < 2.
pub fn cv(xs: &[f64]) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let mean = xs.iter().sum::<f64>() / xs.len() as f64;
    if mean == 0.0 {
        return 0.0;
    }
    let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / xs.len() as f64;
    var.sqrt() / mean
}

#[cfg(test)]
#[test]
fn median_and_nearest_rank() {
    let xs: Vec<f64> = (1..=20).map(f64::from).collect();
    assert_eq!(median(&xs), 10.5);
    assert_eq!(nearest_rank(&xs, 95.0), 19.0);
    assert_eq!(nearest_rank(&xs, 100.0), 20.0);
    let s = summary(&xs);
    assert_eq!(
        (s.n, s.min, s.p50, s.p95, s.max),
        (20, 1.0, 10.5, 19.0, 20.0)
    );
    // Odd n, unsorted input.
    assert_eq!(median(&[5.0, 1.0, 3.0]), 3.0);
    assert_eq!(median(&[4.0, 1.0]), 2.5);
    assert_eq!(summary(&[]).n, 0);
    assert_eq!(cv(&[10.0, 10.0]), 0.0);
    assert!(cv(&[9.0, 11.0]) > 0.09);
}
