//! Vocabulary and provenance dictionaries (FEAT-075 R-2, R-3).
//!
//! Strings that repeat across rows (symbol and link kinds, export namespaces, extension ids) get
//! a 16-bit id in `vocab`; provenance triples get a 32-bit id in `prov`. A vocabulary id is never
//! reused or deleted. A provenance row carries the number of rows that name it and is deleted when
//! that reaches zero. Everything is written inside the transaction that needs it, so an abort
//! leaves neither dangling ids nor leaked counts.

use crate::cache::{Reader, TableId, lp};
use crate::codec::{self, Interner, Pid, Resolver, Vid, WireProv};
use crate::error::StoreError;
use katalogr_model::Provenance;
use redb::{ReadableTable, WriteTransaction};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// `meta` key: the next provenance id (u32 LE).
pub(crate) const PROV_NEXT_META: &str = "prov_next";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a transaction has resolved or assigned so far. Dictionary ids are immutable once
/// committed, so a memo never goes stale within a snapshot or a write transaction.
#[doc(hidden)]
#[derive(Default)]
pub struct Memo {
    vocab: Mutex<HashMap<Vid, String>>,
    vocab_ix: Mutex<HashMap<String, Vid>>,
    prov: Mutex<HashMap<Pid, Provenance>>,
    prov_ix: Mutex<HashMap<Vec<u8>, Pid>>,
}

impl Memo {
    fn remember_vocab(&self, id: Vid, s: &str) {
        lock(&self.vocab).insert(id, s.to_string());
        lock(&self.vocab_ix).insert(s.to_string(), id);
    }

    fn remember_prov(&self, id: Pid, p: &Provenance) {
        lock(&self.prov).insert(id, p.clone());
        lock(&self.prov_ix).insert(prov_ix_key(p), id);
    }
}

/// `prov_ix` key: `lp(repo) || lp(path) || revision`.
pub(crate) fn prov_ix_key(p: &Provenance) -> Vec<u8> {
    let mut k = Vec::new();
    lp(&mut k, &p.repo);
    lp(&mut k, p.path.as_str());
    k.extend_from_slice(p.revision.as_bytes());
    k
}

fn dangling() -> StoreError {
    StoreError::Row("dangling dictionary id".into())
}

/// The string of vocabulary id `id`.
pub(crate) fn vocab_str<R: Reader + ?Sized>(tx: &R, id: Vid) -> Result<String, StoreError> {
    if let Some(s) = lock(&tx.memo().vocab).get(&id) {
        return Ok(s.clone());
    }
    let raw = tx
        .raw_get(TableId::Vocab, &id.to_be_bytes())?
        .ok_or_else(dangling)?;
    let s = String::from_utf8(raw).map_err(|_| dangling())?;
    tx.memo().remember_vocab(id, &s);
    Ok(s)
}

/// The id of `s`, or `None` when it was never written. Reads only.
pub(crate) fn vid_of<R: Reader + ?Sized>(tx: &R, s: &str) -> Result<Option<Vid>, StoreError> {
    if let Some(id) = lock(&tx.memo().vocab_ix).get(s) {
        return Ok(Some(*id));
    }
    let Some(raw) = tx.raw_get(TableId::VocabIx, s.as_bytes())? else {
        return Ok(None);
    };
    let id = Vid::from_be_bytes(raw.as_slice().try_into().map_err(|_| dangling())?);
    tx.memo().remember_vocab(id, s);
    Ok(Some(id))
}

/// The provenance of id `id`.
pub(crate) fn prov_of<R: Reader + ?Sized>(tx: &R, id: Pid) -> Result<Provenance, StoreError> {
    if let Some(p) = lock(&tx.memo().prov).get(&id) {
        return Ok(p.clone());
    }
    let raw = tx
        .raw_get(TableId::Prov, &id.to_be_bytes())?
        .ok_or_else(dangling)?;
    let w: WireProv = codec::dec(&raw)?;
    let p = Provenance {
        repo: w.repo,
        path: w.path,
        revision: w.revision,
    };
    tx.memo().remember_prov(id, &p);
    Ok(p)
}

/// Resolves ids through a reader's memo and the dictionary tables.
pub(crate) struct TxRes<'a, R: Reader + ?Sized>(pub(crate) &'a R);

impl<R: Reader + ?Sized> Resolver for TxRes<'_, R> {
    fn vstr(&self, id: Vid) -> Result<String, StoreError> {
        vocab_str(self.0, id)
    }
    fn prov(&self, id: Pid) -> Result<Provenance, StoreError> {
        prov_of(self.0, id)
    }
}

/// Assigns ids inside a write transaction and counts the provenance references it creates.
pub(crate) struct TxDict<'a> {
    pub(crate) txn: &'a WriteTransaction,
    pub(crate) memo: &'a Memo,
    pub(crate) delta: &'a mut BTreeMap<Pid, i64>,
}

fn db<E: Into<redb::Error>>(e: E) -> StoreError {
    StoreError::Db(e.into().to_string())
}

