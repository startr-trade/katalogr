//! Local redb cache rebuilt from the shards (FEAT-003 section 3 to 8, FEAT-001 section 5).
//!
//! Every table is `&[u8] -> &[u8]`. Keys are built by the functions below (big-endian, length
//! prefixed strings, so ranges by prefix are exact). Values are compact `serde_json` bytes of the
//! row types, which are deterministic because every map in them is ordered.

use crate::error::StoreError;
use crate::kindex::ProjectDir;
use camino::Utf8PathBuf;
use katalogr_model::{
    ArtifactName, Confidence, Export, ExportKey, Fact, Fragment, Id, LinkKind, OutRef, Provenance,
    RelPath, Span, SymbolName,
};
use redb::{
    Database, DatabaseError, ReadTransaction, ReadableDatabase, ReadableTable, StorageError,
    TableDefinition, WriteTransaction,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::ops::Bound;
use std::time::{Duration, Instant};

/// Stored in `meta`; a different number rebuilds the cache from the shards.
pub const CACHE_SCHEMA: u32 = 1;

// ---------------------------------------------------------------- rows

/// What the cache keeps about one artifact. `symbols`, `exports` and `refs` let the stitcher
/// diff and remove a fragment without the shard; `refs` is indexed by `RefId::index`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRow {
    pub name: ArtifactName,
    pub extension: String,
    pub provenance: Provenance,
    pub facts: BTreeMap<String, Fact>,
    /// Sorted.
    pub symbols: Vec<Id>,
    /// Sorted and deduplicated.
    pub exports: Vec<Export>,
    pub refs: Vec<OutRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolRow {
    pub name: SymbolName,
    pub artifact: Id,
    pub kind: String,
    pub span: Span,
    pub facts: BTreeMap<String, Fact>,
}

/// Where a link came from: a link inside the file, or the resolution of the file's n-th ref.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LinkOrigin {
    Intra,
    Ref(u32),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkEntry {
    pub origin: LinkOrigin,
    pub confidence: Confidence,
    pub span: Option<Span>,
    pub provenance: Provenance,
}

/// One row per `(from, kind, to)`; several origins can support the same row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRow {
    /// Sorted by origin, never empty.
    pub entries: Vec<LinkEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LinkKey {
    pub from: Id,
    pub kind: LinkKind,
    pub to: Id,
}

/// A reference: the artifact that holds it and its position in `ArtifactRow::refs`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RefId {
    pub artifact: Id,
    pub index: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorRow {
    pub span: Option<Span>,
    pub message: String,
}

/// A queued path: the key of the `pending` table (FEAT-047 section 5).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PendingKey {
    pub context: String,
    pub repo: String,
    pub path: RelPath,
}

/// One publish rename: `(temp, final)`, both relative to the project directory. An empty temp
/// name means "remove the final file".
pub type JournalEntry = (String, String);

// ---------------------------------------------------------------- keys

