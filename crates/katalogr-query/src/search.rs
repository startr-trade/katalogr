//! `search`: BM25 over names, doc comments, section text and contract keys (FEAT-025).
//!
//! Deterministic: the postings are terms from the identifier-aware tokeniser, the field
//! weights are fixed, ties are broken by canonical name. A token of 3+ characters also matches
//! the indexed terms it prefixes (FEAT-055). There is no stemming, no fuzziness and no model.

use crate::diagnostic::{
    DIAG_TERMS, DIAG_TOP, Diagnostic, ExpandedTerm, PartTrace, PrefixTrace, RankTrace,
    RoutingTrace, TermTrace,
};
use crate::resolve::hit;
use crate::result::{Matched, SearchHit};
use katalogr_model::{Field, Id, tokenize};
use katalogr_store::{LEXICON_FIELDS, Posting, Reader, StoreError};
use std::collections::{BTreeMap, BTreeSet};

const K1: f64 = 1.2;
const B: f64 = 0.75;
/// A query token of at least this many characters also matches the terms it prefixes.
const MIN_PREFIX: usize = 3;
/// At most this many prefix-expanded terms per query token (most frequent first).
const MAX_EXPANSIONS: usize = 50;
/// A prefix-only term contributes this fraction of its BM25 value.
const PREFIX_FACTOR: f64 = 0.5;

/// Scores are printed with six decimal places.
pub(crate) fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// Per document, the candidate index, `t` and contribution that gave the best value.
type Best = BTreeMap<Id, (usize, f64, f64)>;

#[derive(Default)]
struct Acc {
    score: f64,
    /// `(query term index, field index, through a prefix expansion)`.
    matched: Vec<(usize, usize, bool)>,
}

/// BM25F parameters. `Default` returns exactly the constants `search` has always used.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchParams {
    pub k1: f64,
    pub b: f64,
    /// A query token of at least this many characters also matches the terms it prefixes.
    pub min_prefix: usize,
    pub max_expansions: usize,
    pub prefix_factor: f64,
    /// Per field, indexed by `Field::index()`.
    pub weights: [f64; LEXICON_FIELDS],
}

impl Default for SearchParams {
    fn default() -> Self {
        SearchParams {
            k1: K1,
            b: B,
            min_prefix: MIN_PREFIX,
            max_expansions: MAX_EXPANSIONS,
            prefix_factor: PREFIX_FACTOR,
            weights: Field::ALL.map(Field::weight),
        }
    }
}

/// The `limit` best symbols for the words in `terms`, best first. A symbol matches when any
/// query term is one of its tokens. Per term, the field-weighted, length-normalised term
/// frequency `t` contributes `idf * t / (k1 + t)`.
pub fn search(tx: &impl Reader, terms: &str, limit: usize) -> Result<Vec<SearchHit>, StoreError> {
    search_traced(tx, terms, limit, &mut Diagnostic::off())
}

/// What [`search`] does, recording its routing, retrieval and scoring into `diag`. The answer
/// does not depend on `diag`.
pub fn search_traced(
    tx: &impl Reader,
    terms: &str,
    limit: usize,
    diag: &mut Diagnostic,
) -> Result<Vec<SearchHit>, StoreError> {
    search_with_traced(tx, terms, limit, &SearchParams::default(), diag)
}

/// `search` with explicit parameters. Same ordering and tie-break rules.
pub fn search_with(
    tx: &impl Reader,
    terms: &str,
    limit: usize,
    p: &SearchParams,
) -> Result<Vec<SearchHit>, StoreError> {
    search_with_traced(tx, terms, limit, p, &mut Diagnostic::off())
}