impl TxDict<'_> {
    fn get(&self, t: TableId, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        let table = self.txn.open_table(t.def()).map_err(db)?;
        let v = table.get(key).map_err(db)?.map(|g| g.value().to_vec());
        Ok(v)
    }

    fn put(&self, t: TableId, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        let mut table = self.txn.open_table(t.def()).map_err(db)?;
        table.insert(key, value).map_err(db)?;
        Ok(())
    }

    /// The id of `s`, without assigning one.
    fn lookup_vid(&self, s: &str) -> Result<Option<Vid>, StoreError> {
        if let Some(id) = lock(&self.memo.vocab_ix).get(s) {
            return Ok(Some(*id));
        }
        match self.get(TableId::VocabIx, s.as_bytes())? {
            Some(raw) => {
                let id = Vid::from_be_bytes(raw.as_slice().try_into().map_err(|_| dangling())?);
                self.memo.remember_vocab(id, s);
                Ok(Some(id))
            }
            None => Ok(None),
        }
    }
}

impl Interner for TxDict<'_> {
    fn vid(&mut self, s: &str) -> Result<Vid, StoreError> {
        if let Some(id) = self.lookup_vid(s)? {
            return Ok(id);
        }
        let next = {
            let table = self.txn.open_table(TableId::Vocab.def()).map_err(db)?;
            let last = table.last().map_err(db)?;
            match last {
                None => 0,
                Some((k, _)) => {
                    let last = Vid::from_be_bytes(k.value().try_into().map_err(|_| dangling())?);
                    last.checked_add(1)
                        .ok_or_else(|| StoreError::Row("vocabulary is full".into()))?
                }
            }
        };
        self.put(TableId::Vocab, &next.to_be_bytes(), s.as_bytes())?;
        self.put(TableId::VocabIx, s.as_bytes(), &next.to_be_bytes())?;
        self.memo.remember_vocab(next, s);
        Ok(next)
    }

    fn pid(&mut self, p: &Provenance) -> Result<Pid, StoreError> {
        let key = prov_ix_key(p);
        let known = lock(&self.memo.prov_ix).get(&key).copied();
        let id = match known {
            Some(id) => id,
            None => match self.get(TableId::ProvIx, &key)? {
                Some(raw) => {
                    let id = Pid::from_be_bytes(raw.as_slice().try_into().map_err(|_| dangling())?);
                    self.memo.remember_prov(id, p);
                    id
                }
                None => {
                    let next = match self.get(TableId::Meta, PROV_NEXT_META.as_bytes())? {
                        Some(v) => u32::from_le_bytes(
                            v.as_slice()
                                .try_into()
                                .map_err(|_| StoreError::Row("bad prov_next".into()))?,
                        ),
                        None => 0,
                    };
                    let after = next
                        .checked_add(1)
                        .ok_or_else(|| StoreError::Row("provenance ids exhausted".into()))?;
                    let row = WireProv {
                        repo: p.repo.clone(),
                        path: p.path.clone(),
                        revision: p.revision.clone(),
                        refs: 0,
                    };
                    self.put(TableId::Prov, &next.to_be_bytes(), &codec::enc(&row)?)?;
                    self.put(TableId::ProvIx, &key, &next.to_be_bytes())?;
                    self.put(
                        TableId::Meta,
                        PROV_NEXT_META.as_bytes(),
                        &after.to_le_bytes(),
                    )?;
                    self.memo.remember_prov(next, p);
                    next
                }
            },
        };
        *self.delta.entry(id).or_insert(0) += 1;
        Ok(id)
    }
}

/// Applies the reference counts a transaction accumulated, in ascending id order. A count that
/// reaches zero deletes the provenance row; one that would go negative aborts the transaction.
pub(crate) fn apply_prov_deltas(
    txn: &WriteTransaction,
    deltas: &BTreeMap<Pid, i64>,
) -> Result<(), StoreError> {
    if deltas.is_empty() {
        return Ok(());
    }
    let mut prov = txn.open_table(TableId::Prov.def()).map_err(db)?;
    let mut ix = txn.open_table(TableId::ProvIx.def()).map_err(db)?;
    for (pid, delta) in deltas {
        let key = pid.to_be_bytes();
        let Some(raw) = prov
            .get(key.as_slice())
            .map_err(db)?
            .map(|g| g.value().to_vec())
        else {
            if *delta == 0 {
                continue;
            }
            return Err(dangling());
        };
        let mut row: WireProv = codec::dec(&raw)?;
        let refs = i64::from(row.refs) + delta;
        if refs < 0 {
            return Err(StoreError::Row("provenance refcount underflow".into()));
        }
        if refs == 0 {
            let p = Provenance {
                repo: row.repo,
                path: row.path,
                revision: row.revision,
            };
            ix.remove(prov_ix_key(&p).as_slice()).map_err(db)?;
            prov.remove(key.as_slice()).map_err(db)?;
        } else if *delta != 0 {
            row.refs = u32::try_from(refs)
                .map_err(|_| StoreError::Row("provenance refcount overflow".into()))?;
            prov.insert(key.as_slice(), codec::enc(&row)?.as_slice())
                .map_err(db)?;
        }
    }
    Ok(())
}
