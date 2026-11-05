//! Fan rows (FEAT-077, ADR-0028): key encoders for the three fan tables and the expansion of
//! fan rows into the link rows every reader sees.

use crate::cache::{FanKey, LinkEntry, LinkKey, LinkRow, lp, split_lp};
use crate::error::StoreError;
use katalogr_model::{ExportKey, Id, LinkKind};
use std::collections::BTreeMap;

fn bad() -> StoreError {
    StoreError::Row("malformed key".into())
}

fn id_at(b: &[u8]) -> Result<Id, StoreError> {
    Ok(Id(b
        .get(..16)
        .ok_or_else(bad)?
        .try_into()
        .map_err(|_| bad())?))
}

fn utf8(b: &[u8]) -> Result<String, StoreError> {
    Ok(std::str::from_utf8(b).map_err(|_| bad())?.to_string())
}

/// `fan_out` scan prefix: `from`, then `lp(kind)` when a kind is given.
pub(crate) fn fan_out_prefix(from: Id, kind: Option<&LinkKind>) -> Vec<u8> {
    let mut k = from.0.to_vec();
    if let Some(kind) = kind {
        lp(&mut k, &kind.0);
    }
    k
}

/// `fan_out` key: `from || lp(kind) || lp(namespace) || key bytes`.
pub(crate) fn fan_out_key(k: &FanKey) -> Vec<u8> {
    let mut out = fan_out_prefix(k.from, Some(&k.kind));
    lp(&mut out, &k.key.namespace);
    out.extend_from_slice(k.key.key.as_bytes());
    out
}

pub(crate) fn decode_fan_out_key(b: &[u8]) -> Result<FanKey, StoreError> {
    let from = id_at(b)?;
    let (kind, rest) = split_lp(&b[16..])?;
    let (ns, rest) = split_lp(rest)?;
    Ok(FanKey {
        from,
        kind: LinkKind(kind.to_string()),
        key: ExportKey {
            namespace: ns.to_string(),
            key: utf8(rest)?,
        },
    })
}

/// `fan_in` scan prefix: `lp(namespace) || lp(key)`.
pub(crate) fn fan_in_prefix(k: &ExportKey) -> Vec<u8> {
    let mut out = Vec::new();
    lp(&mut out, &k.namespace);
    lp(&mut out, &k.key);
    out
}

/// `fan_in` key: the prefix, the 16-byte `from` and the kind bytes.
pub(crate) fn fan_in_key(k: &FanKey) -> Vec<u8> {
    let mut out = fan_in_prefix(&k.key);
    out.extend_from_slice(&k.from.0);
    out.extend_from_slice(k.kind.0.as_bytes());
    out
}

pub(crate) fn decode_fan_in_key(b: &[u8]) -> Result<FanKey, StoreError> {
    let (ns, rest) = split_lp(b)?;
    let (key, rest) = split_lp(rest)?;
    let from = id_at(rest)?;
    Ok(FanKey {
        from,
        kind: LinkKind(utf8(&rest[16..])?),
        key: ExportKey {
            namespace: ns.to_string(),
            key: key.to_string(),
        },
    })
}

/// `exports_by_target` scan prefix: the target.
pub(crate) fn by_target_prefix(target: Id) -> Vec<u8> {
    target.0.to_vec()
}

/// `exports_by_target` key: `target || lp(namespace) || key bytes`.
pub(crate) fn by_target_key(target: Id, k: &ExportKey) -> Vec<u8> {
    let mut out = by_target_prefix(target);
    lp(&mut out, &k.namespace);
    out.extend_from_slice(k.key.as_bytes());
    out
}

pub(crate) fn decode_by_target_key(b: &[u8]) -> Result<(Id, ExportKey), StoreError> {
    let t = id_at(b)?;
    let (ns, rest) = split_lp(&b[16..])?;
    Ok((
        t,
        ExportKey {
            namespace: ns.to_string(),
            key: utf8(rest)?,
        },
    ))
}

/// The entries of a fan row as they read once expanded to `n` targets.
pub(crate) fn expanded_entries(row: &LinkRow, n: usize) -> Vec<LinkEntry> {
    let fanout = Some(u32::try_from(n).unwrap_or(u32::MAX));
    row.entries
        .iter()
        .map(|e| LinkEntry {
            fanout,
            ..e.clone()
        })
        .collect()
}

/// Merges stored link rows and fan rows expanded to every target, in `links_out` key order.
#[derive(Default)]
pub(crate) struct Expansion {
    rows: BTreeMap<Vec<u8>, (LinkKey, LinkRow)>,
}

impl Expansion {
    pub(crate) fn add_stored(&mut self, key: LinkKey, row: LinkRow) {
        self.rows
            .insert(crate::cache::link_order_key(&key), (key, row));
    }

    /// One virtual row per target; entries of a link that is also stored (or fan out of
    /// another key) are merged in origin order.
    pub(crate) fn add_fan(&mut self, fan: &FanKey, row: &LinkRow, targets: &[Id]) {
        for to in targets {
            let key = LinkKey {
                from: fan.from,
                kind: fan.kind.clone(),
                to: *to,
            };
            let entries = expanded_entries(row, targets.len());
            match self.rows.entry(crate::cache::link_order_key(&key)) {
                std::collections::btree_map::Entry::Vacant(v) => {
                    v.insert((key, LinkRow { entries }));
                }
                std::collections::btree_map::Entry::Occupied(mut o) => {
                    let r = &mut o.get_mut().1;
                    r.entries.extend(entries);
                    r.entries.sort_by_key(|e| e.origin);
                }
            }
        }
    }

    pub(crate) fn finish(self) -> Vec<(LinkKey, LinkRow)> {
        self.rows.into_values().collect()
    }
}