fn lp(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn export_prefix(namespace: &str) -> Vec<u8> {
    let mut k = Vec::new();
    lp(&mut k, namespace);
    k
}

fn export_key(k: &ExportKey) -> Vec<u8> {
    let mut out = export_prefix(&k.namespace);
    out.extend_from_slice(k.key.as_bytes());
    out
}

fn path_key(repo: &str, path: &RelPath) -> Vec<u8> {
    let mut k = Vec::new();
    lp(&mut k, repo);
    k.extend_from_slice(path.as_str().as_bytes());
    k
}

fn split_lp(b: &[u8]) -> Result<(&str, &[u8]), StoreError> {
    let bad = || StoreError::Row("malformed key".into());
    let len = u32::from_be_bytes(b.get(..4).ok_or_else(bad)?.try_into().unwrap()) as usize;
    let s = b.get(4..4 + len).ok_or_else(bad)?;
    Ok((std::str::from_utf8(s).map_err(|_| bad())?, &b[4 + len..]))
}

fn decode_path_key(b: &[u8]) -> Result<(String, RelPath), StoreError> {
    let (repo, rest) = split_lp(b)?;
    let path = std::str::from_utf8(rest).map_err(|_| StoreError::Row("malformed key".into()))?;
    Ok((repo.to_string(), RelPath::new(path)?))
}

fn pending_prefix(context: &str) -> Vec<u8> {
    let mut k = Vec::new();
    lp(&mut k, context);
    k
}

fn pending_key(k: &PendingKey) -> Vec<u8> {
    let mut out = pending_prefix(&k.context);
    lp(&mut out, &k.repo);
    out.extend_from_slice(k.path.as_str().as_bytes());
    out
}

fn decode_pending_key(b: &[u8]) -> Result<PendingKey, StoreError> {
    let (context, rest) = split_lp(b)?;
    let (repo, rest) = split_lp(rest)?;
    let path = std::str::from_utf8(rest).map_err(|_| StoreError::Row("malformed key".into()))?;
    Ok(PendingKey {
        context: context.to_string(),
        repo: repo.to_string(),
        path: RelPath::new(path)?,
    })
}

fn event_key(context: &str, seq: u64) -> Vec<u8> {
    let mut k = pending_prefix(context);
    k.extend_from_slice(&seq.to_be_bytes());
    k
}

fn u64_value(v: Vec<u8>) -> Result<u64, StoreError> {
    Ok(u64::from_le_bytes(v.try_into().map_err(|_| {
        StoreError::Row("bad counter value".into())
    })?))
}

fn link_prefix(from: Id, kind: &LinkKind) -> Vec<u8> {
    let mut k = from.0.to_vec();
    lp(&mut k, &kind.0);
    k
}

fn link_out_key(k: &LinkKey) -> Vec<u8> {
    let mut out = link_prefix(k.from, &k.kind);
    out.extend_from_slice(&k.to.0);
    out
}

fn link_in_key(k: &LinkKey) -> Vec<u8> {
    link_out_key(&LinkKey {
        from: k.to,
        kind: k.kind.clone(),
        to: k.from,
    })
}

fn decode_link_key(b: &[u8]) -> Result<LinkKey, StoreError> {
    let bad = || StoreError::Row("malformed key".into());
    let from: [u8; 16] = b.get(..16).ok_or_else(bad)?.try_into().unwrap();
    let (kind, rest) = split_lp(&b[16..])?;
    let to: [u8; 16] = rest.try_into().map_err(|_| bad())?;
    Ok(LinkKey {
        from: Id(from),
        kind: LinkKind(kind.to_string()),
        to: Id(to),
    })
}

fn ref_key(r: RefId) -> Vec<u8> {
    let mut k = r.artifact.0.to_vec();
    k.extend_from_slice(&r.index.to_be_bytes());
    k
}

fn decode_ref_key(b: &[u8]) -> Result<RefId, StoreError> {
    let bad = || StoreError::Row("malformed key".into());
    if b.len() != 20 {
        return Err(bad());
    }
    Ok(RefId {
        artifact: Id(b[..16].try_into().unwrap()),
        index: u32::from_be_bytes(b[16..].try_into().unwrap()),
    })
}

/// Smallest key greater than every key that starts with `prefix` (None: no upper bound).
fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    while let Some(last) = end.pop() {
        if last < 0xff {
            end.push(last + 1);
            return Some(end);
        }
    }
    None
}

fn ser<T: Serialize>(v: &T) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(v).map_err(|e| StoreError::Row(e.to_string()))
}

fn de<T: DeserializeOwned>(b: &[u8]) -> Result<T, StoreError> {
    serde_json::from_slice(b).map_err(|e| StoreError::Row(e.to_string()))
}

fn db<E: Into<redb::Error>>(e: E) -> StoreError {
    match e.into() {
        redb::Error::DatabaseAlreadyOpen => StoreError::Busy,
        other => StoreError::Db(other.to_string()),
    }
}

// ---------------------------------------------------------------- tables

/// Raw `(key, value)` rows of a table scan.
pub type RawRows = Vec<(Vec<u8>, Vec<u8>)>;
/// `(repo, path)` of a shard.
pub type ShardKey = (String, RelPath);

/// The cache tables (FEAT-001 section 5, FEAT-003 section 5).
#[doc(hidden)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TableId {
    Meta,
    ShardHashes,
    Artifacts,
    Symbols,
    LinksOut,
    LinksIn,
    Exports,
    RefsByKey,
    RefsUnresolved,
    IndexErrors,
    Seq,
    Seen,
    Events,
    Pending,
    Watermark,
    Journal,
}

impl TableId {
    const ALL: [TableId; 16] = [
        TableId::Meta,
        TableId::ShardHashes,
        TableId::Artifacts,
        TableId::Symbols,
        TableId::LinksOut,
        TableId::LinksIn,
        TableId::Exports,
        TableId::RefsByKey,
        TableId::RefsUnresolved,
        TableId::IndexErrors,
        TableId::Seq,
        TableId::Seen,
        TableId::Events,
        TableId::Pending,
        TableId::Watermark,
        TableId::Journal,
    ];

    /// The graph tables, in the order `dump_graph` reports them.
    const GRAPH: [TableId; 8] = [
        TableId::Artifacts,
        TableId::Symbols,
        TableId::LinksOut,
        TableId::LinksIn,
        TableId::Exports,
        TableId::RefsByKey,
        TableId::RefsUnresolved,
        TableId::IndexErrors,
    ];

