//! Local redb cache rebuilt from the shards (FEAT-003 section 3 to 8, FEAT-001 section 5).
//!
//! Every table is `&[u8] -> &[u8]`. Keys are built by the functions below (big-endian, length
//! prefixed strings, so ranges by prefix are exact). Values are compact `serde_json` bytes of the
//! row types, which are deterministic because every map in them is ordered.

use crate::error::StoreError;
use crate::fan;
use crate::kindex::ProjectDir;
use camino::Utf8PathBuf;
use katalogr_model::{
    ArtifactName, Confidence, ContractName, ContractRole, Export, ExportKey, Fact, Fragment, Id,
    LinkKind, OutRef, Provenance, RelPath, Span, SymbolName,
};
use redb::{
    Database, DatabaseError, ReadTransaction, ReadableDatabase, ReadableTable, StorageError,
    TableDefinition, WriteTransaction,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::marker::PhantomData;
use std::ops::Bound;
use std::time::{Duration, Instant};

/// Stored in `meta`; a different number rebuilds the cache from the shards.
pub const CACHE_SCHEMA: u32 = 10;

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

/// What a structural comparison needs of a symbol, read without building its whole row: the
/// name of its last segment (unescaped, without an ordinal) and its facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolBrief {
    pub leaf: String,
    pub facts: BTreeMap<String, Fact>,
}

/// What path scoring needs of a link row, read without decoding its spans and provenance:
/// the confidence of the first entry and the row weight (see [`crate::rank::row_weight`]).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LinkBrief {
    pub confidence: Confidence,
    pub weight: f64,
}

#[derive(Deserialize)]
struct RawLinkEntry {
    origin: LinkOrigin,
    confidence: Confidence,
    fanout: Option<u32>,
}

#[derive(Deserialize)]
struct RawLinkRow {
    entries: Vec<RawLinkEntry>,
}

#[derive(Deserialize)]
struct RawBrief<'a> {
    #[serde(borrow)]
    name: std::borrow::Cow<'a, str>,
    facts: BTreeMap<String, Fact>,
}

impl RawBrief<'_> {
    fn brief(self) -> SymbolBrief {
        // `<artifact>#<kind>:<name>/…[~<ordinal>]`: '/' and '~' inside a name are escaped.
        let body = self.name.split_once('#').map_or("", |(_, b)| b);
        let last = body.rsplit('/').next().unwrap_or("");
        let last = match last.rsplit_once('~') {
            Some((l, n)) if !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()) => l,
            _ => last,
        };
        let raw = last.split_once(':').map_or("", |(_, n)| n);
        SymbolBrief {
            leaf: percent_decode(raw),
            facts: self.facts,
        }
    }
}

fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A contract node: how many bindings of each role name it. The row exists while the sum is
/// positive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractRow {
    pub name: ContractName,
    pub providers: u32,
    pub consumers: u32,
    pub declarers: u32,
}

impl ContractRow {
    pub fn is_empty(&self) -> bool {
        self.providers + self.consumers + self.declarers == 0
    }

    /// Adjust the count of `role` by `delta` (saturating at 0).
    pub fn adjust(&mut self, role: ContractRole, delta: i32) {
        let n = match role {
            ContractRole::Provides => &mut self.providers,
            ContractRole::Consumes => &mut self.consumers,
            ContractRole::Declares => &mut self.declarers,
        };
        *n = n.saturating_add_signed(delta);
    }
}

/// One binding of an artifact as the cache remembers it, so the artifact's old bindings can be
/// withdrawn without its shard.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BindingRow {
    pub symbol: Id,
    pub contract: Id,
    pub role: ContractRole,
}

/// Where a link came from: a link inside the file, or the resolution of the file's n-th ref.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LinkOrigin {
    Intra,
    Ref(u32),
    /// A link written by the `index`-th (bound) reference of `artifact` (ADR-0022): it may start
    /// at a symbol of another artifact.
    Bound {
        artifact: Id,
        index: u32,
    },
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
    /// What produced the link when it was not an extractor, for example a profile rule
    /// (`profile:<name>#<rule id>`). `None` on rows written before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub produced_by: Option<String>,
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

/// One stored multi-target resolution (FEAT-077): the referring node, the link kind, and the
/// export key whose targets (`exports(key)`) the reference resolved to. Ordered like its
/// encoded key: `from`, then `kind`, then `key`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FanKey {
    pub from: Id,
    pub kind: LinkKind,
    pub key: ExportKey,
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

