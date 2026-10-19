//! `search`: BM25 over names, doc comments, section text and contract keys (FEAT-025).
//!
//! Deterministic: the postings are exact terms from the identifier-aware tokeniser, the field
//! weights are fixed, ties are broken by canonical name. There is no stemming, no fuzziness and
//! no model; the caller supplies the words.

use crate::resolve::hit;
use crate::result::{Matched, SearchHit};
use katalogr_model::{Field, Id, tokenize};
use katalogr_store::{LEXICON_FIELDS, Reader, StoreError};
use std::collections::BTreeMap;

const K1: f64 = 1.2;
const B: f64 = 0.75;

/// Scores are printed with six decimal places.
pub(crate) fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

#[derive(Default)]
struct Acc {
    score: f64,
    /// `(query term index, field index)`.
    matched: Vec<(usize, usize)>,
}

/// The `limit` best symbols for the words in `terms`, best first. A symbol matches when any
/// query term is one of its tokens. Per term, the field-weighted, length-normalised term
/// frequency `t` contributes `idf * t / (k1 + t)`.
pub fn search(tx: &impl Reader, terms: &str, limit: usize) -> Result<Vec<SearchHit>, StoreError> {
    let mut query: Vec<String> = Vec::new();
    for t in tokenize(terms) {
        if !query.contains(&t) {
            query.push(t);
        }
    }
    let stats = tx.lexicon_stats()?;
    if query.is_empty() || stats.docs == 0 {
        return Ok(Vec::new());
    }
    let docs = stats.docs as f64;
    let mut avg = [1.0f64; LEXICON_FIELDS];
    for (a, total) in avg.iter_mut().zip(stats.total_len) {
        if total > 0 {
            *a = total as f64 / docs;
        }
    }
    let mut lens: BTreeMap<Id, [u32; LEXICON_FIELDS]> = BTreeMap::new();
    let mut acc: BTreeMap<Id, Acc> = BTreeMap::new();
    for (qi, term) in query.iter().enumerate() {
        let postings = tx.postings(term)?;
        let mut weighted: BTreeMap<Id, f64> = BTreeMap::new();
        for p in &postings {
            let Some(field) = Field::from_index(p.field as usize) else {
                continue;
            };
            let dl = match lens.get(&p.symbol) {
                Some(l) => l[field.index()],
                None => {
                    let l = tx.doc_len(p.symbol)?.unwrap_or([0; LEXICON_FIELDS]);
                    lens.insert(p.symbol, l);
                    l[field.index()]
                }
            };
            let norm = 1.0 - B + B * f64::from(dl) / avg[field.index()];
            *weighted.entry(p.symbol).or_default() += field.weight() * f64::from(p.tf) / norm;
            acc.entry(p.symbol)
                .or_default()
                .matched
                .push((qi, field.index()));
        }
        let n = weighted.len() as f64;
        let idf = (1.0 + (docs - n + 0.5) / (n + 0.5)).ln();
        for (id, t) in weighted {
            acc.entry(id).or_default().score += idf * t / (K1 + t);
        }
    }
    let mut hits: Vec<(f64, SearchHit)> = Vec::new();
    for (id, a) in acc {
        let Some(row) = tx.symbol(id)? else { continue };
        let mut matched = a.matched;
        matched.sort();
        matched.dedup();
        hits.push((
            a.score,
            SearchHit {
                symbol: hit(&row),
                score: round6(a.score),
                matched: matched
                    .into_iter()
                    .map(|(qi, fi)| Matched {
                        term: query[qi].clone(),
                        field: Field::ALL[fi].label().to_string(),
                    })
                    .collect(),
            },
        ));
    }
    hits.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| a.1.symbol.name.cmp(&b.1.symbol.name))
    });
    hits.truncate(limit);
    Ok(hits.into_iter().map(|(_, h)| h).collect())
}