    fn name(self) -> &'static str {
        match self {
            TableId::Meta => "meta",
            TableId::ShardHashes => "shard_hashes",
            TableId::Artifacts => "artifacts",
            TableId::Symbols => "symbols",
            TableId::LinksOut => "links_out",
            TableId::LinksIn => "links_in",
            TableId::Exports => "exports",
            TableId::RefsByKey => "refs_by_key",
            TableId::RefsUnresolved => "refs_unresolved",
            TableId::IndexErrors => "index_errors",
            TableId::Seq => "seq",
            TableId::Seen => "seen",
            TableId::Events => "events",
            TableId::Pending => "pending",
            TableId::Watermark => "watermark",
            TableId::Journal => "journal",
        }
    }

    fn def(self) -> TableDefinition<'static, &'static [u8], &'static [u8]> {
        TableDefinition::new(self.name())
    }
}

fn get_in<T: ReadableTable<&'static [u8], &'static [u8]>>(
    t: &T,
    key: &[u8],
) -> Result<Option<Vec<u8>>, StoreError> {
    Ok(t.get(key).map_err(db)?.map(|g| g.value().to_vec()))
}

fn scan_in<T: ReadableTable<&'static [u8], &'static [u8]>>(
    t: &T,
    prefix: &[u8],
) -> Result<RawRows, StoreError> {
    let hi = match prefix_end(prefix) {
        Some(e) => Bound::Excluded(e),
        None => Bound::Unbounded,
    };
    let hi_ref = match &hi {
        Bound::Excluded(e) => Bound::Excluded(e.as_slice()),
        _ => Bound::Unbounded,
    };
    let it = t
        .range::<&[u8]>((Bound::Included(prefix), hi_ref))
        .map_err(db)?;
    let mut out = Vec::new();
    for r in it {
        let (k, v) = r.map_err(db)?;
        out.push((k.value().to_vec(), v.value().to_vec()));
    }
    Ok(out)
}

// ---------------------------------------------------------------- reading

