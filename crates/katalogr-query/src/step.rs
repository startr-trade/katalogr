//! The backward step shared by `callers` and `tests` (FEAT-004, FEAT-063): who reaches a node
//! through a `calls` link or by consuming a contract the node provides.

use katalogr_model::{Id, LinkKind, Name};
use katalogr_store::{LinkKey, Reader, StoreError};

const CALLS: &str = "calls";
const PROVIDES: &str = "provides";
const CONSUMES: &str = "consumes";

/// One backward step to `from` from the node the step was taken from.
pub(crate) struct Step {
    pub from: Id,
    /// The `calls` link, or the `consumes` link of a contract consumer.
    pub key: LinkKey,
    /// When through a contract: its canonical name and the `provides` link from the node
    /// (None when the node is the contract).
    pub via: Option<(String, Option<LinkKey>)>,
}

/// Incoming `calls` links of `id`, then the consumers of each contract `id` provides (or of
/// `id` itself when it is a contract).
pub(crate) fn incoming(tx: &impl Reader, id: Id) -> Result<Vec<Step>, StoreError> {
    Ok(incoming_many(tx, &[id], true)?.pop().unwrap_or_default())
}

/// [`incoming`] for many nodes, in order; the reads share their table lookups. With
/// `with_contracts` false only `calls` links are followed (for a graph that has no contracts).
pub(crate) fn incoming_many(
    tx: &impl Reader,
    ids: &[Id],
    with_contracts: bool,
) -> Result<Vec<Vec<Step>>, StoreError> {
    let provides_kind = LinkKind(PROVIDES.to_string());
    let ins = tx.links_in_many(ids)?;
    let provs = if with_contracts {
        tx.link_keys_out_many(ids, &provides_kind)?
    } else {
        vec![Vec::new(); ids.len()]
    };
    let mut all = Vec::with_capacity(ids.len());
    for ((&id, ins), provs) in ids.iter().zip(ins).zip(provs) {
        let mut out = Vec::new();
        for key in &ins {
            if key.kind.0 == CALLS {
                out.push(Step {
                    from: key.from,
                    key: key.clone(),
                    via: None,
                });
            }
        }
        // `id` itself when it is a contract (it then has consumers), then the contracts it provides.
        let mut contracts: Vec<(Id, Option<LinkKey>, Option<Vec<LinkKey>>)> = Vec::new();
        if with_contracts && ins.iter().any(|k| k.kind.0 == CONSUMES) {
            contracts.push((id, None, Some(ins.clone())));
        }
        for key in provs {
            contracts.push((key.to, Some(key), None));
        }
        for (cid, provides, known) in contracts {
            let Some(contract) = tx.contract(cid)? else {
                continue;
            };
            let consumers = match known {
                Some(k) => k,
                None => tx.links_in(cid)?,
            };
            for key in consumers {
                if key.kind.0 == CONSUMES {
                    out.push(Step {
                        from: key.from,
                        key,
                        via: Some((contract.name.canonical(), provides.clone())),
                    });
                }
            }
        }
        all.push(out);
    }
    Ok(all)
}
