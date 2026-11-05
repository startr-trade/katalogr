//! `contract_map`: every contract with the ends bound to it, for the diagram views (FEAT-032).

use crate::resolve::hit;
use crate::result::SymbolHit;
use katalogr_model::{Id, Name, Span};
use katalogr_store::{Reader, StoreError};

/// One contract and its ends, each role sorted by canonical name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContractEnds {
    /// Canonical name of the contract.
    pub contract: String,
    pub kind: String,
    pub key: String,
    pub providers: Vec<SymbolHit>,
    pub consumers: Vec<SymbolHit>,
    pub declarers: Vec<SymbolHit>,
}

/// The contracts that were kept, and how many matched before the cap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContractMap {
    pub items: Vec<ContractEnds>,
    /// Contracts of the requested kind; more than `items.len()` means the map is partial.
    pub total: u64,
}

const WHOLE_FILE: Span = Span {
    start_line: 1,
    start_col: 1,
    end_line: 1,
    end_col: 1,
};

/// The contracts of `kind` (all kinds when `None`), sorted by canonical name, the first `max`
/// with their providers, consumers and declarers. The ends are found as `explain` finds them
/// for a contract: the sources of the contract's incoming `provides`, `consumes` and
/// `declares` links, whether symbols or whole artifacts. Ends are not capped.
pub fn contract_map(
    tx: &impl Reader,
    kind: Option<&str>,
    max: usize,
) -> Result<ContractMap, StoreError> {
    let mut rows: Vec<(String, Id, katalogr_model::ContractName)> = tx
        .contracts()?
        .into_iter()
        .filter(|(_, r)| kind.is_none_or(|k| r.name.kind == k))
        .map(|(id, r)| (r.name.canonical(), id, r.name))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let total = rows.len() as u64;
    rows.truncate(max);
    let mut items = Vec::with_capacity(rows.len());
    for (contract, id, name) in rows {
        let mut roles: [Vec<SymbolHit>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for lk in tx.links_in(id)? {
            let Some(role) = ["provides", "consumes", "declares"]
                .iter()
                .position(|r| *r == lk.kind.0)
            else {
                continue;
            };
            let end = if let Some(row) = tx.symbol(lk.from)? {
                hit(&row)
            } else if let Some(a) = tx.artifact(lk.from)? {
                SymbolHit {
                    name: a.name.canonical(),
                    kind: "artifact".to_string(),
                    path: a.name.path.as_str().to_string(),
                    span: WHOLE_FILE,
                }
            } else {
                continue;
            };
            roles[role].push(end);
        }
        for ends in &mut roles {
            ends.sort_by(|a, b| a.name.cmp(&b.name));
        }
        let [providers, consumers, declarers] = roles;
        items.push(ContractEnds {
            contract,
            kind: name.kind,
            key: name.key,
            providers,
            consumers,
            declarers,
        });
    }
    Ok(ContractMap { items, total })
}

#[cfg(test)]
mod fixture {
    use camino::Utf8PathBuf;
    use katalogr_model::{
        ArtifactName, Confidence, ContractName, Fragment, Id, LinkKind, Name, Provenance, RelPath,
        Segment, Span, SymbolName,
    };
    use katalogr_store::{
        Cache, ContractRow, Kindex, LinkEntry, LinkKey, LinkOrigin, LinkRow, ProjectConfig,
        ProjectName, RepoConfig, Restitch, StoreError, SymbolRow, WriteTx,
    };
    use std::time::{Duration, Instant};

    struct NoShards;

    impl Restitch for NoShards {
        fn replace_fragment(&self, _: &mut WriteTx, _: Fragment) -> Result<(), StoreError> {
            Ok(())
        }
        fn remove_artifact(&self, _: &mut WriteTx, _: &ArtifactName) -> Result<(), StoreError> {
            Ok(())
        }
    }

    const SPAN: Span = Span {
        start_line: 2,
        start_col: 1,
        end_line: 2,
        end_col: 5,
    };

    /// `(kind, key, [(role, symbol name)])` contracts written in the given order.
    pub(super) fn build(
        contracts: &[(&str, &str, &[(&str, &str)])],
    ) -> (tempfile::TempDir, Cache) {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let dir = Kindex::init(&root)
            .unwrap()
            .create_project(
                &ProjectName::new("cm").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let cache =
            Cache::open_or_rebuild(&dir, &NoShards, Instant::now() + Duration::from_secs(60))
                .unwrap();
        let path = RelPath::new("f.tx").unwrap();
        let artifact = ArtifactName {
            repo: "r".into(),
            path: path.clone(),
        };
        let provenance = Provenance {
            repo: "r".into(),
            path,
            revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
        };
        let mut tx = cache.write().unwrap();
        for (kind, key, ends) in contracts {
            let cname = ContractName {
                kind: (*kind).into(),
                key: (*key).into(),
            };
            let cid = Id::of(&cname.canonical());
            let count = |r: &str| ends.iter().filter(|e| e.0 == r).count() as u32;
            tx.put_contract(
                cid,
                &ContractRow {
                    name: cname,
                    providers: count("provides"),
                    consumers: count("consumes"),
                    declarers: count("declares"),
                },
            )
            .unwrap();
            for (role, sym) in *ends {
                let n = SymbolName {
                    artifact: artifact.clone(),
                    segments: vec![Segment {
                        kind: "fn".into(),
                        name: (*sym).into(),
                    }],
                    ordinal: 1,
                };
                let id = n.id();
                tx.put_symbol(
                    id,
                    &SymbolRow {
                        name: n,
                        artifact: artifact.id(),
                        kind: "fn".into(),
                        span: SPAN,
                        facts: Default::default(),
                    },
                )
                .unwrap();
                tx.put_link(
                    &LinkKey {
                        from: id,
                        kind: LinkKind((*role).into()),
                        to: cid,
                    },
                    &LinkRow {
                        entries: vec![LinkEntry {
                            origin: LinkOrigin::Intra,
                            confidence: Confidence::Extracted,
                            span: Some(SPAN),
                            provenance: provenance.clone(),
                            fanout: None,
                            produced_by: None,
                        }],
                    },
                )
                .unwrap();
            }
        }
        tx.commit().unwrap();
        (tmp, cache)
    }
}

/// R-2 (FEAT-032): contracts sorted by name, ends sorted by role, the cap applied after the
/// sort with the total reported, and the kind filter counted in the total.
#[cfg(test)]
#[test]
fn sorted_and_capped() {
    let (_tmp, cache) = fixture::build(&[
        ("topic", "zeta", &[("provides", "p_z")]),
        (
            "channel",
            "orders",
            &[("consumes", "b_c"), ("consumes", "a_c"), ("provides", "p1")],
        ),
        ("channel", "alpha", &[("declares", "d1")]),
        ("topic", "beta", &[]),
    ]);
    let tx = cache.read().unwrap();
    let all = contract_map(&tx, None, 10).unwrap();
    assert_eq!(all.total, 4);
    let names: Vec<&str> = all.items.iter().map(|c| c.contract.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    let orders = all.items.iter().find(|c| c.key == "orders").unwrap();
    assert_eq!(orders.kind, "channel");
    let consumers: Vec<&str> = orders.consumers.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(consumers.len(), 2);
    assert!(consumers[0] < consumers[1]);
    assert_eq!(orders.providers.len(), 1);
    assert!(orders.declarers.is_empty());
    let alpha = all.items.iter().find(|c| c.key == "alpha").unwrap();
    assert_eq!(alpha.declarers.len(), 1);

    let capped = contract_map(&tx, None, 2).unwrap();
    assert_eq!(capped.total, 4);
    assert_eq!(capped.items.len(), 2);
    assert_eq!(capped.items[..], all.items[..2]);

    let channels = contract_map(&tx, Some("channel"), 10).unwrap();
    assert_eq!(channels.total, 2);
    assert!(channels.items.iter().all(|c| c.kind == "channel"));
    assert_eq!(contract_map(&tx, Some("none"), 10).unwrap().total, 0);
}