/// Typed reads, shared by [`ReadTx`] (a snapshot) and [`WriteTx`] (sees its own writes).
/// Collections are materialised; ranges are exact prefix scans.
pub trait Reader {
    #[doc(hidden)]
    fn raw_get(&self, t: TableId, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError>;
    #[doc(hidden)]
    fn raw_scan(&self, t: TableId, prefix: &[u8]) -> Result<RawRows, StoreError>;

    fn meta(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.raw_get(TableId::Meta, name.as_bytes())
    }

    fn shard_hash(&self, repo: &str, path: &RelPath) -> Result<Option<[u8; 32]>, StoreError> {
        match self.raw_get(TableId::ShardHashes, &path_key(repo, path))? {
            Some(v) => Ok(Some(
                v.try_into()
                    .map_err(|_| StoreError::Row("bad shard hash".into()))?,
            )),
            None => Ok(None),
        }
    }

    /// Every stored shard hash, sorted by `(repo, path)` key bytes.
    fn shard_hashes(&self) -> Result<Vec<(ShardKey, [u8; 32])>, StoreError> {
        let mut out = Vec::new();
        for (k, v) in self.raw_scan(TableId::ShardHashes, &[])? {
            let h = v
                .try_into()
                .map_err(|_| StoreError::Row("bad shard hash".into()))?;
            out.push((decode_path_key(&k)?, h));
        }
        Ok(out)
    }

    fn artifact(&self, id: Id) -> Result<Option<ArtifactRow>, StoreError> {
        self.raw_get(TableId::Artifacts, &id.0)?
            .map(|v| de(&v))
            .transpose()
    }

    fn artifacts(&self) -> Result<Vec<(Id, ArtifactRow)>, StoreError> {
        self.raw_scan(TableId::Artifacts, &[])?
            .into_iter()
            .map(|(k, v)| Ok((Id(id_bytes(&k)?), de(&v)?)))
            .collect()
    }

    fn symbol(&self, id: Id) -> Result<Option<SymbolRow>, StoreError> {
        self.raw_get(TableId::Symbols, &id.0)?
            .map(|v| de(&v))
            .transpose()
    }

    fn symbols(&self) -> Result<Vec<(Id, SymbolRow)>, StoreError> {
        self.raw_scan(TableId::Symbols, &[])?
            .into_iter()
            .map(|(k, v)| Ok((Id(id_bytes(&k)?), de(&v)?)))
            .collect()
    }

    fn link(&self, key: &LinkKey) -> Result<Option<LinkRow>, StoreError> {
        self.raw_get(TableId::LinksOut, &link_out_key(key))?
            .map(|v| de(&v))
            .transpose()
    }

    /// Links leaving `from`, optionally only of one kind.
    fn links_out(
        &self,
        from: Id,
        kind: Option<&LinkKind>,
    ) -> Result<Vec<(LinkKey, LinkRow)>, StoreError> {
        let prefix = match kind {
            Some(k) => link_prefix(from, k),
            None => from.0.to_vec(),
        };
        self.raw_scan(TableId::LinksOut, &prefix)?
            .into_iter()
            .map(|(k, v)| Ok((decode_link_key(&k)?, de(&v)?)))
            .collect()
    }

    /// Keys of the links arriving at `to` (each is a `from -> to` link, reported in link direction).
    fn links_in(&self, to: Id) -> Result<Vec<LinkKey>, StoreError> {
        self.raw_scan(TableId::LinksIn, &to.0)?
            .into_iter()
            .map(|(k, _)| {
                let r = decode_link_key(&k)?;
                Ok(LinkKey {
                    from: r.to,
                    kind: r.kind,
                    to: r.from,
                })
            })
            .collect()
    }

    /// Targets exported under `key` (empty when none), sorted.
    fn exports(&self, key: &ExportKey) -> Result<Vec<Id>, StoreError> {
        Ok(self
            .raw_get(TableId::Exports, &export_key(key))?
            .map(|v| de(&v))
            .transpose()?
            .unwrap_or_default())
    }

    /// Every `(key, targets)` in a namespace.
    fn exports_in(&self, namespace: &str) -> Result<Vec<(ExportKey, Vec<Id>)>, StoreError> {
        let prefix = export_prefix(namespace);
        self.raw_scan(TableId::Exports, &prefix)?
            .into_iter()
            .map(|(k, v)| {
                let key = std::str::from_utf8(&k[prefix.len()..])
                    .map_err(|_| StoreError::Row("malformed key".into()))?;
                Ok((
                    ExportKey {
                        namespace: namespace.to_string(),
                        key: key.to_string(),
                    },
                    de(&v)?,
                ))
            })
            .collect()
    }

    /// The references that name `key` among their candidates, sorted.
    fn refs_by_key(&self, key: &ExportKey) -> Result<Vec<RefId>, StoreError> {
        Ok(self
            .raw_get(TableId::RefsByKey, &export_key(key))?
            .map(|v| de(&v))
            .transpose()?
            .unwrap_or_default())
    }

    fn unresolved(&self, r: RefId) -> Result<Option<OutRef>, StoreError> {
        self.raw_get(TableId::RefsUnresolved, &ref_key(r))?
            .map(|v| de(&v))
            .transpose()
    }

    /// Unresolved references of one artifact (`Some`) or of all (`None`).
    fn unresolved_refs(&self, artifact: Option<Id>) -> Result<Vec<(RefId, OutRef)>, StoreError> {
        let prefix = artifact.map(|a| a.0.to_vec()).unwrap_or_default();
        self.raw_scan(TableId::RefsUnresolved, &prefix)?
            .into_iter()
            .map(|(k, v)| Ok((decode_ref_key(&k)?, de(&v)?)))
            .collect()
    }

    fn index_errors(&self, repo: &str, path: &RelPath) -> Result<Vec<ErrorRow>, StoreError> {
        Ok(self
            .raw_get(TableId::IndexErrors, &path_key(repo, path))?
            .map(|v| de(&v))
            .transpose()?
            .unwrap_or_default())
    }

    /// Last sequence number assigned in `context` (0 when none).
    fn last_seq(&self, context: &str) -> Result<u64, StoreError> {
        self.raw_get(TableId::Seq, context.as_bytes())?
            .map_or(Ok(0), u64_value)
    }

    /// The sequence number an event id was ingested under.
    fn seen_seq(&self, id: &str) -> Result<Option<u64>, StoreError> {
        self.raw_get(TableId::Seen, id.as_bytes())?
            .map(u64_value)
            .transpose()
    }

    /// The stored bytes of event `(context, seq)`.
    fn event_bytes(&self, context: &str, seq: u64) -> Result<Option<Vec<u8>>, StoreError> {
        self.raw_get(TableId::Events, &event_key(context, seq))
    }

    /// Number of stored events of one context.
    fn event_count(&self, context: &str) -> Result<usize, StoreError> {
        Ok(self
            .raw_scan(TableId::Events, &pending_prefix(context))?
            .len())
    }

    /// The latest sequence number queued for one path.
    fn pending_seq(&self, key: &PendingKey) -> Result<Option<u64>, StoreError> {
        self.raw_get(TableId::Pending, &pending_key(key))?
            .map(u64_value)
            .transpose()
    }

    /// Queued paths of one context (`Some`) or of all (`None`), sorted by key, with the latest
    /// sequence number of each.
    fn pending_entries(&self, context: Option<&str>) -> Result<Vec<(PendingKey, u64)>, StoreError> {
        let prefix = context.map(pending_prefix).unwrap_or_default();
        self.raw_scan(TableId::Pending, &prefix)?
            .into_iter()
            .map(|(k, v)| Ok((decode_pending_key(&k)?, u64_value(v)?)))
            .collect()
    }

    /// Everything with a sequence number up to this is reflected in the published results
    /// (0 when nothing was published).
    fn watermark(&self, context: &str) -> Result<u64, StoreError> {
        self.raw_get(TableId::Watermark, context.as_bytes())?
            .map_or(Ok(0), u64_value)
    }

    /// The renames of a publish that has committed but not yet finished.
    fn publish_journal(&self) -> Result<Vec<JournalEntry>, StoreError> {
        Ok(self
            .raw_get(TableId::Journal, &[])?
            .map(|v| de(&v))
            .transpose()?
            .unwrap_or_default())
    }

    /// Every graph table as raw `(key, value)` rows, in a fixed table order. Two caches built
    /// from the same shards give equal dumps.
    fn dump_graph(&self) -> Result<Vec<(&'static str, RawRows)>, StoreError> {
        TableId::GRAPH
            .iter()
            .map(|t| Ok((t.name(), self.raw_scan(*t, &[])?)))
            .collect()
    }
}

fn id_bytes(k: &[u8]) -> Result<[u8; 16], StoreError> {
    k.try_into()
        .map_err(|_| StoreError::Row("malformed key".into()))
}

/// MVCC snapshot of the cache.
pub struct ReadTx<'a> {
    txn: ReadTransaction,
    _cache: PhantomData<&'a Cache>,
}

