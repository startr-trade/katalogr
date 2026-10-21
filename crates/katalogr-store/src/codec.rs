//! Cache row codec (FEAT-075 R-1, R-4): postcard bytes of store-private wire structs.
//!
//! The logical row types keep their serde derives (and their `skip_serializing_if` attributes,
//! which keep shard JSON stable). Postcard is not self-describing, so a skipped field would shift
//! every later one; the wire structs below mirror the rows without any skipping. Strings that
//! repeat (link kinds, namespaces, extension ids) and provenance go through an [`Interner`] and
//! come back through a [`Resolver`].

use crate::cache::{ArtifactRow, LinkEntry, LinkOrigin, LinkRow, SymbolRow};
use crate::error::StoreError;
use katalogr_model::{
    ArtifactName, Confidence, ContractName, ContractRole, Export, ExportKey, Fact, Id, LinkKind,
    OutRef, Provenance, RefBinding, RelPath, Span, SymbolName,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A vocabulary id: a link kind, symbol kind, export namespace or extension id.
pub(crate) type Vid = u16;
/// A provenance dictionary id.
pub(crate) type Pid = u32;

/// Encode one value as postcard bytes.
pub(crate) fn enc<T: Serialize>(v: &T) -> Result<Vec<u8>, StoreError> {
    postcard::to_allocvec(v).map_err(|e| StoreError::Row(e.to_string()))
}

/// Decode one value; bytes left over after it are an error.
pub(crate) fn dec<'a, T: Deserialize<'a>>(b: &'a [u8]) -> Result<T, StoreError> {
    let (v, rest) = postcard::take_from_bytes(b).map_err(|e| StoreError::Row(e.to_string()))?;
    if rest.is_empty() {
        Ok(v)
    } else {
        Err(StoreError::Row("trailing bytes".into()))
    }
}

/// Turns strings and provenance into dictionary ids while a row is written.
pub(crate) trait Interner {
    fn vid(&mut self, s: &str) -> Result<Vid, StoreError>;
    fn pid(&mut self, p: &Provenance) -> Result<Pid, StoreError>;
}

/// Turns dictionary ids back into strings and provenance while a row is read.
pub(crate) trait Resolver {
    fn vstr(&self, id: Vid) -> Result<String, StoreError>;
    fn prov(&self, id: Pid) -> Result<Provenance, StoreError>;
}

// ---------------------------------------------------------------- wire structs

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum WireOrigin {
    Intra,
    Ref(u32),
    Bound { artifact: Id, index: u32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WireLinkEntry {
    origin: WireOrigin,
    confidence: Confidence,
    span: Option<Span>,
    prov: Pid,
    fanout: Option<u32>,
    produced_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WireLinkRow {
    entries: Vec<WireLinkEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WireSymbolRow {
    name: SymbolName,
    artifact: Id,
    kind: Vid,
    span: Span,
    facts: BTreeMap<String, Fact>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WireOutRef {
    from: Id,
    kind: Vid,
    span: Option<Span>,
    candidates: Vec<(Vid, String)>,
    confidence: Confidence,
    origin: Option<String>,
    binds: Option<(ContractName, ContractRole)>,
}

/// An export target: the artifact itself, its n-th symbol (index into `symbols`), or another id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum WireTarget {
    Artifact,
    Symbol(u32),
    Other(Id),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WireExport {
    ns: Vid,
    key: String,
    target: WireTarget,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WireArtifactRow {
    name: ArtifactName,
    extension: Vid,
    prov: Pid,
    facts: BTreeMap<String, Fact>,
    symbols: Vec<Id>,
    exports: Vec<WireExport>,
    refs: Vec<WireOutRef>,
}

/// A provenance dictionary entry with the number of rows that reference it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WireProv {
    pub(crate) repo: String,
    pub(crate) path: RelPath,
    pub(crate) revision: String,
    pub(crate) refs: u32,
}

// ---------------------------------------------------------------- logical <-> wire

fn out_ref_to_wire(r: &OutRef, int: &mut dyn Interner) -> Result<WireOutRef, StoreError> {
    let mut candidates = Vec::with_capacity(r.candidates.len());
    for c in &r.candidates {
        candidates.push((int.vid(&c.namespace)?, c.key.clone()));
    }
    Ok(WireOutRef {
        from: r.from,
        kind: int.vid(&r.kind.0)?,
        span: r.span,
        candidates,
        confidence: r.confidence,
        origin: r.origin.clone(),
        binds: r.binds.as_ref().map(|b| (b.contract.clone(), b.role)),
    })
}

fn out_ref_from_wire(w: WireOutRef, res: &dyn Resolver) -> Result<OutRef, StoreError> {
    let mut candidates = Vec::with_capacity(w.candidates.len());
    for (ns, key) in w.candidates {
        candidates.push(ExportKey {
            namespace: res.vstr(ns)?,
            key,
        });
    }
    Ok(OutRef {
        from: w.from,
        kind: LinkKind(res.vstr(w.kind)?),
        span: w.span,
        candidates,
        confidence: w.confidence,
        origin: w.origin,
        binds: w
            .binds
            .map(|(contract, role)| RefBinding { contract, role }),
    })
}

/// `id` is the artifact's own id: it decides which exports are stored as `Artifact`.
pub(crate) fn artifact_to_wire(
    id: Id,
    row: &ArtifactRow,
    int: &mut dyn Interner,
) -> Result<WireArtifactRow, StoreError> {
    let extension = int.vid(&row.extension)?;
    let prov = int.pid(&row.provenance)?;
    let mut exports = Vec::with_capacity(row.exports.len());
    for e in &row.exports {
        let target = if e.target == id {
            WireTarget::Artifact
        } else {
            match row.symbols.binary_search(&e.target) {
                Ok(i) => WireTarget::Symbol(i as u32),
                Err(_) => WireTarget::Other(e.target),
            }
        };
        exports.push(WireExport {
            ns: int.vid(&e.key.namespace)?,
            key: e.key.key.clone(),
            target,
        });
    }
    let mut refs = Vec::with_capacity(row.refs.len());
    for r in &row.refs {
        refs.push(out_ref_to_wire(r, int)?);
    }
    Ok(WireArtifactRow {
        name: row.name.clone(),
        extension,
        prov,
        facts: row.facts.clone(),
        symbols: row.symbols.clone(),
        exports,
        refs,
    })
}

pub(crate) fn artifact_from_wire(
    id: Id,
    w: WireArtifactRow,
    res: &dyn Resolver,
) -> Result<ArtifactRow, StoreError> {
    let mut exports = Vec::with_capacity(w.exports.len());
    for e in w.exports {
        let target = match e.target {
            WireTarget::Artifact => id,
            WireTarget::Symbol(i) => *w
                .symbols
                .get(i as usize)
                .ok_or_else(|| StoreError::Row("export target out of range".into()))?,
            WireTarget::Other(t) => t,
        };
        exports.push(Export {
            key: ExportKey {
                namespace: res.vstr(e.ns)?,
                key: e.key,
            },
            target,
        });
    }
    let mut refs = Vec::with_capacity(w.refs.len());
    for r in w.refs {
        refs.push(out_ref_from_wire(r, res)?);
    }
    Ok(ArtifactRow {
        name: w.name,
        extension: res.vstr(w.extension)?,
        provenance: res.prov(w.prov)?,
        facts: w.facts,
        symbols: w.symbols,
        exports,
        refs,
    })
}

pub(crate) fn symbol_to_wire(
    row: &SymbolRow,
    int: &mut dyn Interner,
) -> Result<WireSymbolRow, StoreError> {
    Ok(WireSymbolRow {
        name: row.name.clone(),
        artifact: row.artifact,
        kind: int.vid(&row.kind)?,
        span: row.span,
        facts: row.facts.clone(),
    })
}

pub(crate) fn symbol_from_wire(
    w: WireSymbolRow,
    res: &dyn Resolver,
) -> Result<SymbolRow, StoreError> {
    Ok(SymbolRow {
        name: w.name,
        artifact: w.artifact,
        kind: res.vstr(w.kind)?,
        span: w.span,
        facts: w.facts,
    })
}

pub(crate) fn link_to_wire(
    row: &LinkRow,
    int: &mut dyn Interner,
) -> Result<WireLinkRow, StoreError> {
    let mut entries = Vec::with_capacity(row.entries.len());
    for e in &row.entries {
        entries.push(WireLinkEntry {
            origin: match e.origin {
                LinkOrigin::Intra => WireOrigin::Intra,
                LinkOrigin::Ref(i) => WireOrigin::Ref(i),
                LinkOrigin::Bound { artifact, index } => WireOrigin::Bound { artifact, index },
            },
            confidence: e.confidence,
            span: e.span,
            prov: int.pid(&e.provenance)?,
            fanout: e.fanout,
            produced_by: e.produced_by.clone(),
        });
    }
    Ok(WireLinkRow { entries })
}

pub(crate) fn link_from_wire(w: WireLinkRow, res: &dyn Resolver) -> Result<LinkRow, StoreError> {
    let mut entries = Vec::with_capacity(w.entries.len());
    for e in w.entries {
        entries.push(LinkEntry {
            origin: match e.origin {
                WireOrigin::Intra => LinkOrigin::Intra,
                WireOrigin::Ref(i) => LinkOrigin::Ref(i),
                WireOrigin::Bound { artifact, index } => LinkOrigin::Bound { artifact, index },
            },
            confidence: e.confidence,
            span: e.span,
            provenance: res.prov(e.prov)?,
            fanout: e.fanout,
            produced_by: e.produced_by,
        });
    }
    Ok(LinkRow { entries })
}

#[cfg(test)]
mod tests {
    use super::*;
    use katalogr_model::{AnyName, Name, parse_name};
    use proptest::prelude::*;

    /// An in-memory dictionary: ids are positions in first-use order.
    #[derive(Default)]
    struct Mem {
        vocab: Vec<String>,
        provs: Vec<Provenance>,
    }

    impl Interner for Mem {
        fn vid(&mut self, s: &str) -> Result<Vid, StoreError> {
            let i = match self.vocab.iter().position(|v| v == s) {
                Some(i) => i,
                None => {
                    self.vocab.push(s.to_string());
                    self.vocab.len() - 1
                }
            };
            Ok(i as Vid)
        }
        fn pid(&mut self, p: &Provenance) -> Result<Pid, StoreError> {
            let i = match self.provs.iter().position(|v| v == p) {
                Some(i) => i,
                None => {
                    self.provs.push(p.clone());
                    self.provs.len() - 1
                }
            };
            Ok(i as Pid)
        }
    }

    impl Resolver for Mem {
        fn vstr(&self, id: Vid) -> Result<String, StoreError> {
            self.vocab
                .get(id as usize)
                .cloned()
                .ok_or_else(|| StoreError::Row("unknown vocabulary id".into()))
        }
        fn prov(&self, id: Pid) -> Result<Provenance, StoreError> {
            self.provs
                .get(id as usize)
                .cloned()
                .ok_or_else(|| StoreError::Row("unknown provenance id".into()))
        }
    }

    fn id() -> impl Strategy<Value = Id> {
        any::<[u8; 16]>().prop_map(Id)
    }

    fn ident() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9_]{0,6}"
    }

    fn span() -> impl Strategy<Value = Span> {
        (1..500u32, 1..200u32, 1..500u32, 1..200u32).prop_map(|(a, b, c, d)| Span {
            start_line: a,
            start_col: b,
            end_line: c,
            end_col: d,
        })
    }

    fn confidence() -> impl Strategy<Value = Confidence> {
        prop_oneof![
            Just(Confidence::Extracted),
            Just(Confidence::Inferred),
            Just(Confidence::Declared)
        ]
    }

    fn fact() -> impl Strategy<Value = Fact> {
        let leaf = prop_oneof![
            ".{0,8}".prop_map(Fact::Str),
            any::<i64>().prop_map(Fact::Int),
            any::<bool>().prop_map(Fact::Bool),
        ];
        leaf.prop_recursive(2, 8, 3, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..3).prop_map(Fact::List),
                prop::collection::btree_map(ident(), inner, 0..3).prop_map(Fact::Map),
            ]
        })
    }

    fn facts() -> impl Strategy<Value = BTreeMap<String, Fact>> {
        prop::collection::btree_map(ident(), fact(), 0..3)
    }

    fn artifact_name() -> impl Strategy<Value = ArtifactName> {
        ("[a-z][a-z0-9_-]{0,5}", "[a-z]{1,4}(/[a-z]{1,4}){0,2}\\.rs")
            .prop_map(|(r, p)| ArtifactName::new(&r, &p).unwrap())
    }

    fn symbol_name() -> impl Strategy<Value = SymbolName> {
        (artifact_name(), ident()).prop_map(|(a, n)| {
            match parse_name(&format!("{}#fn:{n}", a.canonical())).unwrap() {
                AnyName::Symbol(s) => s,
                other => panic!("not a symbol: {other:?}"),
            }
        })
    }

    fn contract_name() -> impl Strategy<Value = ContractName> {
        (ident(), ident()).prop_map(|(kind, key)| ContractName { kind, key })
    }

    fn provenance() -> impl Strategy<Value = Provenance> {
        ("[a-z]{1,4}", "[a-z]{1,4}\\.rs", "[0-9a-f]{40}").prop_map(|(repo, path, revision)| {
            Provenance {
                repo,
                path: RelPath::new(&path).unwrap(),
                revision,
            }
        })
    }

    fn export_key() -> impl Strategy<Value = ExportKey> {
        (ident(), ".{0,8}").prop_map(|(namespace, key)| ExportKey { namespace, key })
    }

    fn out_ref() -> impl Strategy<Value = OutRef> {
        (
            id(),
            ident(),
            prop::option::of(span()),
            prop::collection::vec(export_key(), 0..3),
            confidence(),
            prop::option::of(ident()),
            prop::option::of((
                contract_name(),
                prop_oneof![
                    Just(ContractRole::Provides),
                    Just(ContractRole::Consumes),
                    Just(ContractRole::Declares)
                ],
            )),
        )
            .prop_map(
                |(from, kind, span, candidates, confidence, origin, binds)| OutRef {
                    from,
                    kind: LinkKind(kind),
                    span,
                    candidates,
                    confidence,
                    origin,
                    binds: binds.map(|(contract, role)| RefBinding { contract, role }),
                },
            )
    }

    fn artifact_row() -> impl Strategy<Value = (Id, ArtifactRow)> {
        (
            id(),
            artifact_name(),
            ident(),
            provenance(),
            facts(),
            prop::collection::btree_set(id(), 0..4),
            prop::collection::vec(out_ref(), 0..3),
            prop::collection::vec((export_key(), 0..6usize, id()), 0..4),
        )
            .prop_map(
                |(aid, name, extension, provenance, facts, syms, refs, ex)| {
                    let symbols: Vec<Id> = syms.into_iter().collect();
                    let exports = ex
                        .into_iter()
                        .map(|(key, pick, other)| Export {
                            key,
                            // Itself, a symbol of the row, or a foreign id.
                            target: match pick {
                                0 => aid,
                                1..=3 if !symbols.is_empty() => symbols[pick % symbols.len()],
                                _ => other,
                            },
                        })
                        .collect();
                    (
                        aid,
                        ArtifactRow {
                            name,
                            extension,
                            provenance,
                            facts,
                            symbols,
                            exports,
                            refs,
                        },
                    )
                },
            )
    }

    fn symbol_row() -> impl Strategy<Value = SymbolRow> {
        (symbol_name(), id(), ident(), span(), facts()).prop_map(
            |(name, artifact, kind, span, facts)| SymbolRow {
                name,
                artifact,
                kind,
                span,
                facts,
            },
        )
    }

    fn link_entry() -> impl Strategy<Value = LinkEntry> {
        (
            prop_oneof![
                Just(LinkOrigin::Intra),
                any::<u32>().prop_map(LinkOrigin::Ref),
                (id(), any::<u32>())
                    .prop_map(|(artifact, index)| LinkOrigin::Bound { artifact, index }),
            ],
            confidence(),
            prop::option::of(span()),
            provenance(),
            prop::option::of(any::<u32>()),
            prop::option::of(ident()),
        )
            .prop_map(
                |(origin, confidence, span, provenance, fanout, produced_by)| LinkEntry {
                    origin,
                    confidence,
                    span,
                    provenance,
                    fanout,
                    produced_by,
                },
            )
    }

    fn link_row() -> impl Strategy<Value = LinkRow> {
        prop::collection::vec(link_entry(), 1..4).prop_map(|entries| LinkRow { entries })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn wire_roundtrip_every_row(
            (aid, art) in artifact_row(),
            sym in symbol_row(),
            link in link_row(),
        ) {
            let mut mem = Mem::default();
            let wa = enc(&artifact_to_wire(aid, &art, &mut mem).unwrap()).unwrap();
            let ws = enc(&symbol_to_wire(&sym, &mut mem).unwrap()).unwrap();
            let wl = enc(&link_to_wire(&link, &mut mem).unwrap()).unwrap();
            prop_assert_eq!(artifact_from_wire(aid, dec(&wa).unwrap(), &mem).unwrap(), art);
            prop_assert_eq!(symbol_from_wire(dec(&ws).unwrap(), &mem).unwrap(), sym);
            prop_assert_eq!(link_from_wire(dec(&wl).unwrap(), &mem).unwrap(), link);
        }
    }

    #[test]
    fn id_binary_is_16_bytes() {
        let id = Id::of("core:src/graph.rs");
        let bytes = enc(&id).unwrap();
        assert_eq!(bytes, id.0);
        assert_eq!(dec::<Id>(&bytes).unwrap(), id);
        assert_eq!(enc(&Some(id)).unwrap().len(), 17);
    }

    #[test]
    fn dec_rejects_trailing_bytes() {
        let mut bytes = enc(&7u32).unwrap();
        bytes.push(0);
        assert!(matches!(dec::<u32>(&bytes), Err(StoreError::Row(m)) if m == "trailing bytes"));
    }

    fn prov(p: &str) -> Provenance {
        Provenance {
            repo: "r".into(),
            path: RelPath::new(p).unwrap(),
            revision: "abc".into(),
        }
    }

    /// Every optional field absent and present: a stray `skip_serializing_if` on a wire struct
    /// (or a model type encoded directly) would shift the later fields and fail here.
    #[test]
    fn wire_options_roundtrip() {
        let sp = Span {
            start_line: 1,
            start_col: 2,
            end_line: 3,
            end_col: 4,
        };
        for mask in 0..8u8 {
            let some = |bit: u8| mask & bit != 0;
            let entry = LinkEntry {
                origin: LinkOrigin::Ref(3),
                confidence: Confidence::Inferred,
                span: some(1).then_some(sp),
                provenance: prov("a.rs"),
                fanout: some(2).then_some(9),
                produced_by: some(4).then(|| "profile:p#r".to_string()),
            };
            let row = LinkRow {
                entries: vec![entry.clone(), entry],
            };
            let mut mem = Mem::default();
            let bytes = enc(&link_to_wire(&row, &mut mem).unwrap()).unwrap();
            assert_eq!(link_from_wire(dec(&bytes).unwrap(), &mem).unwrap(), row);

            let r = OutRef {
                from: Id::of("x"),
                kind: LinkKind("calls".into()),
                span: some(1).then_some(sp),
                candidates: vec![ExportKey {
                    namespace: "ns".into(),
                    key: "k".into(),
                }],
                confidence: Confidence::Declared,
                origin: some(2).then(|| "profile:p#r".to_string()),
                binds: some(4).then(|| RefBinding {
                    contract: ContractName {
                        kind: "channel".into(),
                        key: "orders".into(),
                    },
                    role: ContractRole::Consumes,
                }),
            };
            let aid = Id::of("a");
            let art = ArtifactRow {
                name: ArtifactName::new("r", "a.rs").unwrap(),
                extension: "ext-a".into(),
                provenance: prov("a.rs"),
                facts: BTreeMap::new(),
                symbols: vec![],
                exports: vec![],
                refs: vec![r.clone(), r],
            };
            let bytes = enc(&artifact_to_wire(aid, &art, &mut mem).unwrap()).unwrap();
            assert_eq!(
                artifact_from_wire(aid, dec(&bytes).unwrap(), &mem).unwrap(),
                art
            );
        }
    }

    #[test]
    fn export_target_ordinals_roundtrip() {
        let aid = Id::of("r:a.rs");
        let mut symbols = vec![Id::of("r:a.rs#fn:one"), Id::of("r:a.rs#fn:two")];
        symbols.sort();
        let foreign = Id::of("r:b.rs#fn:far");
        let key = |k: &str| ExportKey {
            namespace: "ns".into(),
            key: k.into(),
        };
        let art = ArtifactRow {
            name: ArtifactName::new("r", "a.rs").unwrap(),
            extension: "ext-a".into(),
            provenance: prov("a.rs"),
            facts: BTreeMap::new(),
            symbols: symbols.clone(),
            exports: vec![
                Export {
                    key: key("self"),
                    target: aid,
                },
                Export {
                    key: key("first"),
                    target: symbols[0],
                },
                Export {
                    key: key("second"),
                    target: symbols[1],
                },
                Export {
                    key: key("foreign"),
                    target: foreign,
                },
            ],
            refs: vec![],
        };
        let mut mem = Mem::default();
        let wire = artifact_to_wire(aid, &art, &mut mem).unwrap();
        let targets: Vec<&WireTarget> = wire.exports.iter().map(|e| &e.target).collect();
        assert_eq!(
            targets,
            [
                &WireTarget::Artifact,
                &WireTarget::Symbol(0),
                &WireTarget::Symbol(1),
                &WireTarget::Other(foreign)
            ]
        );
        let back = artifact_from_wire(aid, dec(&enc(&wire).unwrap()).unwrap(), &mem).unwrap();
        assert_eq!(back, art);

        let mut bad = wire;
        bad.exports[1].target = WireTarget::Symbol(2);
        assert!(artifact_from_wire(aid, bad, &mem).is_err());
    }
}