/// What [`search_with`] does, recording its routing, retrieval and scoring into `diag`. The
/// answer does not depend on `diag`.
pub fn search_with_traced(
    tx: &impl Reader,
    terms: &str,
    limit: usize,
    p: &SearchParams,
    diag: &mut Diagnostic,
) -> Result<Vec<SearchHit>, StoreError> {
    let mut query: Vec<String> = Vec::new();
    for t in tokenize(terms) {
        if !query.contains(&t) {
            query.push(t);
        }
    }
    if let Some(d) = diag.on() {
        let dropped = terms
            .split_whitespace()
            .filter(|w| tokenize(w).is_empty())
            .map(str::to_string)
            .collect();
        d.routing = Some(RoutingTrace {
            command: "search".into(),
            input: terms.to_string(),
            kind: if query.is_empty() { "none" } else { "tokens" }.into(),
            tokens: query.clone(),
            dropped,
            tier: None,
            candidates: 0,
            after_collapse: 0,
        });
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
    // Recorded only when tracing: per query token, the term that gave each document its best value.
    let mut sources: Vec<Best> = Vec::new();
    let mut cand_terms: Vec<Vec<(bool, String)>> = Vec::new();
    for (qi, term) in query.iter().enumerate() {
        // The exact term, then (3+ characters) the most frequent indexed terms it prefixes.
        let mut candidates: Vec<(bool, String, Vec<Posting>)> =
            vec![(false, term.clone(), tx.postings(term)?)];
        let attempted = term.chars().count() >= p.min_prefix;
        if attempted {
            for (t, postings) in tx.postings_prefix(term, p.max_expansions)? {
                candidates.push((true, t, postings));
            }
        }
        // Per candidate term, the field-weighted, length-normalised frequency of each document.
        let mut per_term: Vec<(bool, BTreeMap<Id, f64>)> = Vec::new();
        let mut matching: BTreeSet<Id> = BTreeSet::new();
        for (is_prefix, _, postings) in &candidates {
            let mut weighted: BTreeMap<Id, f64> = BTreeMap::new();
            for post in postings {
                let Some(field) = Field::from_index(post.field as usize) else {
                    continue;
                };
                let dl = match lens.get(&post.symbol) {
                    Some(l) => l[field.index()],
                    None => {
                        let l = tx.doc_len(post.symbol)?.unwrap_or([0; LEXICON_FIELDS]);
                        lens.insert(post.symbol, l);
                        l[field.index()]
                    }
                };
                let norm = 1.0 - p.b + p.b * f64::from(dl) / avg[field.index()];
                *weighted.entry(post.symbol).or_default() +=
                    p.weights[field.index()] * f64::from(post.tf) / norm;
                matching.insert(post.symbol);
                acc.entry(post.symbol)
                    .or_default()
                    .matched
                    .push((qi, field.index(), *is_prefix));
            }
            per_term.push((*is_prefix, weighted));
        }
        // One idf per query token, from the documents any of its terms matches, so a rare
        // expansion does not outrank the token itself. Per document, the best contribution.
        let n = matching.len() as f64;
        let idf = (1.0 + (docs - n + 0.5) / (n + 0.5)).ln();
        let mut best: BTreeMap<Id, f64> = BTreeMap::new();
        let tracing = diag.on().is_some();
        let mut source: Best = BTreeMap::new();
        for (ci, (is_prefix, weighted)) in per_term.into_iter().enumerate() {
            let factor = if is_prefix { p.prefix_factor } else { 1.0 };
            for (id, t) in weighted {
                let c = factor * idf * t / (p.k1 + t);
                let e = best.entry(id).or_insert(0.0);
                if c > *e {
                    *e = c;
                    if tracing {
                        source.insert(id, (ci, t, c));
                    }
                }
            }
        }
        if let Some(d) = diag.on() {
            let expanded = candidates.len() - 1;
            let mut listed: Vec<ExpandedTerm> = candidates[1..]
                .iter()
                .map(|(_, t, p)| ExpandedTerm {
                    term: t.clone(),
                    postings: p.len() as u64,
                })
                .collect();
            d.cap(&mut listed, DIAG_TERMS, "prefix_terms");
            d.terms.push(TermTrace {
                token: term.clone(),
                exact_postings: candidates[0].2.len() as u64,
                matching_docs: matching.len() as u64,
                idf: round6(idf),
                prefix: PrefixTrace {
                    attempted,
                    expanded: expanded as u64,
                    cap_hit: expanded == MAX_EXPANSIONS,
                    factor: PREFIX_FACTOR,
                    terms: listed,
                },
            });
            sources.push(source);
            cand_terms.push(candidates.iter().map(|c| (c.0, c.1.clone())).collect());
        }
        for (id, c) in best {
            acc.entry(id).or_default().score += c;
        }
    }
    let mut hits: Vec<(f64, Id, SearchHit)> = Vec::new();
    for (id, a) in acc {
        let Some(row) = tx.symbol(id)? else { continue };
        let mut matched = a.matched;
        matched.sort();
        matched.dedup();
        // A prefix entry is redundant where the exact term matched the same field.
        let exact: Vec<(usize, usize)> = matched
            .iter()
            .filter(|m| !m.2)
            .map(|m| (m.0, m.1))
            .collect();
        matched.retain(|m| !m.2 || !exact.contains(&(m.0, m.1)));
        hits.push((
            a.score,
            id,
            SearchHit {
                symbol: hit(&row),
                score: round6(a.score),
                matched: matched
                    .into_iter()
                    .map(|(qi, fi, prefix)| Matched {
                        term: query[qi].clone(),
                        field: Field::ALL[fi].label().to_string(),
                        prefix,
                    })
                    .collect(),
            },
        ));
    }
    hits.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| a.2.symbol.name.cmp(&b.2.symbol.name))
    });
    if let Some(d) = diag.on() {
        for (i, (score, id, h)) in hits.iter().take(limit.min(DIAG_TOP)).enumerate() {
            let mut parts = Vec::new();
            for (qi, token) in query.iter().enumerate() {
                let Some(&(ci, t, c)) = sources[qi].get(id) else {
                    continue;
                };
                let (is_prefix, term) = &cand_terms[qi][ci];
                parts.push(PartTrace {
                    token: token.clone(),
                    via: if *is_prefix { "prefix" } else { "exact" }.into(),
                    term: term.clone(),
                    t: round6(t),
                    factor: if *is_prefix { PREFIX_FACTOR } else { 1.0 },
                    contribution: round6(c),
                });
            }
            d.ranking.push(RankTrace {
                position: i as u64 + 1,
                symbol: h.symbol.name.clone(),
                score: h.score,
                parts,
                tied_with_next: hits.get(i + 1).is_some_and(|n| n.0 == *score),
            });
        }
        d.add_omitted(
            "ranking",
            hits.len().min(limit).saturating_sub(DIAG_TOP) as u64,
        );
    }
    hits.truncate(limit);
    Ok(hits.into_iter().map(|(_, _, h)| h).collect())
}