pub(crate) fn lp(out: &mut Vec<u8>, s: &str) {
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

/// Scan prefix of the `refs_by_key` rows of one export key: `lp(namespace) || lp(key)`. The
/// length prefixes keep `fn:a` from matching `fn:ab`.
fn ref_by_key_prefix(k: &ExportKey) -> Vec<u8> {
    let mut out = Vec::new();
    lp(&mut out, &k.namespace);
    lp(&mut out, &k.key);
    out
}

/// `refs_by_key` row key: the prefix, then the 16-byte artifact id and the big-endian index.
fn ref_by_key_key(k: &ExportKey, r: RefId) -> Vec<u8> {
    let mut out = ref_by_key_prefix(k);
    out.extend_from_slice(&ref_key(r));
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

pub(crate) fn split_lp(b: &[u8]) -> Result<(&str, &[u8]), StoreError> {
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

pub(crate) fn link_out_key(k: &LinkKey) -> Vec<u8> {
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

/// `stored` keys arriving at `to` plus the keys of `fans` (rows that reach `to`), sorted by
/// `links_in` key bytes and deduplicated.
fn merge_links_in(stored: Vec<LinkKey>, to: Id, fans: Vec<FanKey>) -> Vec<LinkKey> {
    let mut all: Vec<(Vec<u8>, LinkKey)> = stored
        .into_iter()
        .chain(fans.into_iter().map(|f| LinkKey {
            from: f.from,
            kind: f.kind,
            to,
        }))
        .map(|k| (link_in_key(&k), k))
        .collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    all.dedup_by(|a, b| a.0 == b.0);
    all.into_iter().map(|(_, k)| k).collect()
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
/// A stored shard hash with the `(size, mtime)` recorded beside it.
pub type ShardHashRow = (ShardKey, [u8; 32], Option<ShardStat>);

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
    Contracts,
    BindingsByArtifact,
    FanOut,
    FanIn,
    ExportsByTarget,
}

impl TableId {
    const ALL: [TableId; 26] = [
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
        TableId::Contracts,
        TableId::BindingsByArtifact,
        TableId::FanOut,
        TableId::FanIn,
        TableId::ExportsByTarget,
    ];

    /// The graph tables, in the order `dump_graph` reports them.
    const GRAPH: [TableId; 13] = [
        TableId::Artifacts,
        TableId::Symbols,
        TableId::LinksOut,
        TableId::LinksIn,
        TableId::Exports,
        TableId::RefsByKey,
        TableId::RefsUnresolved,
        TableId::IndexErrors,
        TableId::Contracts,
        TableId::BindingsByArtifact,
        TableId::FanOut,
        TableId::FanIn,
        TableId::ExportsByTarget,
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
            TableId::Contracts => "contracts",
            TableId::BindingsByArtifact => "bindings_by_artifact",
            TableId::FanOut => "fan_out",
            TableId::FanIn => "fan_in",
            TableId::ExportsByTarget => "exports_by_target",
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

/// The keys of [`scan_in`], without copying the values.
fn scan_keys_in<T: ReadableTable<&'static [u8], &'static [u8]>>(
    t: &T,
    prefix: &[u8],
) -> Result<Vec<Vec<u8>>, StoreError> {
    let hi = prefix_end(prefix);
    let hi_ref = match &hi {
        Some(e) => Bound::Excluded(e.as_slice()),
        None => Bound::Unbounded,
    };
    let it = t
        .range::<&[u8]>((Bound::Included(prefix), hi_ref))
        .map_err(db)?;
    let mut out = Vec::new();
    for r in it {
        let (k, _) = r.map_err(db)?;
        out.push(k.value().to_vec());
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
    /// The keys of [`Reader::raw_scan`], without copying the values.
    #[doc(hidden)]
    fn raw_scan_keys(&self, t: TableId, prefix: &[u8]) -> Result<Vec<Vec<u8>>, StoreError> {
        Ok(self
            .raw_scan(t, prefix)?
            .into_iter()
            .map(|(k, _)| k)
            .collect())
    }
    /// [`Reader::raw_get`] for many keys, opening the table once.
    #[doc(hidden)]
    fn raw_get_many(
        &self,
        t: TableId,
        keys: &[Vec<u8>],
    ) -> Result<Vec<Option<Vec<u8>>>, StoreError> {
        keys.iter().map(|k| self.raw_get(t, k)).collect()
    }
    /// [`Reader::raw_scan_keys`] for many prefixes, opening the table once.
    #[doc(hidden)]
    fn raw_scan_keys_many(
        &self,
        t: TableId,
        prefixes: &[Vec<u8>],
    ) -> Result<Vec<Vec<Vec<u8>>>, StoreError> {
        prefixes.iter().map(|p| self.raw_scan_keys(t, p)).collect()
    }

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

    /// Every stored shard hash with the `(size, mtime)` recorded beside it, sorted by
    /// `(repo, path)` key bytes.
    fn shard_hash_rows(&self) -> Result<Vec<ShardHashRow>, StoreError> {
        let mut out = Vec::new();
        for (k, v) in self.raw_scan(TableId::ShardHashes, &[])? {
            let (hash, stat) = split_hash_value(&v)?;
            out.push((decode_path_key(&k)?, hash, stat));
        }
        Ok(out)
    }

    /// Files with at least one index-error row and the number of rows, sorted by key bytes.
    fn index_error_files(&self) -> Result<Vec<(ShardKey, usize)>, StoreError> {
        let mut out = Vec::new();
        for (k, v) in self.raw_scan(TableId::IndexErrors, &[])? {
            let rows: Vec<ErrorRow> = de(&v)?;
            if !rows.is_empty() {
                out.push((decode_path_key(&k)?, rows.len()));
            }
        }
        Ok(out)
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

    /// The stored `links_out` row of `key`, without fan rows (FEAT-077). The stitcher uses only
    /// the stored-only reads.
    fn stored_link(&self, key: &LinkKey) -> Result<Option<LinkRow>, StoreError> {
        self.raw_get(TableId::LinksOut, &link_out_key(key))?
            .map(|v| de(&v))
            .transpose()
    }

    /// Stored rows leaving `from` (optionally of one kind), sorted by key bytes; no fan rows.
    fn stored_links_out(
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

    /// [`Reader::symbol`] for many ids, in order; the reads share one table lookup.
    fn symbols_many(&self, ids: &[Id]) -> Result<Vec<Option<SymbolRow>>, StoreError> {
        let keys: Vec<Vec<u8>> = ids.iter().map(|i| i.0.to_vec()).collect();
        self.raw_get_many(TableId::Symbols, &keys)?
            .into_iter()
            .map(|v| v.map(|v| de(&v)).transpose())
            .collect()
    }

    /// The briefs of many symbols, in order (None for an id that is not a symbol). A large
    /// batch is decoded on a few threads; the result does not depend on how many.
    fn symbol_briefs_many(&self, ids: &[Id]) -> Result<Vec<Option<SymbolBrief>>, StoreError> {
        let keys: Vec<Vec<u8>> = ids.iter().map(|i| i.0.to_vec()).collect();
        let rows = self.raw_get_many(TableId::Symbols, &keys)?;
        let decode = |v: &Option<Vec<u8>>| -> Result<Option<SymbolBrief>, StoreError> {
            v.as_ref()
                .map(|v| {
                    serde_json::from_slice::<RawBrief>(v)
                        .map(RawBrief::brief)
                        .map_err(|e| StoreError::Row(e.to_string()))
                })
                .transpose()
        };
        const PARALLEL_FROM: usize = 20_000;
        let threads = std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(4);
        if rows.len() < PARALLEL_FROM || threads < 2 {
            return rows.iter().map(decode).collect();
        }
        let chunk = rows.len().div_ceil(threads);
        let parts: Vec<Result<Vec<Option<SymbolBrief>>, StoreError>> = std::thread::scope(|s| {
            let handles: Vec<_> = rows
                .chunks(chunk)
                .map(|c| s.spawn(|| c.iter().map(decode).collect::<Result<Vec<_>, _>>()))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("decode thread"))
                .collect()
        });
        let mut out = Vec::with_capacity(rows.len());
        for p in parts {
            out.extend(p?);
        }
        Ok(out)
    }

    /// The targets of `key` through a per-call cache, so each key's exports are decoded once.
    #[doc(hidden)]
    fn cached_targets<'c>(
        &self,
        cache: &'c mut HashMap<ExportKey, Vec<Id>>,
        key: &ExportKey,
    ) -> Result<&'c [Id], StoreError> {
        if !cache.contains_key(key) {
            let t = self.exports(key)?;
            cache.insert(key.clone(), t);
        }
        Ok(&cache[key])
    }

    /// The fan row of `key`, if stored.
    fn fan(&self, key: &FanKey) -> Result<Option<LinkRow>, StoreError> {
        self.raw_get(TableId::FanOut, &fan::fan_out_key(key))?
            .map(|v| de(&v))
            .transpose()
    }

    /// Fan rows leaving `from` (optionally of one kind), sorted by encoded key.
    fn fans_out(
        &self,
        from: Id,
        kind: Option<&LinkKind>,
    ) -> Result<Vec<(FanKey, LinkRow)>, StoreError> {
        self.raw_scan(TableId::FanOut, &fan::fan_out_prefix(from, kind))?
            .into_iter()
            .map(|(k, v)| Ok((fan::decode_fan_out_key(&k)?, de(&v)?)))
            .collect()
    }

    /// The keys of the fan rows leaving `from` with `kind`, without decoding their rows.
    #[doc(hidden)]
    fn fan_keys_out(&self, from: Id, kind: &LinkKind) -> Result<Vec<FanKey>, StoreError> {
        self.raw_scan_keys(TableId::FanOut, &fan::fan_out_prefix(from, Some(kind)))?
            .iter()
            .map(|k| fan::decode_fan_out_key(k))
            .collect()
    }

    /// Every fan row, sorted by encoded key.
    fn fans_all(&self) -> Result<Vec<(FanKey, LinkRow)>, StoreError> {
        self.raw_scan(TableId::FanOut, &[])?
            .into_iter()
            .map(|(k, v)| Ok((fan::decode_fan_out_key(&k)?, de(&v)?)))
            .collect()
    }

    /// The fan rows whose key is `key` (from `fan_in`), sorted by `(from, kind)` bytes.
    fn fans_in(&self, key: &ExportKey) -> Result<Vec<FanKey>, StoreError> {
        self.raw_scan_keys(TableId::FanIn, &fan::fan_in_prefix(key))?
            .iter()
            .map(|k| fan::decode_fan_in_key(k))
            .collect()
    }

    /// The export keys with two or more targets that include `target`, sorted by encoded key.
    fn fan_keys_of(&self, target: Id) -> Result<Vec<ExportKey>, StoreError> {
        self.raw_scan_keys(TableId::ExportsByTarget, &fan::by_target_prefix(target))?
            .iter()
            .map(|k| Ok(fan::decode_by_target_key(k)?.1))
            .collect()
    }

    /// The fan keys of `from` and `kind` that reach `to`, each with its row.
    #[doc(hidden)]
    fn fans_reaching(&self, key: &LinkKey) -> Result<Vec<(FanKey, LinkRow)>, StoreError> {
        let mut out = Vec::new();
        for fk in self.fan_keys_out(key.from, &key.kind)? {
            if self
                .raw_get(
                    TableId::ExportsByTarget,
                    &fan::by_target_key(key.to, &fk.key),
                )?
                .is_none()
            {
                continue;
            }
            if let Some(row) = self.fan(&fk)? {
                out.push((fk, row));
            }
        }
        Ok(out)
    }

    /// The fan rows that contribute to the expanded link `key`, as (export key, target
    /// count), sorted by encoded fan key. Empty for a link that has only stored entries.
    fn fan_sources(&self, key: &LinkKey) -> Result<Vec<(ExportKey, u32)>, StoreError> {
        let mut out = Vec::new();
        for fk in self.fan_keys_out(key.from, &key.kind)? {
            if self
                .raw_get(
                    TableId::ExportsByTarget,
                    &fan::by_target_key(key.to, &fk.key),
                )?
                .is_none()
            {
                continue;
            }
            let n = self.exports(&fk.key)?.len();
            out.push((fk.key, u32::try_from(n).unwrap_or(u32::MAX)));
        }
        Ok(out)
    }

    /// One link: the stored row and every fan row that reaches `key.to`, merged in origin
    /// order, each fan entry with a fan-out of its key's current target count.
    fn link(&self, key: &LinkKey) -> Result<Option<LinkRow>, StoreError> {
        let stored = self.stored_link(key)?;
        let fans = self.fans_reaching(key)?;
        if fans.is_empty() {
            return Ok(stored);
        }
        let mut entries = stored.map(|r| r.entries).unwrap_or_default();
        for (fk, row) in &fans {
            let n = self.exports(&fk.key)?.len();
            entries.extend(fan::expanded_entries(row, n));
        }
        entries.sort_by_key(|e| e.origin);
        Ok(Some(LinkRow { entries }))
    }

    /// Links leaving `from`, optionally only of one kind: stored rows and expanded fan rows.
    fn links_out(
        &self,
        from: Id,
        kind: Option<&LinkKind>,
    ) -> Result<Vec<(LinkKey, LinkRow)>, StoreError> {
        let stored = self.stored_links_out(from, kind)?;
        let fans = self.fans_out(from, kind)?;
        if fans.is_empty() {
            return Ok(stored);
        }
        let mut ex = fan::Expansion::default();
        for (k, r) in stored {
            ex.add_stored(k, r);
        }
        let mut cache = HashMap::new();
        for (fk, row) in &fans {
            let targets = self.cached_targets(&mut cache, &fk.key)?;
            ex.add_fan(fk, row, targets);
        }
        Ok(ex.finish())
    }

    /// Every link row, sorted by `(from, kind, to)` key bytes: stored rows and expanded fan rows.
    fn links_all(&self) -> Result<Vec<(LinkKey, LinkRow)>, StoreError> {
        let stored: Vec<(LinkKey, LinkRow)> = self
            .raw_scan(TableId::LinksOut, &[])?
            .into_iter()
            .map(|(k, v)| Ok((decode_link_key(&k)?, de(&v)?)))
            .collect::<Result<_, StoreError>>()?;
        let fans = self.fans_all()?;
        if fans.is_empty() {
            return Ok(stored);
        }
        let mut ex = fan::Expansion::default();
        for (k, r) in stored {
            ex.add_stored(k, r);
        }
        let mut cache = HashMap::new();
        for (fk, row) in &fans {
            let targets = self.cached_targets(&mut cache, &fk.key)?;
            ex.add_fan(fk, row, targets);
        }
        Ok(ex.finish())
    }

    /// Keys of the links arriving at `to` (each is a `from -> to` link, reported in link
    /// direction), in `links_in` key order: stored keys and the keys of fan rows that reach `to`.
    fn links_in(&self, to: Id) -> Result<Vec<LinkKey>, StoreError> {
        let stored = self.stored_links_in(to)?;
        let keys = self.fan_keys_of(to)?;
        if keys.is_empty() {
            return Ok(stored);
        }
        let mut fans = Vec::new();
        for k in &keys {
            fans.extend(self.fans_in(k)?);
        }
        Ok(merge_links_in(stored, to, fans))
    }

    /// The keys of the links leaving `from` with `kind`, without decoding their rows.
    fn link_keys_out(&self, from: Id, kind: &LinkKind) -> Result<Vec<LinkKey>, StoreError> {
        let stored: Vec<LinkKey> = self
            .raw_scan_keys(TableId::LinksOut, &link_prefix(from, kind))?
            .iter()
            .map(|k| decode_link_key(k))
            .collect::<Result<_, _>>()?;
        let fans = self.fan_keys_out(from, kind)?;
        let mut cache = HashMap::new();
        self.merge_fan_targets(stored, from, kind, &fans, &mut cache)
    }

    #[doc(hidden)]
    fn merge_fan_targets(
        &self,
        mut keys: Vec<LinkKey>,
        from: Id,
        kind: &LinkKind,
        fans: &[FanKey],
        cache: &mut HashMap<ExportKey, Vec<Id>>,
    ) -> Result<Vec<LinkKey>, StoreError> {
        if fans.is_empty() {
            return Ok(keys);
        }
        for fk in fans {
            for t in self.cached_targets(cache, &fk.key)? {
                keys.push(LinkKey {
                    from,
                    kind: kind.clone(),
                    to: *t,
                });
            }
        }
        keys.sort_by(|a, b| a.to.cmp(&b.to));
        keys.dedup();
        Ok(keys)
    }

    /// [`Reader::link_keys_out`] for many sources, in order.
    fn link_keys_out_many(
        &self,
        froms: &[Id],
        kind: &LinkKind,
    ) -> Result<Vec<Vec<LinkKey>>, StoreError> {
        let prefixes: Vec<Vec<u8>> = froms.iter().map(|f| link_prefix(*f, kind)).collect();
        let stored = self.raw_scan_keys_many(TableId::LinksOut, &prefixes)?;
        let fan_prefixes: Vec<Vec<u8>> = froms
            .iter()
            .map(|f| fan::fan_out_prefix(*f, Some(kind)))
            .collect();
        let fans = self.raw_scan_keys_many(TableId::FanOut, &fan_prefixes)?;
        let mut cache = HashMap::new();
        let mut out = Vec::with_capacity(froms.len());
        for ((from, keys), fks) in froms.iter().zip(stored).zip(fans) {
            let keys: Vec<LinkKey> = keys
                .iter()
                .map(|k| decode_link_key(k))
                .collect::<Result<_, _>>()?;
            let fks: Vec<FanKey> = fks
                .iter()
                .map(|k| fan::decode_fan_out_key(k))
                .collect::<Result<_, _>>()?;
            out.push(self.merge_fan_targets(keys, *from, kind, &fks, &mut cache)?);
        }
        Ok(out)
    }

    /// [`Reader::links_in`] for many targets, in order.
    fn links_in_many(&self, tos: &[Id]) -> Result<Vec<Vec<LinkKey>>, StoreError> {
        let prefixes: Vec<Vec<u8>> = tos.iter().map(|t| t.0.to_vec()).collect();
        let stored: Vec<Vec<LinkKey>> = self
            .raw_scan_keys_many(TableId::LinksIn, &prefixes)?
            .into_iter()
            .map(|keys| {
                keys.iter()
                    .map(|k| {
                        let r = decode_link_key(k)?;
                        Ok(LinkKey {
                            from: r.to,
                            kind: r.kind,
                            to: r.from,
                        })
                    })
                    .collect()
            })
            .collect::<Result<_, StoreError>>()?;
        let by_target: Vec<Vec<Vec<u8>>> = self.raw_scan_keys_many(
            TableId::ExportsByTarget,
            &tos.iter()
                .map(|t| fan::by_target_prefix(*t))
                .collect::<Vec<_>>(),
        )?;
        if by_target.iter().all(|k| k.is_empty()) {
            return Ok(stored);
        }
        let mut per_to: Vec<Vec<ExportKey>> = Vec::with_capacity(tos.len());
        let mut distinct: BTreeSet<ExportKey> = BTreeSet::new();
        for keys in &by_target {
            let ks: Vec<ExportKey> = keys
                .iter()
                .map(|k| Ok(fan::decode_by_target_key(k)?.1))
                .collect::<Result<_, StoreError>>()?;
            distinct.extend(ks.iter().cloned());
            per_to.push(ks);
        }
        let distinct: Vec<ExportKey> = distinct.into_iter().collect();
        let fan_prefixes: Vec<Vec<u8>> = distinct.iter().map(fan::fan_in_prefix).collect();
        let mut fans_of: HashMap<ExportKey, Vec<FanKey>> = HashMap::new();
        for (k, rows) in distinct
            .iter()
            .zip(self.raw_scan_keys_many(TableId::FanIn, &fan_prefixes)?)
        {
            let fks: Vec<FanKey> = rows
                .iter()
                .map(|r| fan::decode_fan_in_key(r))
                .collect::<Result<_, _>>()?;
            fans_of.insert(k.clone(), fks);
        }
        let mut out = Vec::with_capacity(tos.len());
        for ((to, st), ks) in tos.iter().zip(stored).zip(per_to) {
            let mut fans = Vec::new();
            for k in &ks {
                if let Some(f) = fans_of.get(k) {
                    fans.extend(f.iter().cloned());
                }
            }
            out.push(if fans.is_empty() {
                st
            } else {
                merge_links_in(st, *to, fans)
            });
        }
        Ok(out)
    }

    /// The [`LinkBrief`] of many links, in order (None for a link that does not exist); the
    /// reads share one table lookup. A link that fan rows reach is briefed over its merged
    /// entries.
    fn link_briefs_many(&self, keys: &[LinkKey]) -> Result<Vec<Option<LinkBrief>>, StoreError> {
        let raw: Vec<Vec<u8>> = keys.iter().map(link_out_key).collect();
        let stored = self.raw_get_many(TableId::LinksOut, &raw)?;
        // The fan keys of each distinct (from, kind) among the keys.
        let pairs: Vec<(Id, LinkKind)> = keys
            .iter()
            .map(|k| (k.from, k.kind.clone()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let prefixes: Vec<Vec<u8>> = pairs
            .iter()
            .map(|(f, k)| fan::fan_out_prefix(*f, Some(k)))
            .collect();
        let mut fans_of: HashMap<(Id, LinkKind), Vec<FanKey>> = HashMap::new();
        for (pair, rows) in pairs
            .iter()
            .zip(self.raw_scan_keys_many(TableId::FanOut, &prefixes)?)
        {
            if rows.is_empty() {
                continue;
            }
            let fks: Vec<FanKey> = rows
                .iter()
                .map(|r| fan::decode_fan_out_key(r))
                .collect::<Result<_, _>>()?;
            fans_of.insert(pair.clone(), fks);
        }
        // Fan rows that reach a key: one batched by-target lookup.
        let mut probes: Vec<(usize, FanKey)> = Vec::new();
        for (i, k) in keys.iter().enumerate() {
            if let Some(fks) = fans_of.get(&(k.from, k.kind.clone())) {
                for fk in fks {
                    probes.push((i, fk.clone()));
                }
            }
        }
        let probe_keys: Vec<Vec<u8>> = probes
            .iter()
            .map(|(i, fk)| fan::by_target_key(keys[*i].to, &fk.key))
            .collect();
        let hit = self.raw_get_many(TableId::ExportsByTarget, &probe_keys)?;
        let mut extra: HashMap<usize, Vec<RawLinkEntry>> = HashMap::new();
        let mut rows: HashMap<FanKey, Vec<RawLinkEntry>> = HashMap::new();
        let mut counts: HashMap<ExportKey, usize> = HashMap::new();
        for ((i, fk), h) in probes.iter().zip(hit) {
            if h.is_none() {
                continue;
            }
            if !rows.contains_key(fk) {
                let row = self
                    .fan(fk)?
                    .ok_or_else(|| StoreError::Row("fan_in without fan_out".into()))?;
                rows.insert(
                    fk.clone(),
                    row.entries
                        .iter()
                        .map(|e| RawLinkEntry {
                            origin: e.origin,
                            confidence: e.confidence,
                            fanout: None,
                        })
                        .collect(),
                );
            }
            if !counts.contains_key(&fk.key) {
                counts.insert(fk.key.clone(), self.exports(&fk.key)?.len());
            }
            let n = u32::try_from(counts[&fk.key]).unwrap_or(u32::MAX);
            extra
                .entry(*i)
                .or_default()
                .extend(rows[fk].iter().map(|e| RawLinkEntry {
                    origin: e.origin,
                    confidence: e.confidence,
                    fanout: Some(n),
                }));
        }
        let mut out = Vec::with_capacity(keys.len());
        for (i, v) in stored.into_iter().enumerate() {
            let mut entries: Option<Vec<RawLinkEntry>> = match v {
                Some(v) => Some(
                    serde_json::from_slice::<RawLinkRow>(&v)
                        .map_err(|e| StoreError::Row(e.to_string()))?
                        .entries,
                ),
                None => None,
            };
            if let Some(more) = extra.remove(&i) {
                let all = entries.get_or_insert_with(Vec::new);
                all.extend(more);
                all.sort_by_key(|e| e.origin);
            }
            out.push(
                entries
                    .map(|entries| {
                        let first = entries
                            .first()
                            .ok_or_else(|| StoreError::Row("link without entries".into()))?;
                        let weight = entries
                            .iter()
                            .map(|e| match (e.confidence, e.fanout) {
                                (Confidence::Inferred, Some(n)) if n > 1 => 1.0 / f64::from(n),
                                _ => 1.0,
                            })
                            .fold(0.0f64, f64::max);
                        Ok::<LinkBrief, StoreError>(LinkBrief {
                            confidence: first.confidence,
                            weight,
                        })
                    })
                    .transpose()?,
            );
        }
        Ok(out)
    }

    /// Number of links arriving at each target, all kinds. A symbol with no incoming link is
    /// absent. Counts the expanded links.
    fn in_degrees(&self) -> Result<BTreeMap<Id, u64>, StoreError> {
        let mut out: BTreeMap<Id, u64> = BTreeMap::new();
        for (key, _) in self.links_all()? {
            *out.entry(key.to).or_default() += 1;
        }
        Ok(out)
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

    /// Keys of the stored rows arriving at `to`, in `links_in` key order; no fan rows.
    fn stored_links_in(&self, to: Id) -> Result<Vec<LinkKey>, StoreError> {
        self.raw_scan_keys(TableId::LinksIn, &to.0)?
            .iter()
            .map(|k| {
                let r = decode_link_key(k)?;
                Ok(LinkKey {
                    from: r.to,
                    kind: r.kind,
                    to: r.from,
                })
            })
            .collect()
    }

    /// The contract node `id` names, if any binding still does.
    fn contract(&self, id: Id) -> Result<Option<ContractRow>, StoreError> {
        self.raw_get(TableId::Contracts, &id.0)?
            .map(|v| de(&v))
            .transpose()
    }

    /// Every contract node, sorted by id.
    fn contracts(&self) -> Result<Vec<(Id, ContractRow)>, StoreError> {
        self.raw_scan(TableId::Contracts, &[])?
            .into_iter()
            .map(|(k, v)| Ok((Id(id_bytes(&k)?), de(&v)?)))
            .collect()
    }

    /// The bindings one artifact contributed (empty when none).
    fn bindings_of(&self, artifact: Id) -> Result<Vec<BindingRow>, StoreError> {
        Ok(self
            .raw_get(TableId::BindingsByArtifact, &artifact.0)?
            .map(|v| de(&v))
            .transpose()?
            .unwrap_or_default())
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
        let prefix = ref_by_key_prefix(key);
        self.raw_scan_keys(TableId::RefsByKey, &prefix)?
            .iter()
            .map(|k| {
                let tail = k.len().checked_sub(20).filter(|&n| n >= prefix.len());
                match tail {
                    Some(n) => decode_ref_key(&k[n..]),
                    None => Err(StoreError::Row("malformed key".into())),
                }
            })
            .collect()
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

    /// Every recorded index error, sorted by `(repo, path)` key bytes.
    fn index_errors_all(&self) -> Result<Vec<(ShardKey, Vec<ErrorRow>)>, StoreError> {
        self.raw_scan(TableId::IndexErrors, &[])?
            .into_iter()
            .map(|(k, v)| {
                let (repo, path) = decode_path_key(&k)?;
                Ok(((repo, path), de(&v)?))
            })
            .collect()
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

    /// The watermark the stored rank reflects, when a rank is stored.
    fn rank_watermark(&self) -> Result<Option<u64>, StoreError> {
        self.meta(RANK_WATERMARK_META)?.map(u64_value).transpose()
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
    _cache: PhantomData<&'a ()>,
}

impl ReadTx<'_> {
    /// A snapshot over a transaction from any database handle (the read-only inspection uses
    /// one from a `ReadOnlyDatabase`).
    pub(crate) fn from_txn(txn: ReadTransaction) -> Self {
        ReadTx {
            txn,
            _cache: PhantomData,
        }
    }
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

    fn raw_scan_keys(&self, t: TableId, prefix: &[u8]) -> Result<Vec<Vec<u8>>, StoreError> {
        scan_keys_in(&self.txn.open_table(t.def()).map_err(db)?, prefix)
    }

    fn raw_get_many(
        &self,
        t: TableId,
        keys: &[Vec<u8>],
    ) -> Result<Vec<Option<Vec<u8>>>, StoreError> {
        let table = self.txn.open_table(t.def()).map_err(db)?;
        keys.iter().map(|k| get_in(&table, k)).collect()
    }

    fn raw_scan_keys_many(
        &self,
        t: TableId,
        prefixes: &[Vec<u8>],
    ) -> Result<Vec<Vec<Vec<u8>>>, StoreError> {
        let table = self.txn.open_table(t.def()).map_err(db)?;
        prefixes.iter().map(|p| scan_keys_in(&table, p)).collect()
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
    /// Fan rows and export keys with their stored bytes when first touched (FEAT-077).
    fans: BTreeMap<FanKey, Option<Vec<u8>>>,
    exports: BTreeMap<ExportKey, Option<Vec<u8>>>,
}

impl Reader for WriteTx<'_> {
    fn raw_get(&self, t: TableId, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        get_in(&self.txn.open_table(t.def()).map_err(db)?, key)
    }

    fn raw_scan(&self, t: TableId, prefix: &[u8]) -> Result<RawRows, StoreError> {
        scan_in(&self.txn.open_table(t.def()).map_err(db)?, prefix)
    }

    fn raw_scan_keys(&self, t: TableId, prefix: &[u8]) -> Result<Vec<Vec<u8>>, StoreError> {
        scan_keys_in(&self.txn.open_table(t.def()).map_err(db)?, prefix)
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

    pub fn put_contract(&mut self, id: Id, row: &ContractRow) -> Result<(), StoreError> {
        self.put(TableId::Contracts, &id.0, &ser(row)?)
    }

    pub fn remove_contract(&mut self, id: Id) -> Result<(), StoreError> {
        self.del(TableId::Contracts, &id.0)
    }

    /// An empty list removes the row.
    pub fn put_bindings(&mut self, artifact: Id, rows: &[BindingRow]) -> Result<(), StoreError> {
        if rows.is_empty() {
            self.del(TableId::BindingsByArtifact, &artifact.0)
        } else {
            self.put(TableId::BindingsByArtifact, &artifact.0, &ser(&rows)?)
        }
    }

    /// Write the fan row `key` (entries sorted by origin, never empty, each `fanout: None`) to
    /// `fan_out` and its key-only reverse row to `fan_in`. Recorded for rank tracking.
    pub fn put_fan(&mut self, key: &FanKey, row: &LinkRow) -> Result<(), StoreError> {
        debug_assert!(!row.entries.is_empty(), "fan row without entries");
        self.touch_fan(key)?;
        self.put(TableId::FanOut, &fan::fan_out_key(key), &ser(row)?)?;
        self.put(TableId::FanIn, &fan::fan_in_key(key), &[])
    }

    /// Delete the fan row and its `fan_in` row; absent is fine.
    pub fn remove_fan(&mut self, key: &FanKey) -> Result<(), StoreError> {
        self.touch_fan(key)?;
        self.del(TableId::FanOut, &fan::fan_out_key(key))?;
        self.del(TableId::FanIn, &fan::fan_in_key(key))
    }

    /// An empty set removes the row. Also keeps `exports_by_target`: a row `(t, key)` exists
    /// exactly when `t` is in `targets` and `targets` has two or more entries.
    pub fn put_exports(&mut self, key: &ExportKey, targets: &[Id]) -> Result<(), StoreError> {
        let ek = export_key(key);
        let old_raw = self.raw_get(TableId::Exports, &ek)?;
        let old: Vec<Id> = match &old_raw {
            Some(v) => de(v)?,
            None => Vec::new(),
        };
        self.touched.exports.entry(key.clone()).or_insert(old_raw);
        let no_rows: &[Id] = &[];
        let old_rows = if old.len() >= 2 { &old[..] } else { no_rows };
        let new_rows = if targets.len() >= 2 { targets } else { no_rows };
        for t in old_rows.iter().filter(|t| !new_rows.contains(t)) {
            self.del(TableId::ExportsByTarget, &fan::by_target_key(*t, key))?;
        }
        for t in new_rows.iter().filter(|t| !old_rows.contains(t)) {
            self.put(TableId::ExportsByTarget, &fan::by_target_key(*t, key), &[])?;
        }
        if targets.is_empty() {
            self.del(TableId::Exports, &ek)
        } else {
            self.put(TableId::Exports, &ek, &ser(&targets)?)
        }
    }

    /// Insert the row `(key, r)`; idempotent.
    pub fn add_ref_by_key(&mut self, key: &ExportKey, r: RefId) -> Result<(), StoreError> {
        self.put(TableId::RefsByKey, &ref_by_key_key(key, r), &[])
    }

    /// Delete the row `(key, r)`; absent is fine.
    pub fn remove_ref_by_key(&mut self, key: &ExportKey, r: RefId) -> Result<(), StoreError> {
        self.del(TableId::RefsByKey, &ref_by_key_key(key, r))
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
            let weight = self.stored_link(key)?.map(|r| crate::rank::row_weight(&r));
            self.touched.links.insert(key.clone(), weight);
        }
        Ok(())
    }

    /// Remember the stored bytes a fan row had when this transaction first touched it.
    fn touch_fan(&mut self, key: &FanKey) -> Result<(), StoreError> {
        if !self.touched.fans.contains_key(key) {
            let was = self.raw_get(TableId::FanOut, &fan::fan_out_key(key))?;
            self.touched.fans.insert(key.clone(), was);
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
            let now = self.stored_link(key)?.map(|r| crate::rank::row_weight(&r));
            if now != *was {
                return Ok(true);
            }
        }
        for (key, was) in &self.touched.fans {
            if self.raw_get(TableId::FanOut, &fan::fan_out_key(key))? != *was {
                return Ok(true);
            }
        }
        // A changed export key moves the weights of the fan rows that name it.
        for (key, was) in &self.touched.exports {
            if self.raw_get(TableId::Exports, &export_key(key))? != *was
                && !self
                    .raw_scan_keys(TableId::FanIn, &fan::fan_in_prefix(key))?
                    .is_empty()
            {
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
    pub fn of(meta: &std::fs::Metadata) -> Option<ShardStat> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Kindex, ProjectConfig, ProjectName, Restitch};
    use camino::Utf8PathBuf;
    use katalogr_model::{ArtifactName, Fragment};
    use std::time::Duration;

    struct Noop;

    impl Restitch for Noop {
        fn replace_fragment(&self, _: &mut WriteTx, _: Fragment) -> Result<(), StoreError> {
            Ok(())
        }
        fn remove_artifact(&self, _: &mut WriteTx, _: &ArtifactName) -> Result<(), StoreError> {
            Ok(())
        }
    }

    fn cache(t: &tempfile::TempDir) -> Cache {
        let root = Utf8PathBuf::from_path_buf(t.path().join("kx")).unwrap();
        let dir = Kindex::init(&root)
            .unwrap()
            .create_project(&ProjectName::new("p").unwrap(), ProjectConfig::default())
            .unwrap();
        Cache::open_or_rebuild(&dir, &Noop, Instant::now() + Duration::from_secs(5)).unwrap()
    }

    fn key(ns: &str, k: &str) -> ExportKey {
        ExportKey {
            namespace: ns.into(),
            key: k.into(),
        }
    }

    fn rid(a: u8, index: u32) -> RefId {
        RefId {
            artifact: Id([a; 16]),
            index,
        }
    }

    #[test]
    fn refs_by_key_rows_sorted() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        let k = key("fn", "a");
        let mut tx = c.write().unwrap();
        let rows = [rid(2, 0), rid(1, 300), rid(1, 2), rid(3, 1)];
        for r in rows {
            tx.add_ref_by_key(&k, r).unwrap();
        }
        tx.add_ref_by_key(&k, rid(1, 2)).unwrap(); // idempotent
        let mut want = rows.to_vec();
        want.sort();
        assert_eq!(tx.refs_by_key(&k).unwrap(), want);
        tx.remove_ref_by_key(&k, rid(1, 300)).unwrap();
        tx.remove_ref_by_key(&k, rid(9, 9)).unwrap(); // absent is fine
        want.retain(|r| *r != rid(1, 300));
        assert_eq!(tx.refs_by_key(&k).unwrap(), want);
        tx.commit().unwrap();
        assert_eq!(c.read().unwrap().refs_by_key(&k).unwrap(), want);
    }

    #[test]
    fn refs_by_key_prefix_isolated() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        let mut tx = c.write().unwrap();
        tx.add_ref_by_key(&key("fn", "a"), rid(1, 0)).unwrap();
        tx.add_ref_by_key(&key("fn", "ab"), rid(2, 0)).unwrap();
        tx.add_ref_by_key(&key("f", "na"), rid(3, 0)).unwrap();
        assert_eq!(tx.refs_by_key(&key("fn", "a")).unwrap(), vec![rid(1, 0)]);
        assert_eq!(tx.refs_by_key(&key("fn", "ab")).unwrap(), vec![rid(2, 0)]);
        assert_eq!(tx.refs_by_key(&key("f", "na")).unwrap(), vec![rid(3, 0)]);
        assert!(tx.refs_by_key(&key("fn", "")).unwrap().is_empty());
        tx.remove_ref_by_key(&key("fn", "a"), rid(1, 0)).unwrap();
        assert!(tx.refs_by_key(&key("fn", "a")).unwrap().is_empty());
        assert_eq!(tx.refs_by_key(&key("fn", "ab")).unwrap(), vec![rid(2, 0)]);
    }

    fn nid(n: u8) -> Id {
        Id([n; 16])
    }

    fn lk(from: u8, kind: &str, to: u8) -> LinkKey {
        LinkKey {
            from: nid(from),
            kind: LinkKind(kind.into()),
            to: nid(to),
        }
    }

    fn fk(from: u8, kind: &str, k: &ExportKey) -> FanKey {
        FanKey {
            from: nid(from),
            kind: LinkKind(kind.into()),
            key: k.clone(),
        }
    }

    fn entry(origin: LinkOrigin, confidence: Confidence, fanout: Option<u32>) -> LinkEntry {
        LinkEntry {
            origin,
            confidence,
            span: None,
            provenance: Provenance {
                repo: "r".into(),
                path: RelPath::new("a.tx").unwrap(),
                revision: "0".repeat(40),
            },
            fanout,
            produced_by: None,
        }
    }

    fn fan_row(origins: &[u32]) -> LinkRow {
        LinkRow {
            entries: origins
                .iter()
                .map(|i| entry(LinkOrigin::Ref(*i), Confidence::Inferred, None))
                .collect(),
        }
    }

    fn count(tx: &impl Reader, t: TableId) -> usize {
        tx.raw_scan(t, &[]).unwrap().len()
    }

    #[test]
    fn put_fan_writes_both_tables() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        let k = key("ns", "k");
        let f = fk(1, "calls", &k);
        let mut tx = c.write().unwrap();
        tx.put_fan(&f, &fan_row(&[0, 1])).unwrap();
        assert_eq!(count(&tx, TableId::FanOut), 1);
        assert_eq!(count(&tx, TableId::FanIn), 1);
        assert_eq!(tx.fan(&f).unwrap(), Some(fan_row(&[0, 1])));
        assert_eq!(tx.fans_in(&k).unwrap(), vec![f.clone()]);
        assert_eq!(
            tx.fans_out(nid(1), None).unwrap(),
            vec![(f.clone(), fan_row(&[0, 1]))]
        );
        assert_eq!(tx.fans_all().unwrap().len(), 1);
        tx.remove_fan(&f).unwrap();
        tx.remove_fan(&f).unwrap(); // absent is fine
        assert_eq!(count(&tx, TableId::FanOut), 0);
        assert_eq!(count(&tx, TableId::FanIn), 0);
    }

    #[test]
    fn exports_by_target_tracks_multi_target_keys() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        let k = key("ns", "k");
        let mut tx = c.write().unwrap();
        let keys_of = |tx: &WriteTx, n: u8| tx.fan_keys_of(nid(n)).unwrap();
        tx.put_exports(&k, &[nid(1)]).unwrap();
        assert_eq!(count(&tx, TableId::ExportsByTarget), 0);
        tx.put_exports(&k, &[nid(1), nid(2)]).unwrap();
        assert_eq!(keys_of(&tx, 1), vec![k.clone()]);
        assert_eq!(keys_of(&tx, 2), vec![k.clone()]);
        tx.put_exports(&k, &[nid(1), nid(2), nid(3)]).unwrap();
        assert_eq!(count(&tx, TableId::ExportsByTarget), 3);
        tx.put_exports(&k, &[nid(2)]).unwrap();
        assert_eq!(count(&tx, TableId::ExportsByTarget), 0);
        tx.put_exports(&k, &[nid(2), nid(5)]).unwrap();
        tx.put_exports(&k, &[]).unwrap();
        assert_eq!(count(&tx, TableId::ExportsByTarget), 0);
        assert!(tx.exports(&k).unwrap().is_empty());
    }

    /// A fan row on `k` (targets 1..=3) from node 9 plus a stored row to node 2.
    fn fan_graph(c: &Cache) {
        let k = key("ns", "k");
        let mut tx = c.write().unwrap();
        tx.put_exports(&k, &[nid(1), nid(2), nid(3)]).unwrap();
        tx.put_fan(&fk(9, "calls", &k), &fan_row(&[4])).unwrap();
        tx.put_link(
            &lk(9, "calls", 2),
            &LinkRow {
                entries: vec![entry(LinkOrigin::Intra, Confidence::Extracted, None)],
            },
        )
        .unwrap();
        tx.put_link(
            &lk(8, "calls", 3),
            &LinkRow {
                entries: vec![entry(LinkOrigin::Intra, Confidence::Extracted, None)],
            },
        )
        .unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn expanded_links_all_sorted_and_merged() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        fan_graph(&c);
        let rx = c.read().unwrap();
        let all = rx.links_all().unwrap();
        let keys: Vec<LinkKey> = all.iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(
            keys,
            vec![
                lk(8, "calls", 3),
                lk(9, "calls", 1),
                lk(9, "calls", 2),
                lk(9, "calls", 3)
            ]
        );
        let merged = &all[2].1;
        let origins: Vec<LinkOrigin> = merged.entries.iter().map(|e| e.origin).collect();
        assert_eq!(origins, vec![LinkOrigin::Intra, LinkOrigin::Ref(4)]);
        assert_eq!(merged.entries[1].fanout, Some(3));
        assert_eq!(merged.entries[1].confidence, Confidence::Inferred);
        let out = rx.links_out(nid(9), None).unwrap();
        assert_eq!(out, all[1..].to_vec());
        assert_eq!(
            rx.links_out(nid(9), Some(&LinkKind("other".into())))
                .unwrap(),
            vec![]
        );
        assert_eq!(rx.in_degrees().unwrap()[&nid(3)], 2);
    }

    #[test]
    fn link_point_read_merges_fan_entries() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        fan_graph(&c);
        let rx = c.read().unwrap();
        let merged = rx.link(&lk(9, "calls", 2)).unwrap().unwrap();
        assert_eq!(merged.entries.len(), 2);
        let only_fan = rx.link(&lk(9, "calls", 1)).unwrap().unwrap();
        assert_eq!(only_fan.entries.len(), 1);
        assert_eq!(only_fan.entries[0].fanout, Some(3));
        assert!(rx.link(&lk(9, "calls", 7)).unwrap().is_none());
        assert!(rx.link(&lk(9, "other", 1)).unwrap().is_none());
        let stored = rx.link(&lk(8, "calls", 3)).unwrap().unwrap();
        assert_eq!(stored.entries.len(), 1);
        assert_eq!(rx.stored_link(&lk(9, "calls", 1)).unwrap(), None);
    }

    #[test]
    fn links_in_includes_fan_targets() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        fan_graph(&c);
        let rx = c.read().unwrap();
        assert_eq!(
            rx.links_in(nid(3)).unwrap(),
            vec![lk(8, "calls", 3), lk(9, "calls", 3)]
        );
        assert_eq!(rx.links_in(nid(1)).unwrap(), vec![lk(9, "calls", 1)]);
        // Stored and fan on the same link: one key.
        assert_eq!(rx.links_in(nid(2)).unwrap(), vec![lk(9, "calls", 2)]);
        assert_eq!(rx.stored_links_in(nid(1)).unwrap(), vec![]);
        let many = rx.links_in_many(&[nid(3), nid(1), nid(7)]).unwrap();
        assert_eq!(many[0], rx.links_in(nid(3)).unwrap());
        assert_eq!(many[1], rx.links_in(nid(1)).unwrap());
        assert!(many[2].is_empty());
    }

    #[test]
    fn briefs_and_keys_many_see_fan_targets() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        fan_graph(&c);
        let rx = c.read().unwrap();
        let keys = [
            lk(9, "calls", 1),
            lk(9, "calls", 2),
            lk(8, "calls", 3),
            lk(9, "calls", 6),
        ];
        let briefs = rx.link_briefs_many(&keys).unwrap();
        let rows: Vec<Option<LinkRow>> = keys.iter().map(|k| rx.link(k).unwrap()).collect();
        for (b, r) in briefs.iter().zip(&rows) {
            let want = r.as_ref().map(|r| LinkBrief {
                confidence: r.entries[0].confidence,
                weight: crate::rank::row_weight(r),
            });
            assert_eq!(*b, want);
        }
        assert!(briefs[3].is_none());
        let kind = LinkKind("calls".into());
        let one = rx.link_keys_out(nid(9), &kind).unwrap();
        assert_eq!(
            one,
            vec![lk(9, "calls", 1), lk(9, "calls", 2), lk(9, "calls", 3)]
        );
        let many = rx
            .link_keys_out_many(&[nid(9), nid(8), nid(7)], &kind)
            .unwrap();
        assert_eq!(many[0], one);
        assert_eq!(many[1], vec![lk(8, "calls", 3)]);
        assert!(many[2].is_empty());
    }

    #[test]
    fn fanout_derived_from_target_count() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        fan_graph(&c);
        let k = key("ns", "k");
        let mut tx = c.write().unwrap();
        assert_eq!(
            tx.link(&lk(9, "calls", 1)).unwrap().unwrap().entries[0].fanout,
            Some(3)
        );
        tx.put_exports(&k, &[nid(1), nid(2), nid(3), nid(4)])
            .unwrap();
        assert_eq!(
            tx.link(&lk(9, "calls", 1)).unwrap().unwrap().entries[0].fanout,
            Some(4)
        );
        assert!(tx.link(&lk(9, "calls", 4)).unwrap().is_some());
        let w = tx.link_briefs_many(&[lk(9, "calls", 1)]).unwrap();
        assert_eq!(w[0].unwrap().weight, 0.25);
    }

    #[test]
    fn fan_sources_names_keys_and_counts() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        let (a, b) = (key("ns", "a"), key("ns", "b"));
        let mut tx = c.write().unwrap();
        tx.put_exports(&a, &[nid(1), nid(2)]).unwrap();
        tx.put_exports(&b, &[nid(2), nid(3), nid(4)]).unwrap();
        tx.put_fan(&fk(9, "calls", &a), &fan_row(&[0])).unwrap();
        tx.put_fan(&fk(9, "calls", &b), &fan_row(&[1])).unwrap();
        tx.put_link(
            &lk(9, "calls", 2),
            &LinkRow {
                entries: vec![entry(LinkOrigin::Intra, Confidence::Extracted, None)],
            },
        )
        .unwrap();
        assert_eq!(
            tx.fan_sources(&lk(9, "calls", 2)).unwrap(),
            vec![(a.clone(), 2), (b.clone(), 3)]
        );
        assert_eq!(tx.fan_sources(&lk(9, "calls", 1)).unwrap(), vec![(a, 2)]);
        assert_eq!(tx.fan_sources(&lk(9, "calls", 4)).unwrap(), vec![(b, 3)]);
        assert!(tx.fan_sources(&lk(9, "calls", 6)).unwrap().is_empty());
        assert!(tx.fan_sources(&lk(9, "other", 2)).unwrap().is_empty());
    }

    #[test]
    fn fan_change_invalidates_rank() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        fan_graph(&c);
        let k = key("ns", "k");
        let mut tx = c.write().unwrap();
        assert!(!tx.rank_inputs_changed().unwrap());
        tx.put_fan(&fk(9, "calls", &k), &fan_row(&[4, 5])).unwrap();
        assert!(tx.rank_inputs_changed().unwrap());
        drop(tx);
        let mut tx = c.write().unwrap();
        tx.remove_fan(&fk(9, "calls", &k)).unwrap();
        assert!(tx.rank_inputs_changed().unwrap());
        drop(tx);
        let mut tx = c.write().unwrap();
        tx.put_fan(&fk(7, "calls", &k), &fan_row(&[0])).unwrap();
        assert!(tx.rank_inputs_changed().unwrap());
    }

    #[test]
    fn export_change_under_fan_invalidates_rank() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        fan_graph(&c);
        let k = key("ns", "k");
        let mut tx = c.write().unwrap();
        tx.put_exports(&k, &[nid(1), nid(2), nid(3), nid(4)])
            .unwrap();
        assert!(tx.rank_inputs_changed().unwrap());
        drop(tx);
        // A key no fan row names does not.
        let mut tx = c.write().unwrap();
        tx.put_exports(&key("ns", "other"), &[nid(1), nid(2)])
            .unwrap();
        assert!(!tx.rank_inputs_changed().unwrap());
    }

    #[test]
    fn unchanged_fan_keeps_rank() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        fan_graph(&c);
        let k = key("ns", "k");
        let mut tx = c.write().unwrap();
        tx.put_fan(&fk(9, "calls", &k), &fan_row(&[4])).unwrap();
        tx.put_exports(&k, &[nid(1), nid(2)]).unwrap();
        tx.put_exports(&k, &[nid(1), nid(2), nid(3)]).unwrap();
        assert!(!tx.rank_inputs_changed().unwrap());
        // Removed and written back.
        tx.remove_fan(&fk(9, "calls", &k)).unwrap();
        tx.put_fan(&fk(9, "calls", &k), &fan_row(&[4])).unwrap();
        assert!(!tx.rank_inputs_changed().unwrap());
    }
}
