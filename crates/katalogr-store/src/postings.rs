//! Packed postings (FEAT-075 R-5).
//!
//! A posting row holds the postings of one term for a block of `POSTING_BLOCK` consecutive
//! document ordinals, so a term that occurs in many symbols costs one small row per block and
//! not one row per posting. A symbol gets an ordinal the first time it is indexed and keeps it
//! while it stays indexed; ordinals are never reused. Writes in a transaction are buffered per
//! row and flushed with one read-modify-write per touched row.
//!
//! Row key: `term || 0x00 || block (u32 BE)`. Row value: a varint count `n`, then `n` entries of
//! (varint ordinal delta, varint `tf << 2 | field`), sorted by `(ordinal, field)`; the first
//! delta is from `block * POSTING_BLOCK`.

use crate::cache::{LEXICON_FIELDS, TableId, db};
use crate::error::StoreError;
use katalogr_model::Id;
use redb::{ReadableTable, WriteTransaction};
use std::collections::{BTreeMap, BTreeSet};

/// How many consecutive ordinals share a posting row. Chosen by the FEAT-075 benchmark: 1,024
/// keeps almost all of the size gain of one row per term and makes maintenance cheaper than one
/// row per posting.
pub(crate) const POSTING_BLOCK: u32 = 1024;

const _: () = assert!(LEXICON_FIELDS <= 4);

/// `meta` key: the next unused document ordinal (u32 LE).
pub(crate) const DOC_NEXT_ORD_META: &str = "doc_next_ord";

/// One decoded posting entry: `(ordinal, field, tf)`.
pub(crate) type Entry = (u32, u8, u32);

/// The key of the row that holds `term`'s postings for the block of `ord`.
pub(crate) fn row_key(term: &str, ord: u32) -> Vec<u8> {
    let mut k = prefix(term);
    k.extend_from_slice(&(ord / POSTING_BLOCK).to_be_bytes());
    k
}

/// Scan prefix of one exact term: `term NUL` (terms are alphanumeric).
pub(crate) fn prefix(term: &str) -> Vec<u8> {
    let mut k = term.as_bytes().to_vec();
    k.push(0);
    k
}

/// The block number a row key ends with.
pub(crate) fn block_of_key(k: &[u8]) -> Result<u32, StoreError> {
    let tail = k
        .len()
        .checked_sub(4)
        .and_then(|n| k.get(n..))
        .ok_or_else(|| StoreError::Row("malformed posting".into()))?;
    Ok(u32::from_be_bytes(tail.try_into().expect("4 bytes")))
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub(crate) fn varint(b: &[u8], at: &mut usize) -> Result<u64, StoreError> {
    let bad = || StoreError::Row("malformed varint".into());
    let mut v = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *b.get(*at).ok_or_else(bad)?;
        *at += 1;
        if shift >= 64 {
            return Err(bad());
        }
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
        shift += 7;
    }
}

/// The ordinal of a `doc_ord` value.
pub(crate) fn decode_ord(b: &[u8]) -> Result<u32, StoreError> {
    let mut at = 0;
    let v = varint(b, &mut at)?;
    if at != b.len() {
        return Err(StoreError::Row("bad doc_ord row".into()));
    }
    u32::try_from(v).map_err(|_| StoreError::Row("bad doc_ord row".into()))
}

pub(crate) fn encode_ord(ord: u32) -> Vec<u8> {
    let mut v = Vec::new();
    put_varint(&mut v, u64::from(ord));
    v
}

/// A `doc_by_ord` value: the symbol id, then the four field lengths.
pub(crate) fn encode_doc(id: Id, lens: &[u32; LEXICON_FIELDS]) -> Vec<u8> {
    let mut v = id.0.to_vec();
    for l in lens {
        put_varint(&mut v, u64::from(*l));
    }
    v
}

pub(crate) fn decode_doc(b: &[u8]) -> Result<(Id, [u32; LEXICON_FIELDS]), StoreError> {
    let bad = || StoreError::Row("bad doc_by_ord row".into());
    let id = Id(b.get(..16).ok_or_else(bad)?.try_into().map_err(|_| bad())?);
    let mut at = 16;
    let mut lens = [0u32; LEXICON_FIELDS];
    for l in &mut lens {
        *l = u32::try_from(varint(b, &mut at)?).map_err(|_| bad())?;
    }
    if at != b.len() {
        return Err(bad());
    }
    Ok((id, lens))
}