/// The engine's name for a read view of the graph.
pub type Snapshot<'a> = ReadTx<'a>;

impl Reader for ReadTx<'_> {
    fn raw_get(&self, t: TableId, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        get_in(&self.txn.open_table(t.def()).map_err(db)?, key)
    }

    fn raw_scan(&self, t: TableId, prefix: &[u8]) -> Result<RawRows, StoreError> {
        scan_in(&self.txn.open_table(t.def()).map_err(db)?, prefix)
    }
}

// ---------------------------------------------------------------- writing

/// The single writer. Dropping it without [`WriteTx::commit`] abandons every change.
pub struct WriteTx<'a> {
    txn: WriteTransaction,
    _cache: PhantomData<&'a Cache>,
}

impl Reader for WriteTx<'_> {
    fn raw_get(&self, t: TableId, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        get_in(&self.txn.open_table(t.def()).map_err(db)?, key)
    }

    fn raw_scan(&self, t: TableId, prefix: &[u8]) -> Result<RawRows, StoreError> {
        scan_in(&self.txn.open_table(t.def()).map_err(db)?, prefix)
    }
}

impl WriteTx<'_> {
    fn put(&mut self, t: TableId, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        let mut table = self.txn.open_table(t.def()).map_err(db)?;
        table.insert(key, value).map_err(db)?;
        Ok(())
    }

    fn del(&mut self, t: TableId, key: &[u8]) -> Result<(), StoreError> {
        let mut table = self.txn.open_table(t.def()).map_err(db)?;
        table.remove(key).map_err(db)?;
        Ok(())
    }

    pub fn set_meta(&mut self, name: &str, value: &[u8]) -> Result<(), StoreError> {
        self.put(TableId::Meta, name.as_bytes(), value)
    }

    pub fn put_shard_hash(
        &mut self,
        repo: &str,
        path: &RelPath,
        hash: &[u8; 32],
    ) -> Result<(), StoreError> {
        self.put(TableId::ShardHashes, &path_key(repo, path), hash)
    }

    pub fn remove_shard_hash(&mut self, repo: &str, path: &RelPath) -> Result<(), StoreError> {
        self.del(TableId::ShardHashes, &path_key(repo, path))
    }

    pub fn put_artifact(&mut self, id: Id, row: &ArtifactRow) -> Result<(), StoreError> {
        self.put(TableId::Artifacts, &id.0, &ser(row)?)
    }

    pub fn remove_artifact_row(&mut self, id: Id) -> Result<(), StoreError> {
        self.del(TableId::Artifacts, &id.0)
    }

    pub fn put_symbol(&mut self, id: Id, row: &SymbolRow) -> Result<(), StoreError> {
        self.put(TableId::Symbols, &id.0, &ser(row)?)
    }

    pub fn remove_symbol(&mut self, id: Id) -> Result<(), StoreError> {
        self.del(TableId::Symbols, &id.0)
    }

    /// Writes `links_out` and the matching `links_in` entry.
    pub fn put_link(&mut self, key: &LinkKey, row: &LinkRow) -> Result<(), StoreError> {
        self.put(TableId::LinksOut, &link_out_key(key), &ser(row)?)?;
        self.put(TableId::LinksIn, &link_in_key(key), &[])
    }

    /// Removes `links_out` and the matching `links_in` entry.
    pub fn remove_link(&mut self, key: &LinkKey) -> Result<(), StoreError> {
        self.del(TableId::LinksOut, &link_out_key(key))?;
        self.del(TableId::LinksIn, &link_in_key(key))
    }

    /// An empty set removes the row.
    pub fn put_exports(&mut self, key: &ExportKey, targets: &[Id]) -> Result<(), StoreError> {
        if targets.is_empty() {
            self.del(TableId::Exports, &export_key(key))
        } else {
            self.put(TableId::Exports, &export_key(key), &ser(&targets)?)
        }
    }

    /// An empty set removes the row.
    pub fn put_refs_by_key(&mut self, key: &ExportKey, refs: &[RefId]) -> Result<(), StoreError> {
        if refs.is_empty() {
            self.del(TableId::RefsByKey, &export_key(key))
        } else {
            self.put(TableId::RefsByKey, &export_key(key), &ser(&refs)?)
        }
    }

    pub fn put_unresolved(&mut self, r: RefId, out: &OutRef) -> Result<(), StoreError> {
        self.put(TableId::RefsUnresolved, &ref_key(r), &ser(out)?)
    }

    pub fn remove_unresolved(&mut self, r: RefId) -> Result<(), StoreError> {
        self.del(TableId::RefsUnresolved, &ref_key(r))
    }

    /// An empty list removes the row.
    pub fn put_index_errors(
        &mut self,
        repo: &str,
        path: &RelPath,
        rows: &[ErrorRow],
    ) -> Result<(), StoreError> {
        if rows.is_empty() {
            self.del(TableId::IndexErrors, &path_key(repo, path))
        } else {
            self.put(TableId::IndexErrors, &path_key(repo, path), &ser(&rows)?)
        }
    }

    pub fn set_last_seq(&mut self, context: &str, seq: u64) -> Result<(), StoreError> {
        self.put(TableId::Seq, context.as_bytes(), &seq.to_le_bytes())
    }

    pub fn put_seen(&mut self, id: &str, seq: u64) -> Result<(), StoreError> {
        self.put(TableId::Seen, id.as_bytes(), &seq.to_le_bytes())
    }

    pub fn put_event(&mut self, context: &str, seq: u64, bytes: &[u8]) -> Result<(), StoreError> {
        self.put(TableId::Events, &event_key(context, seq), bytes)
    }

    /// Removes the events of `context` whose sequence number is at or below `upto`.
    pub fn prune_events(&mut self, context: &str, upto: u64) -> Result<(), StoreError> {
        let prefix = pending_prefix(context);
        for (k, _) in self.raw_scan(TableId::Events, &prefix)? {
            let seq = u64::from_be_bytes(
                k[prefix.len()..]
                    .try_into()
                    .map_err(|_| StoreError::Row("malformed key".into()))?,
            );
            if seq <= upto {
                self.del(TableId::Events, &k)?;
            }
        }
        Ok(())
    }

    pub fn put_pending(&mut self, key: &PendingKey, seq: u64) -> Result<(), StoreError> {
        self.put(TableId::Pending, &pending_key(key), &seq.to_le_bytes())
    }

    pub fn remove_pending(&mut self, key: &PendingKey) -> Result<(), StoreError> {
        self.del(TableId::Pending, &pending_key(key))
    }

    pub fn set_watermark(&mut self, context: &str, seq: u64) -> Result<(), StoreError> {
        self.put(TableId::Watermark, context.as_bytes(), &seq.to_le_bytes())
    }

    /// An empty list clears the journal.
    pub fn set_publish_journal(&mut self, entries: &[JournalEntry]) -> Result<(), StoreError> {
        if entries.is_empty() {
            self.del(TableId::Journal, &[])
        } else {
            self.put(TableId::Journal, &[], &ser(&entries)?)
        }
    }

    pub fn commit(self) -> Result<(), StoreError> {
        self.txn.commit().map_err(db)
    }
}

