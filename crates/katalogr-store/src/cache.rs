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
pub const CACHE_SCHEMA: u32 = 5;

/// `meta` key: the watermark the stored rank reflects.
const RANK_WATERMARK_META: &str = "rank_watermark";

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
    /// For an inferred link: how many targets its reference resolved to. `None` on rows
    /// written before this field existed, and on links that are not inferred.
    pub fanout: Option<u32>,
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

/// Number of searchable fields per document (see `katalogr_model::Field`).
pub const LEXICON_FIELDS: usize = 4;

/// One posting: a term occurs `tf` times in `field` of `symbol`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Posting {
    pub field: u8,
    pub symbol: Id,
    pub tf: u32,
}

/// Document count and per-field total lengths, for BM25.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct LexiconStats {
    pub docs: u64,
    pub total_len: [u64; LEXICON_FIELDS],
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

fn ext_context_key(repo: &str, extension: &str) -> Vec<u8> {
    let mut k = Vec::new();
    lp(&mut k, repo);
    k.extend_from_slice(extension.as_bytes());
    k
}

/// Terms are alphanumeric, so a NUL ends the term; `term NUL` is an exact-term prefix.
fn posting_prefix(term: &str) -> Vec<u8> {
    let mut k = term.as_bytes().to_vec();
    k.push(0);
    k
}

fn posting_key(term: &str, field: u8, symbol: Id) -> Vec<u8> {
    let mut k = posting_prefix(term);
    k.push(field);
    k.extend_from_slice(&symbol.0);
    k
}

const STATS_KEY: &[u8] = b"stats";

fn encode_stats(s: &LexiconStats) -> Vec<u8> {
    let mut v = s.docs.to_le_bytes().to_vec();
    for t in s.total_len {
        v.extend_from_slice(&t.to_le_bytes());
    }
    v
}

fn decode_stats(v: &[u8]) -> Result<LexiconStats, StoreError> {
    let bad = || StoreError::Row("bad lexicon stats".into());
    if v.len() != 8 * (1 + LEXICON_FIELDS) {
        return Err(bad());
    }
    let at = |i: usize| u64::from_le_bytes(v[i * 8..i * 8 + 8].try_into().expect("8 bytes"));
    let mut total_len = [0u64; LEXICON_FIELDS];
    for (i, t) in total_len.iter_mut().enumerate() {
        *t = at(i + 1);
    }
    Ok(LexiconStats {
        docs: at(0),
        total_len,
    })
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
/// The SHA-256 and the bytes of an extension context.
pub type ExtContextRow = ([u8; 32], Vec<u8>);
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
    ExtContext,
    Postings,
    DocLen,
    LexiconStats,
    Rank,
}