/// Encodes the entries of the block that starts at `base`. Sorts them by `(ordinal, field)`.
pub(crate) fn encode_block(base: u32, entries: &mut [Entry]) -> Vec<u8> {
    entries.sort_unstable_by_key(|e| (e.0, e.1));
    let mut out = Vec::with_capacity(entries.len() * 3 + 2);
    put_varint(&mut out, entries.len() as u64);
    let mut prev = base;
    for (ord, field, tf) in entries.iter() {
        put_varint(&mut out, u64::from(ord - prev));
        put_varint(&mut out, (u64::from(*tf) << 2) | u64::from(*field));
        prev = *ord;
    }
    out
}

/// Decodes a block that starts at `base`.
pub(crate) fn decode_block(base: u32, b: &[u8]) -> Result<Vec<Entry>, StoreError> {
    let bad = || StoreError::Row("malformed posting".into());
    let mut at = 0;
    let n = varint(b, &mut at)? as usize;
    // Every entry takes at least two bytes: a corrupt count cannot reserve more than the row.
    if n > b.len() {
        return Err(bad());
    }
    let mut out = Vec::with_capacity(n);
    let mut ord = u64::from(base);
    for _ in 0..n {
        ord += varint(b, &mut at)?;
        let packed = varint(b, &mut at)?;
        out.push((
            u32::try_from(ord).map_err(|_| bad())?,
            (packed & 3) as u8,
            u32::try_from(packed >> 2).map_err(|_| bad())?,
        ));
    }
    if at != b.len() {
        return Err(bad());
    }
    Ok(out)
}

/// What one transaction changes in one row.
#[derive(Default)]
pub(crate) struct RowDelta {
    pub(crate) remove: BTreeSet<u32>,
    pub(crate) add: Vec<Entry>,
}

/// The buffered posting writes of a transaction, per row key.
#[derive(Default)]
pub(crate) struct PostingBuf {
    pub(crate) rows: BTreeMap<Vec<u8>, RowDelta>,
}

impl PostingBuf {
    pub(crate) fn add(&mut self, term: &str, ord: u32, field: u8, tf: u32) {
        self.rows
            .entry(row_key(term, ord))
            .or_default()
            .add
            .push((ord, field, tf));
    }

    /// Removes the postings of `ord` from `term`'s row, including any added earlier in the
    /// transaction.
    pub(crate) fn remove(&mut self, term: &str, ord: u32) {
        let row = self.rows.entry(row_key(term, ord)).or_default();
        row.add.retain(|e| e.0 != ord);
        row.remove.insert(ord);
    }
}