// ---------------------------------------------------------------- cache

/// What a fragment-level writer must provide so the store can rebuild the graph tables from the
/// shards without depending on the engine.
pub trait Restitch {
    fn replace_fragment(&self, tx: &mut WriteTx, f: Fragment) -> Result<(), StoreError>;
    fn remove_artifact(&self, tx: &mut WriteTx, a: &ArtifactName) -> Result<(), StoreError>;
}

/// What `open_or_rebuild` did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub replaced: usize,
    pub removed: usize,
    /// Shards that failed to decode and were moved to `.cache/quarantine/`.
    pub quarantined: Vec<(String, RelPath)>,
    /// The deadline passed before every shard was handled; the next open continues.
    pub deadline_hit: bool,
}

pub struct Cache {
    db: Database,
    report: SyncReport,
}

const BACKOFF_START: Duration = Duration::from_millis(5);
const BACKOFF_MAX: Duration = Duration::from_millis(50);

fn open_db(path: &Utf8PathBuf, deadline: Instant) -> Result<Database, StoreError> {
    let mut wait = BACKOFF_START;
    let mut recreated = false;
    loop {
        match Database::create(path.as_std_path()) {
            Ok(d) => return Ok(d),
            Err(DatabaseError::DatabaseAlreadyOpen) => {
                let now = Instant::now();
                if now >= deadline {
                    return Err(StoreError::Busy);
                }
                std::thread::sleep(wait.min(deadline - now));
                wait = (wait * 2).min(BACKOFF_MAX);
            }
            // A corrupt cache is deleted and rebuilt (FEAT-003 section 8).
            Err(DatabaseError::Storage(StorageError::Corrupted(_)))
            | Err(DatabaseError::UpgradeRequired(_))
                if !recreated =>
            {
                recreated = true;
                std::fs::remove_file(path).map_err(|e| StoreError::io(path, e))?;
            }
            Err(e) => return Err(db(e)),
        }
    }
}