impl TableId {
    const ALL: [TableId; 21] = [
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
        TableId::ExtContext,
        TableId::Postings,
        TableId::DocLen,
        TableId::LexiconStats,
        TableId::Rank,
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
            TableId::ExtContext => "ext_context",
            TableId::Postings => "postings",
            TableId::DocLen => "doc_len",
            TableId::LexiconStats => "lexicon_stats",
            TableId::Rank => "rank",
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
            Some(v) => Ok(Some(split_hash_value(&v)?.0)),
            None => Ok(None),
        }
    }

    /// The `(size, mtime)` of the shard file when its hash was last recorded; `None` when no
    /// hash is stored or it was stored without them.
    fn shard_stat(&self, repo: &str, path: &RelPath) -> Result<Option<ShardStat>, StoreError> {
        match self.raw_get(TableId::ShardHashes, &path_key(repo, path))? {
            Some(v) => Ok(split_hash_value(&v)?.1),
            None => Ok(None),
        }
    }

    /// Every stored shard hash, sorted by `(repo, path)` key bytes.
    fn shard_hashes(&self) -> Result<Vec<(ShardKey, [u8; 32])>, StoreError> {
        let mut out = Vec::new();
        for (k, v) in self.raw_scan(TableId::ShardHashes, &[])? {
            out.push((decode_path_key(&k)?, split_hash_value(&v)?.0));
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

    /// Number of links arriving at each target, all kinds, from one scan of `links_in`. A symbol
    /// with no incoming link is absent.
    fn in_degrees(&self) -> Result<BTreeMap<Id, u64>, StoreError> {
        let mut out: BTreeMap<Id, u64> = BTreeMap::new();
        for (key, _) in self.raw_scan(TableId::LinksIn, &[])? {
            let target: [u8; 16] = key
                .get(..16)
                .and_then(|b| b.try_into().ok())
                .ok_or_else(|| StoreError::Row("malformed key".into()))?;
            *out.entry(Id(target)).or_default() += 1;
        }
        Ok(out)
    }

    /// Every link row, sorted by `(from, kind, to)` key bytes.
    fn links_all(&self) -> Result<Vec<(LinkKey, LinkRow)>, StoreError> {
        self.raw_scan(TableId::LinksOut, &[])?
            .into_iter()
            .map(|(k, v)| Ok((decode_link_key(&k)?, de(&v)?)))
            .collect()
    }

    /// The postings of one exact term, sorted by `(field, symbol)`.
    fn postings(&self, term: &str) -> Result<Vec<Posting>, StoreError> {
        let prefix = posting_prefix(term);
        self.raw_scan(TableId::Postings, &prefix)?
            .into_iter()
            .map(|(k, v)| {
                let bad = || StoreError::Row("malformed posting".into());
                let rest = &k[prefix.len()..];
                if rest.len() != 17 {
                    return Err(bad());
                }
                Ok(Posting {
                    field: rest[0],
                    symbol: Id(rest[1..].try_into().map_err(|_| bad())?),
                    tf: u32::from_le_bytes(v.as_slice().try_into().map_err(|_| bad())?),
                })
            })
            .collect()
    }

    /// The postings of the indexed terms that `prefix` strictly prefixes (the exact term
    /// `prefix` is excluded; use [`Reader::postings`] for it), at most `limit` terms: those with
    /// the highest total term frequency first, ties by term. Each term's postings are sorted by
    /// `(field, symbol)`; the returned terms are in that selection order.
    fn postings_prefix(
        &self,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<(String, Vec<Posting>)>, StoreError> {
        let bad = || StoreError::Row("malformed posting".into());
        let mut by_term: BTreeMap<String, (u64, Vec<Posting>)> = BTreeMap::new();
        for (k, v) in self.raw_scan(TableId::Postings, prefix.as_bytes())? {
            let nul = k.iter().position(|b| *b == 0).ok_or_else(bad)?;
            let rest = &k[nul + 1..];
            if rest.len() != 17 {
                return Err(bad());
            }
            if nul == prefix.len() {
                continue;
            }
            let term = String::from_utf8(k[..nul].to_vec()).map_err(|_| bad())?;
            let tf = u32::from_le_bytes(v.as_slice().try_into().map_err(|_| bad())?);
            let entry = by_term.entry(term).or_default();
            entry.0 += u64::from(tf);
            entry.1.push(Posting {
                field: rest[0],
                symbol: Id(rest[1..].try_into().map_err(|_| bad())?),
                tf,
            });
        }
        let mut terms: Vec<(String, (u64, Vec<Posting>))> = by_term.into_iter().collect();
        terms.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(&b.0)));
        terms.truncate(limit);
        Ok(terms.into_iter().map(|(t, (_, p))| (t, p)).collect())
    }

    /// Per-field token counts of one indexed symbol.
    fn doc_len(&self, id: Id) -> Result<Option<[u32; LEXICON_FIELDS]>, StoreError> {
        self.raw_get(TableId::DocLen, &id.0)?
            .map(|v| decode_doc_len(&v))
            .transpose()
    }

    fn lexicon_stats(&self) -> Result<LexiconStats, StoreError> {
        match self.raw_get(TableId::LexiconStats, STATS_KEY)? {
            Some(v) => decode_stats(&v),
            None => Ok(LexiconStats::default()),
        }
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

    /// Whether the stored rank reflects `context`'s current watermark.
    fn rank_is_current(&self, context: &str) -> Result<bool, StoreError> {
        let Some(v) = self.meta(RANK_WATERMARK_META)? else {
            return Ok(false);
        };
        Ok(u64_value(v)? == self.watermark(context)?)
    }

    /// The stored rank of every symbol (FEAT-057), or `None` when it does not reflect
    /// `context`'s current watermark. The values are bit-exact what [`crate::pagerank`] computed.
    fn cached_rank(&self, context: &str) -> Result<Option<BTreeMap<Id, f64>>, StoreError> {
        if !self.rank_is_current(context)? {
            return Ok(None);
        }
        let mut out = BTreeMap::new();
        for (k, v) in self.raw_scan(TableId::Rank, &[])? {
            let bad = || StoreError::Row("bad rank row".into());
            let id = Id(k.try_into().map_err(|_| bad())?);
            let bits = u64::from_le_bytes(v.try_into().map_err(|_| bad())?);
            out.insert(id, f64::from_bits(bits));
        }
        Ok(Some(out))
    }

    /// The renames of a publish that has committed but not yet finished.
    fn publish_journal(&self) -> Result<Vec<JournalEntry>, StoreError> {
        Ok(self
            .raw_get(TableId::Journal, &[])?
            .map(|v| de(&v))
            .transpose()?
            .unwrap_or_default())
    }

    /// The extension context of `(repo, extension id)`: its SHA-256 and its bytes.
    fn ext_context(
        &self,
        repo: &str,
        extension: &str,
    ) -> Result<Option<ExtContextRow>, StoreError> {
        let Some(v) = self.raw_get(TableId::ExtContext, &ext_context_key(repo, extension))? else {
            return Ok(None);
        };
        if v.len() < 32 {
            return Err(StoreError::Row("bad ext_context row".into()));
        }
        let (h, bytes) = v.split_at(32);
        Ok(Some((h.try_into().expect("32 bytes"), bytes.to_vec())))
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

fn encode_doc_len(l: &[u32; LEXICON_FIELDS]) -> Vec<u8> {
    l.iter().flat_map(|n| n.to_le_bytes()).collect()
}

fn decode_doc_len(v: &[u8]) -> Result<[u32; LEXICON_FIELDS], StoreError> {
    if v.len() != 4 * LEXICON_FIELDS {
        return Err(StoreError::Row("bad doc_len row".into()));
    }
    let mut out = [0u32; LEXICON_FIELDS];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u32::from_le_bytes(v[i * 4..i * 4 + 4].try_into().expect("4 bytes"));
    }
    Ok(out)
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
    touched: RankTouched,
}

/// What a write transaction touched of the rank's inputs (FEAT-057): each link with its rank
/// weight when first touched (`None`: absent), each symbol with whether it existed.
#[derive(Default)]
struct RankTouched {
    links: BTreeMap<LinkKey, Option<f64>>,
    symbols: BTreeMap<Id, bool>,
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

    /// Like [`WriteTx::put_shard_hash`], also recording the shard file's `(size, mtime)` so a
    /// later open can skip hashing it while they are unchanged.
    pub fn put_shard_hash_stat(
        &mut self,
        repo: &str,
        path: &RelPath,
        hash: &[u8; 32],
        stat: ShardStat,
    ) -> Result<(), StoreError> {
        let mut v = Vec::with_capacity(52);
        v.extend_from_slice(hash);
        v.extend_from_slice(&stat.size.to_le_bytes());
        v.extend_from_slice(&stat.mtime_secs.to_le_bytes());
        v.extend_from_slice(&stat.mtime_nanos.to_le_bytes());
        self.put(TableId::ShardHashes, &path_key(repo, path), &v)
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
        self.touch_symbol(id)?;
        self.put(TableId::Symbols, &id.0, &ser(row)?)
    }

    pub fn remove_symbol(&mut self, id: Id) -> Result<(), StoreError> {
        self.touch_symbol(id)?;
        self.del(TableId::Symbols, &id.0)
    }

    /// Writes `links_out` and the matching `links_in` entry.
    pub fn put_link(&mut self, key: &LinkKey, row: &LinkRow) -> Result<(), StoreError> {
        self.touch_link(key)?;
        self.put(TableId::LinksOut, &link_out_key(key), &ser(row)?)?;
        self.put(TableId::LinksIn, &link_in_key(key), &[])
    }

    /// Removes `links_out` and the matching `links_in` entry.
    pub fn remove_link(&mut self, key: &LinkKey) -> Result<(), StoreError> {
        self.touch_link(key)?;
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

    /// Stores the context of `(repo, extension id)` with its SHA-256.
    pub fn put_ext_context(
        &mut self,
        repo: &str,
        extension: &str,
        hash: &[u8; 32],
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        let mut v = hash.to_vec();
        v.extend_from_slice(bytes);
        self.put(TableId::ExtContext, &ext_context_key(repo, extension), &v)
    }

    /// Index one symbol: its postings `(term, field, tf)` and per-field lengths. Adds the
    /// lengths to the lexicon statistics. Call [`WriteTx::remove_doc_postings`] first when the
    /// symbol is already indexed.
    pub fn put_doc_postings(
        &mut self,
        id: Id,
        lens: [u32; LEXICON_FIELDS],
        terms: &[(String, u8, u32)],
    ) -> Result<(), StoreError> {
        for (term, field, tf) in terms {
            self.put(
                TableId::Postings,
                &posting_key(term, *field, id),
                &tf.to_le_bytes(),
            )?;
        }
        self.put(TableId::DocLen, &id.0, &encode_doc_len(&lens))?;
        let mut stats = self.lexicon_stats()?;
        stats.docs += 1;
        for (t, l) in stats.total_len.iter_mut().zip(lens) {
            *t += u64::from(l);
        }
        self.put(TableId::LexiconStats, STATS_KEY, &encode_stats(&stats))
    }

    /// Remove the postings `(term, field)` of one symbol and its length row, subtracting the
    /// lengths from the lexicon statistics. A symbol that is not indexed changes nothing.
    pub fn remove_doc_postings(
        &mut self,
        id: Id,
        terms: &[(String, u8)],
    ) -> Result<(), StoreError> {
        let Some(lens) = self.doc_len(id)? else {
            return Ok(());
        };
        for (term, field) in terms {
            self.del(TableId::Postings, &posting_key(term, *field, id))?;
        }
        self.del(TableId::DocLen, &id.0)?;
        let mut stats = self.lexicon_stats()?;
        stats.docs = stats.docs.saturating_sub(1);
        for (t, l) in stats.total_len.iter_mut().zip(lens) {
            *t = t.saturating_sub(u64::from(l));
        }
        self.put(TableId::LexiconStats, STATS_KEY, &encode_stats(&stats))
    }

    /// Remember the state a symbol had when this transaction first touched it.
    fn touch_symbol(&mut self, id: Id) -> Result<(), StoreError> {
        if !self.touched.symbols.contains_key(&id) {
            let existed = self.raw_get(TableId::Symbols, &id.0)?.is_some();
            self.touched.symbols.insert(id, existed);
        }
        Ok(())
    }

    /// Remember the rank weight a link had when this transaction first touched it.
    fn touch_link(&mut self, key: &LinkKey) -> Result<(), StoreError> {
        if !self.touched.links.contains_key(key) {
            let weight = self.link(key)?.map(|r| crate::rank::row_weight(&r));
            self.touched.links.insert(key.clone(), weight);
        }
        Ok(())
    }

    /// Whether this transaction changed anything the rank depends on: the set of symbols, or
    /// the rank weight of any link row. Writes that restore what was there cancel out.
    pub fn rank_inputs_changed(&self) -> Result<bool, StoreError> {
        for (id, existed) in &self.touched.symbols {
            if self.raw_get(TableId::Symbols, &id.0)?.is_some() != *existed {
                return Ok(true);
            }
        }
        for (key, was) in &self.touched.links {
            let now = self.link(key)?.map(|r| crate::rank::row_weight(&r));
            if now != *was {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Replace the stored rank with `scores`, recorded as reflecting `watermark`.
    pub fn store_rank(&mut self, scores: &[(Id, f64)], watermark: u64) -> Result<(), StoreError> {
        self.txn.delete_table(TableId::Rank.def()).map_err(db)?;
        {
            let mut table = self.txn.open_table(TableId::Rank.def()).map_err(db)?;
            for (id, score) in scores {
                table
                    .insert(id.0.as_slice(), score.to_bits().to_le_bytes().as_slice())
                    .map_err(db)?;
            }
        }
        self.set_rank_watermark(watermark)
    }

    /// Record that the stored rank reflects `watermark`.
    pub fn set_rank_watermark(&mut self, watermark: u64) -> Result<(), StoreError> {
        self.set_meta(RANK_WATERMARK_META, &watermark.to_le_bytes())
    }

    /// Commits. A transaction that changed the rank's inputs also drops the stored rank's
    /// watermark, so no reader can take a rank that does not match the graph it reads.
    pub fn commit(mut self) -> Result<(), StoreError> {
        if self.rank_inputs_changed()? {
            self.del(TableId::Meta, RANK_WATERMARK_META.as_bytes())?;
        }
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

/// Size and modification time of a shard file, recorded next to its hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShardStat {
    pub size: u64,
    pub mtime_secs: u64,
    pub mtime_nanos: u32,
}

impl ShardStat {
    fn of(meta: &std::fs::Metadata) -> Option<ShardStat> {
        let d = meta
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        Some(ShardStat {
            size: meta.len(),
            mtime_secs: d.as_secs(),
            mtime_nanos: d.subsec_nanos(),
        })
    }
}

/// A `shard_hashes` value: the 32-byte hash, optionally followed by size (u64), mtime seconds
/// (u64) and mtime nanoseconds (u32), all little-endian.
fn split_hash_value(v: &[u8]) -> Result<([u8; 32], Option<ShardStat>), StoreError> {
    let bad = || StoreError::Row("bad shard hash".into());
    let hash: [u8; 32] = v.get(..32).ok_or_else(bad)?.try_into().map_err(|_| bad())?;
    match v.len() {
        32 => Ok((hash, None)),
        52 => {
            let u64_at = |i: usize| u64::from_le_bytes(v[i..i + 8].try_into().expect("8 bytes"));
            Ok((
                hash,
                Some(ShardStat {
                    size: u64_at(32),
                    mtime_secs: u64_at(40),
                    mtime_nanos: u32::from_le_bytes(v[48..52].try_into().expect("4 bytes")),
                }),
            ))
        }
        _ => Err(bad()),
    }
}

/// What `open_or_rebuild` did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub replaced: usize,
    pub removed: usize,
    /// Shards whose bytes were read and hashed (those with an unchanged size and mtime are not).
    pub hashed: usize,
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
            touched: RankTouched::default(),
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
            // The stat is taken before the read, so a write in between is seen at the next open.
            let stat = match std::fs::metadata(&file) {
                Ok(m) => ShardStat::of(&m),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(StoreError::io(&file, e)),
            };
            let stored = tx.shard_hash(repo, path)?;
            if stored.is_some() && stat.is_some() && tx.shard_stat(repo, path)? == stat {
                continue;
            }
            let bytes = match std::fs::read(&file) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(StoreError::io(&file, e)),
            };
            report.hashed += 1;
            let hash: [u8; 32] = Sha256::digest(&bytes).into();
            let record = |tx: &mut WriteTx| match stat {
                Some(st) => tx.put_shard_hash_stat(repo, path, &hash, st),
                None => tx.put_shard_hash(repo, path, &hash),
            };
            if stored == Some(hash) {
                if tx.shard_stat(repo, path)? != stat {
                    record(&mut tx)?;
                }
                continue;
            }
            match crate::shard::decode(&bytes) {
                Ok(fragment) => {
                    stitch.replace_fragment(&mut tx, fragment)?;
                    record(&mut tx)?;
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
        touched: RankTouched::default(),
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