#[cfg(test)]
thread_local! {
    /// Rows read by `flush` on this thread.
    pub(crate) static FLUSH_GETS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Applies the buffered writes: one read-modify-write per touched row, in key order. The buffer
/// is empty afterwards.
pub(crate) fn flush(buf: &mut PostingBuf, txn: &WriteTransaction) -> Result<(), StoreError> {
    if buf.rows.is_empty() {
        return Ok(());
    }
    let mut table = txn.open_table(TableId::Postings.def()).map_err(db)?;
    for (key, delta) in std::mem::take(&mut buf.rows) {
        let block = block_of_key(&key)?;
        let base = block * POSTING_BLOCK;
        #[cfg(test)]
        FLUSH_GETS.with(|c| c.set(c.get() + 1));
        let old = table
            .get(key.as_slice())
            .map_err(db)?
            .map(|g| g.value().to_vec());
        let mut entries = match old {
            Some(v) => decode_block(base, &v)?,
            None => Vec::new(),
        };
        entries.retain(|e| !delta.remove.contains(&e.0));
        entries.extend(delta.add);
        entries.sort_unstable_by_key(|e| (e.0, e.1));
        debug_assert!(
            entries
                .windows(2)
                .all(|w| (w[0].0, w[0].1) != (w[1].0, w[1].1)),
            "a posting was added twice"
        );
        // In release the last of two equal `(ordinal, field)` entries wins.
        let mut unique: Vec<Entry> = Vec::with_capacity(entries.len());
        for e in entries {
            match unique.last_mut() {
                Some(last) if (last.0, last.1) == (e.0, e.1) => *last = e,
                _ => unique.push(e),
            }
        }
        if unique.is_empty() {
            table.remove(key.as_slice()).map_err(db)?;
        } else {
            table
                .insert(key.as_slice(), encode_block(base, &mut unique).as_slice())
                .map_err(db)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use redb::Database;

    #[test]
    fn posting_block_roundtrip() {
        for block in [0u32, 1, 4000] {
            let base = block * POSTING_BLOCK;
            let mut entries: Vec<Entry> = vec![
                (base, 0, 1),
                (base, 3, 1 << 30),
                (base + 1, 1, 2),
                (base + POSTING_BLOCK - 1, 3, 1),
                (base + 500, 2, u32::MAX >> 2),
            ];
            entries.reverse();
            let bytes = encode_block(base, &mut entries);
            assert_eq!(decode_block(base, &bytes).unwrap(), entries);
            // Sorted by (ordinal, field).
            assert!(
                entries
                    .windows(2)
                    .all(|w| (w[0].0, w[0].1) < (w[1].0, w[1].1))
            );
        }
        // The boundary ordinals 0, 1023 and 1024 land in blocks 0, 0 and 1.
        assert_eq!(block_of_key(&row_key("t", 0)).unwrap(), 0);
        assert_eq!(block_of_key(&row_key("t", 1023)).unwrap(), 0);
        assert_eq!(block_of_key(&row_key("t", 1024)).unwrap(), 1);
        let empty = encode_block(0, &mut Vec::new());
        assert!(decode_block(0, &empty).unwrap().is_empty());
        // Corrupt rows are errors, not panics or large allocations.
        assert!(decode_block(0, &[0xff, 0xff, 0xff, 0xff, 0x0f]).is_err());
        assert!(decode_block(0, &[2, 1]).is_err());
        assert!(decode_block(0, &[1, 0, 4, 9]).is_err());
        assert!(decode_block(0, &[]).is_err());
    }

    #[test]
    fn doc_rows_roundtrip() {
        let id = Id([7; 16]);
        let lens = [3, 0, 300, u32::MAX];
        assert_eq!(decode_doc(&encode_doc(id, &lens)).unwrap(), (id, lens));
        for ord in [0, 1, 127, 128, u32::MAX] {
            assert_eq!(decode_ord(&encode_ord(ord)).unwrap(), ord);
        }
        assert!(decode_doc(&[0; 5]).is_err());
    }

    #[test]
    fn posting_buffer_one_rmw_per_row() {
        let dir = tempfile::tempdir().unwrap();
        let database = Database::create(dir.path().join("p.redb")).unwrap();
        let txn = database.begin_write().unwrap();
        txn.open_table(TableId::Postings.def()).unwrap();
        let mut buf = PostingBuf::default();
        // 300 postings into 3 rows: two terms in block 0, one in block 1.
        for ord in 0..100u32 {
            buf.add("alpha", ord, 0, 1);
            buf.add("beta", ord, 1, 2);
            buf.add("alpha", 1024 + ord, 0, 3);
        }
        FLUSH_GETS.with(|c| c.set(0));
        flush(&mut buf, &txn).unwrap();
        assert_eq!(FLUSH_GETS.with(std::cell::Cell::get), 3);
        assert!(buf.rows.is_empty());
        // A second flush with removals and additions on the same row is one read again.
        buf.remove("alpha", 5);
        buf.add("alpha", 5, 2, 9);
        buf.remove("alpha", 6);
        FLUSH_GETS.with(|c| c.set(0));
        flush(&mut buf, &txn).unwrap();
        assert_eq!(FLUSH_GETS.with(std::cell::Cell::get), 1);
        let table = txn.open_table(TableId::Postings.def()).unwrap();
        let row = table.get(row_key("alpha", 0).as_slice()).unwrap().unwrap();
        let entries = decode_block(0, row.value()).unwrap();
        assert_eq!(entries.len(), 99);
        assert!(entries.contains(&(5, 2, 9)));
        assert!(!entries.iter().any(|e| e.0 == 6));
        assert!(!entries.contains(&(5, 0, 1)));
        // Removing the last entries deletes the row.
        drop(row);
        drop(table);
        for ord in 0..100 {
            buf.remove("beta", ord);
        }
        flush(&mut buf, &txn).unwrap();
        let table = txn.open_table(TableId::Postings.def()).unwrap();
        assert!(table.get(row_key("beta", 0).as_slice()).unwrap().is_none());
    }

    #[test]
    fn add_then_remove_in_one_transaction_leaves_nothing() {
        let mut buf = PostingBuf::default();
        buf.add("t", 3, 0, 1);
        buf.remove("t", 3);
        let row = buf.rows.get(&row_key("t", 3)).unwrap();
        assert!(row.add.is_empty() && row.remove.contains(&3));
    }
}