impl Cache {
    /// R-6. Open `.cache/index.redb` (creating it, or recreating it when its schema differs),
    /// then bring it in line with the shards: every shard is hashed (SHA-256) and compared with
    /// `shard_hashes`; changed and new shards are decoded and passed to
    /// [`Restitch::replace_fragment`], deleted ones to [`Restitch::remove_artifact`]; the hashes
    /// are written and everything commits once. When `deadline` passes mid-way the work done so
    /// far is committed and the next open continues. A busy database is retried with backoff
    /// until `deadline`, then [`StoreError::Busy`].
    pub fn open_or_rebuild(
        dir: &ProjectDir,
        stitch: &dyn Restitch,
        deadline: Instant,
    ) -> Result<Cache, StoreError> {
        let cache_dir = dir.cache_dir();
        std::fs::create_dir_all(&cache_dir).map_err(|e| StoreError::io(&cache_dir, e))?;
        let path = cache_dir.join("index.redb");

        let mut database = open_db(&path, deadline)?;
        if !schema_current(&database)? {
            drop(database);
            std::fs::remove_file(&path).map_err(|e| StoreError::io(&path, e))?;
            database = open_db(&path, deadline)?;
            schema_current(&database)?;
        }
        let mut cache = Cache {
            db: database,
            report: SyncReport::default(),
        };
        // An interrupted publish is finished before the shards are compared with the hashes.
        cache.replay_journal(dir)?;
        cache.report = cache.sync(dir, stitch, deadline)?;
        Ok(cache)
    }

