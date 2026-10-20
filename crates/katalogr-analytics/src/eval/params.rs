//! Parameters one evaluation run uses, and the sweep grid over them.

use super::EvalError;
use katalogr_query::SearchParams;
use std::collections::BTreeMap;

/// Parameters one run uses.
#[derive(Clone, Debug, PartialEq)]
pub struct EvalParams {
    pub search: SearchParams,
    /// Items per side for `explain`.
    pub per_side: usize,
}

impl Default for EvalParams {
    fn default() -> Self {
        EvalParams {
            search: SearchParams::default(),
            per_side: 10,
        }
    }
}

/// The sweepable keys, in this order.
pub const PARAM_KEYS: &[&str] = &[
    "search.k1",
    "search.b",
    "search.min_prefix",
    "search.max_expansions",
    "search.prefix_factor",
    "search.weight.name",
    "search.weight.doc",
    "search.weight.section",
    "search.weight.contract",
    "explain.per_side",
];

pub const MAX_COMBINATIONS: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub struct SweepParam {
    pub key: String,
    pub values: Vec<f64>,
}

const INTEGER_KEYS: &[&str] = &[
    "search.min_prefix",
    "search.max_expansions",
    "explain.per_side",
];

fn usage(msg: impl Into<String>) -> EvalError {
    EvalError::Usage(msg.into())
}

fn check(key: &str, value: f64) -> Result<(), EvalError> {
    if !PARAM_KEYS.contains(&key) {
        return Err(usage(format!(
            "unknown parameter {key:?}; valid keys: {}",
            PARAM_KEYS.join(", ")
        )));
    }
    if !value.is_finite() || value < 0.0 {
        return Err(usage(format!(
            "{key}: {value} must be a finite, non-negative number"
        )));
    }
    if INTEGER_KEYS.contains(&key) && value.fract() != 0.0 {
        return Err(usage(format!("{key}: {value} must be an integer")));
    }
    Ok(())
}

/// `KEY=V1,V2,...`.
pub fn parse_sweep(spec: &str) -> Result<SweepParam, EvalError> {
    let (key, list) = spec
        .split_once('=')
        .ok_or_else(|| usage(format!("{spec:?}: expected KEY=V1,V2,...")))?;
    let key = key.trim();
    if list.trim().is_empty() {
        return Err(usage(format!("{key}: no values given")));
    }
    let mut values = Vec::new();
    for v in list.split(',') {
        let v = v.trim();
        let n: f64 = v
            .parse()
            .map_err(|_| usage(format!("{key}: {v:?} is not a number")))?;
        check(key, n)?;
        values.push(n);
    }
    Ok(SweepParam {
        key: key.to_string(),
        values,
    })
}

/// Set one key on `p` (same validation as `parse_sweep`).
pub fn apply(p: &mut EvalParams, key: &str, value: f64) -> Result<(), EvalError> {
    check(key, value)?;
    match key {
        "search.k1" => p.search.k1 = value,
        "search.b" => p.search.b = value,
        "search.min_prefix" => p.search.min_prefix = value as usize,
        "search.max_expansions" => p.search.max_expansions = value as usize,
        "search.prefix_factor" => p.search.prefix_factor = value,
        "search.weight.name" => p.search.weights[0] = value,
        "search.weight.doc" => p.search.weights[1] = value,
        "search.weight.section" => p.search.weights[2] = value,
        "search.weight.contract" => p.search.weights[3] = value,
        "explain.per_side" => p.per_side = value as usize,
        _ => unreachable!("check accepts only PARAM_KEYS"),
    }
    Ok(())
}

/// Every parameter as `key -> value`, for reports and baselines.
pub fn describe(p: &EvalParams) -> BTreeMap<String, f64> {
    PARAM_KEYS
        .iter()
        .map(|k| {
            let v = match *k {
                "search.k1" => p.search.k1,
                "search.b" => p.search.b,
                "search.min_prefix" => p.search.min_prefix as f64,
                "search.max_expansions" => p.search.max_expansions as f64,
                "search.prefix_factor" => p.search.prefix_factor,
                "search.weight.name" => p.search.weights[0],
                "search.weight.doc" => p.search.weights[1],
                "search.weight.section" => p.search.weights[2],
                "search.weight.contract" => p.search.weights[3],
                _ => p.per_side as f64,
            };
            (k.to_string(), v)
        })
        .collect()
}

/// Cartesian product in the order given (first key slowest), starting from `base`.
pub fn grid(base: &EvalParams, sweep: &[SweepParam]) -> Result<Vec<EvalParams>, EvalError> {
    for (i, s) in sweep.iter().enumerate() {
        if sweep[..i].iter().any(|o| o.key == s.key) {
            return Err(usage(format!("{}: given twice", s.key)));
        }
    }
    let mut total: usize = 1;
    for s in sweep {
        total = total.saturating_mul(s.values.len());
        if total > MAX_COMBINATIONS {
            return Err(usage(format!(
                "more than {MAX_COMBINATIONS} combinations; narrow the ranges"
            )));
        }
    }
    let mut out = vec![base.clone()];
    for s in sweep {
        let mut next = Vec::with_capacity(out.len() * s.values.len());
        for p in &out {
            for v in &s.values {
                let mut q = p.clone();
                apply(&mut q, &s.key, *v)?;
                next.push(q);
            }
        }
        out = next;
    }
    Ok(out)
}
