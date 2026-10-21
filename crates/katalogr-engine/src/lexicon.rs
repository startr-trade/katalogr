//! Postings maintenance (FEAT-025 section 4): the searchable text of each symbol is tokenised
//! and written inside the same transaction as the fragment, so search is never ahead of or
//! behind the graph.
//!
//! Fields: the symbol's own name, its `doc` fact, its `text` fact (a section's first
//! paragraph) and the keys of the exports that point at it (contract ids).

pub use katalogr_model::{Field, tokenize};

use katalogr_model::{Fact, Id};
use katalogr_store::{ArtifactRow, LEXICON_FIELDS, Reader, StoreError, SymbolRow, WriteTx};
use std::collections::BTreeMap;

/// Fact holding the first paragraph of a doc comment.
pub const DOC_FACT: &str = "doc";
/// Fact holding the first paragraph of a section.
pub const TEXT_FACT: &str = "text";

/// What one symbol contributes to the index.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Doc {
    pub lens: [u32; LEXICON_FIELDS],
    /// `(term, field index)` to term frequency.
    pub terms: BTreeMap<(String, u8), u32>,
}

fn fact_str<'a>(row: &'a SymbolRow, key: &str) -> Option<&'a str> {
    match row.facts.get(key) {
        Some(Fact::Str(s)) => Some(s.as_str()),
        _ => None,
    }
}

/// The tokens of each field of `row`; `contracts` are the keys of the exports targeting it.
pub fn doc_of(row: &SymbolRow, contracts: &[&str]) -> Doc {
    let mut doc = Doc::default();
    let mut add = |field: Field, text: &str| {
        for t in tokenize(text) {
            doc.lens[field.index()] += 1;
            *doc.terms.entry((t, field.index() as u8)).or_default() += 1;
        }
    };
    if let Some(last) = row.name.segments.last() {
        add(Field::Name, &last.name);
    }
    if let Some(d) = fact_str(row, DOC_FACT) {
        add(Field::Doc, d);
    }
    if let Some(t) = fact_str(row, TEXT_FACT) {
        add(Field::Section, t);
    }
    for c in contracts {
        add(Field::Contract, c);
    }
    doc
}

fn contracts_by_symbol(artifact: &ArtifactRow) -> BTreeMap<Id, Vec<&str>> {
    let mut out: BTreeMap<Id, Vec<&str>> = BTreeMap::new();
    for e in &artifact.exports {
        out.entry(e.target).or_default().push(e.key.key.as_str());
    }
    out
}

/// Pure: the lexicon document of each symbol of `artifact`, in `artifact.symbols` order, from
/// the given rows. A symbol without a row is skipped.
pub fn docs_of_artifact(artifact: &ArtifactRow, rows: &[(Id, SymbolRow)]) -> Vec<(Id, Doc)> {
    let contracts = contracts_by_symbol(artifact);
    let by_id: BTreeMap<Id, &SymbolRow> = rows.iter().map(|(id, r)| (*id, r)).collect();
    let mut out = Vec::with_capacity(artifact.symbols.len());
    for id in &artifact.symbols {
        let Some(row) = by_id.get(id) else { continue };
        let keys = contracts.get(id).map(Vec::as_slice).unwrap_or(&[]);
        out.push((*id, doc_of(row, keys)));
    }
    out
}

/// Write the postings of precomputed documents.
pub fn index_docs(tx: &mut WriteTx, docs: &[(Id, Doc)]) -> Result<(), StoreError> {
    for (id, doc) in docs {
        let terms: Vec<(String, u8, u32)> = doc
            .terms
            .iter()
            .map(|((t, f), n)| (t.clone(), *f, *n))
            .collect();
        tx.put_doc_postings(*id, doc.lens, &terms)?;
    }
    Ok(())
}

/// Index every symbol of `artifact`. The symbol rows must already be in the transaction.
pub fn index_artifact(tx: &mut WriteTx, artifact: &ArtifactRow) -> Result<(), StoreError> {
    let mut rows = Vec::with_capacity(artifact.symbols.len());
    for id in &artifact.symbols {
        if let Some(row) = tx.symbol(*id)? {
            rows.push((*id, row));
        }
    }
    index_docs(tx, &docs_of_artifact(artifact, &rows))
}

/// Remove the postings of every symbol of `artifact`. The symbol rows must still be present.
pub fn unindex_artifact(tx: &mut WriteTx, artifact: &ArtifactRow) -> Result<(), StoreError> {
    let contracts = contracts_by_symbol(artifact);
    for id in &artifact.symbols {
        let Some(row) = tx.symbol(*id)? else { continue };
        let keys = contracts.get(id).map(Vec::as_slice).unwrap_or(&[]);
        let terms: Vec<(String, u8)> = doc_of(&row, keys).terms.into_keys().collect();
        tx.remove_doc_postings(*id, &terms)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use katalogr_model::{ArtifactName, Name, RelPath, Segment, Span, SymbolName};

    fn row(name: &str, facts: &[(&str, &str)]) -> SymbolRow {
        let artifact = ArtifactName {
            repo: "r".into(),
            path: RelPath::new("a.tx").unwrap(),
        };
        SymbolRow {
            name: SymbolName {
                artifact: artifact.clone(),
                segments: vec![Segment {
                    kind: "fn".into(),
                    name: name.into(),
                }],
                ordinal: 1,
            },
            artifact: artifact.id(),
            kind: "fn".into(),
            span: Span {
                start_line: 1,
                start_col: 1,
                end_line: 1,
                end_col: 1,
            },
            facts: facts
                .iter()
                .map(|(k, v)| (k.to_string(), Fact::Str(v.to_string())))
                .collect(),
        }
    }

    #[test]
    fn fields_are_tokenised_separately() {
        let d = doc_of(
            &row(
                "applyRateCode",
                &[("doc", "Apply the rate"), ("text", "Rates")],
            ),
            &["pkg.applyRateCode"],
        );
        assert_eq!(d.lens[Field::Name.index()], 4);
        assert_eq!(d.lens[Field::Doc.index()], 3);
        assert_eq!(d.lens[Field::Section.index()], 1);
        assert_eq!(d.lens[Field::Contract.index()], 5);
        assert_eq!(d.terms[&("rate".to_string(), Field::Name.index() as u8)], 1);
        assert_eq!(d.terms[&("apply".to_string(), Field::Doc.index() as u8)], 1);
    }
}