    pub fn read(&self) -> Result<ReadTx<'_>, StoreError> {
        Ok(ReadTx {
            txn: self.db.begin_read().map_err(db)?,
            _cache: PhantomData,
        })
    }

    pub fn write(&self) -> Result<WriteTx<'_>, StoreError> {
        Ok(WriteTx {
            txn: self.db.begin_write().map_err(db)?,
            _cache: PhantomData,
        })
    }

    /// Finish an interrupted publish (FEAT-047, ADR-0014): for each journal entry, rename the
    /// temp file onto the final name when the temp file still exists (an empty temp name removes
    /// the final file), fsync, then clear the journal. Idempotent. Returns the entries handled.
    pub fn replay_journal(&self, dir: &ProjectDir) -> Result<usize, StoreError> {
        let entries = self.read()?.publish_journal()?;
        if entries.is_empty() {
            return Ok(0);
        }
        #[cfg_attr(not(feature = "failpoints"), allow(unused_variables))]
        for (i, (tmp, fin)) in entries.iter().enumerate() {
            let to = dir.root().join(fin);
            if tmp.is_empty() {
                match std::fs::remove_file(&to) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(StoreError::io(&to, e)),
                }
            } else {
                let from = dir.root().join(tmp);
                if from.exists() {
                    std::fs::File::open(&from)
                        .and_then(|f| f.sync_all())
                        .map_err(|e| StoreError::io(&from, e))?;
                    std::fs::rename(&from, &to).map_err(|e| StoreError::io(&from, e))?;
                }
            }
            if let Some(parent) = to.parent()
                && parent.is_dir()
            {
                std::fs::File::open(parent)
                    .and_then(|f| f.sync_all())
                    .map_err(|e| StoreError::io(parent, e))?;
            }
            #[cfg(feature = "failpoints")]
            if i == 0 {
                crate::failpoint::hit("mid_renames");
            }
        }
        #[cfg(feature = "failpoints")]
        crate::failpoint::hit("before_journal_clear");
        let mut tx = self.write()?;
        tx.set_publish_journal(&[])?;
        tx.commit()?;
        Ok(entries.len())
    }

    /// What the last `open_or_rebuild` did.
    pub fn report(&self) -> &SyncReport {
        &self.report
    }

    fn sync(
        &self,
        dir: &ProjectDir,
        stitch: &dyn Restitch,
        deadline: Instant,
    ) -> Result<SyncReport, StoreError> {
        let mut report = SyncReport::default();
        let on_disk = dir.list_shards()?;
        let mut tx = self.write()?;
        let mut stale: BTreeSet<(String, RelPath)> =
            tx.shard_hashes()?.into_iter().map(|(k, _)| k).collect();

        for (repo, path) in &on_disk {
            stale.remove(&(repo.clone(), path.clone()));
            if Instant::now() >= deadline {
                report.deadline_hit = true;
                break;
            }
            let file = dir.shard_path(repo, path);
            let bytes = match std::fs::read(&file) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(StoreError::io(&file, e)),
            };
            let hash: [u8; 32] = Sha256::digest(&bytes).into();
            if tx.shard_hash(repo, path)? == Some(hash) {
                continue;
            }
            match crate::shard::decode(&bytes) {
                Ok(fragment) => {
                    stitch.replace_fragment(&mut tx, fragment)?;
                    tx.put_shard_hash(repo, path, &hash)?;
                    tx.put_index_errors(repo, path, &[])?;
                    report.replaced += 1;
                }
                Err(e) => {
                    quarantine(dir, repo, path)?;
                    // Whatever this shard contributed before is gone with it.
                    if tx.shard_hash(repo, path)?.is_some() {
                        stitch.remove_artifact(
                            &mut tx,
                            &ArtifactName {
                                repo: repo.clone(),
                                path: path.clone(),
                            },
                        )?;
                        tx.remove_shard_hash(repo, path)?;
                    }
                    tx.put_index_errors(
                        repo,
                        path,
                        &[ErrorRow {
                            span: None,
                            message: e.to_string(),
                        }],
                    )?;
                    report.quarantined.push((repo.clone(), path.clone()));
                }
            }
        }

        if !report.deadline_hit {
            // Only meaningful when every on-disk shard was visited.
            for (repo, path) in stale {
                if Instant::now() >= deadline {
                    report.deadline_hit = true;
                    break;
                }
                stitch.remove_artifact(
                    &mut tx,
                    &ArtifactName {
                        repo: repo.clone(),
                        path: path.clone(),
                    },
                )?;
                tx.remove_shard_hash(&repo, &path)?;
                tx.put_index_errors(&repo, &path, &[])?;
                report.removed += 1;
            }
        }
        tx.commit()?;
        Ok(report)
    }
}

/// Create the tables and check `meta.cache_schema`. `false`: written by another schema.
fn schema_current(database: &Database) -> Result<bool, StoreError> {
    let txn = database.begin_write().map_err(db)?;
    let mut tx = WriteTx {
        txn,
        _cache: PhantomData,
    };
    for t in TableId::ALL {
        tx.txn.open_table(t.def()).map_err(db)?;
    }
    let ok = match tx.meta("cache_schema")? {
        None => {
            tx.set_meta("cache_schema", &CACHE_SCHEMA.to_le_bytes())?;
            true
        }
        Some(v) => v == CACHE_SCHEMA.to_le_bytes(),
    };
    if ok {
        tx.commit()?;
    }
    Ok(ok)
}

/// Moves an undecodable shard to `.cache/quarantine/<repo>/<path>`.
fn quarantine(dir: &ProjectDir, repo: &str, path: &RelPath) -> Result<(), StoreError> {
    let from = dir.shard_path(repo, path);
    let to = dir.cache_dir().join("quarantine").join(repo).join(format!(
        "{}{}",
        path.as_str(),
        crate::shard::SUFFIX
    ));
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| StoreError::io(parent, e))?;
    }
    std::fs::rename(&from, &to).map_err(|e| StoreError::io(&from, e))
}
