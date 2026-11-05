//! Local redb cache rebuilt from the shards (FEAT-003 section 3 to 8, FEAT-001 section 5).
//!
//! Every table is `&[u8] -> &[u8]`. Keys are built by the functions below (big-endian, length
//! prefixed strings, so ranges by prefix are exact). Since schema 13 (FEAT-075) the graph rows are
//! postcard bytes of the wire structs in `codec.rs`, with link kinds, namespaces and provenance
//! interned through the dictionaries in `dict.rs`; readers give back the logical rows.

use crate::codec::{self, EntryBrief, Pid, Vid};
use crate::compact::{
    self, COMPACTED_BYTES_META, CompactMode, CompactOutcome, CompactPolicy, CompactReport,
};
use crate::dict;
use crate::error::StoreError;
use crate::fan;
use crate::kindex::ProjectDir;
use crate::postings::{self, PostingBuf};
use camino::{Utf8Path, Utf8PathBuf};
use katalogr_model::{
    ArtifactName, Confidence, ContractName, ContractRole, Export, ExportKey, Fact, Fragment, Id,
    LinkKind, Name, OutRef, Provenance, RelPath, Span, SymbolName,
};
use redb::{
    ConcurrencyMode, Database, DatabaseError, ReadOnlyDatabase, ReadTransaction, ReadableDatabase,
    ReadableTable, StorageError, TableDefinition, WriteTransaction,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::marker::PhantomData;
use std::sync::{Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

/// Stored in `meta`; a different number rebuilds the cache from the shards.
pub const CACHE_SCHEMA: u32 = 13;

/// `meta` key: the watermark the stored rank reflects.
const RANK_WATERMARK_META: &str = "rank_watermark";

/// `meta` key: the head of the stored rank (FEAT-076).
const RANK_TOP_META: &str = "rank_top";

/// How many entries of the rank's head are stored beside it.
pub const RANK_TOP_ENTRIES: usize = 4096;

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

/// A symbol without its facts, read without JSON (FEAT-076). `name` is the canonical name and
/// `path` the path of the symbol's artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolCore {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub artifact: Id,
    pub span: Span,
}

/// The head of the stored rank: the symbol count and the best `RANK_TOP_ENTRIES` scores, by
/// score descending then canonical name.
#[derive(Clone, Debug, PartialEq)]
pub struct RankTop {
    pub total: u64,
    pub entries: Vec<(Id, f64)>,
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

/// The brief of a symbol from its canonical name and facts.
fn symbol_brief(name: &str, facts: BTreeMap<String, Fact>) -> SymbolBrief {
    // `<artifact>#<kind>:<name>/…[~<ordinal>]`: '/' and '~' inside a name are escaped.
    let body = name.split_once('#').map_or("", |(_, b)| b);
    let last = body.rsplit('/').next().unwrap_or("");
    let last = match last.rsplit_once('~') {
        Some((l, n)) if !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()) => l,
        _ => last,
    };
    let raw = last.split_once(':').map_or("", |(_, n)| n);
    SymbolBrief {
        leaf: percent_decode(raw),
        facts,
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

/// One posting: a term occurs `tf` times in `field` of `symbol`, whose per-field token counts
/// are `doc_len`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Posting {
    pub field: u8,
    pub symbol: Id,
    pub tf: u32,
    pub doc_len: [u32; LEXICON_FIELDS],
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

/// The `symbol_core` key of a symbol: `lp(lowercased plain name) || id`.
fn core_key(row: &SymbolRow, id: Id) -> Vec<u8> {
    let plain = row.name.segments.last().map_or("", |s| s.name.as_str());
    let mut k = Vec::new();
    lp(&mut k, &plain.to_lowercase());
    k.extend_from_slice(&id.0);
    k
}

/// The `symbol_core` value of a symbol: `lp(canonical) || lp(kind) || lp(path) || artifact ||
/// start_line, start_col, end_line, end_col` (big-endian u32).
fn core_value(row: &SymbolRow) -> Vec<u8> {
    let mut v = Vec::new();
    lp(&mut v, &row.name.canonical());
    lp(&mut v, &row.kind);
    lp(&mut v, row.name.artifact.path.as_str());
    v.extend_from_slice(&row.artifact.0);
    for n in [
        row.span.start_line,
        row.span.start_col,
        row.span.end_line,
        row.span.end_col,
    ] {
        v.extend_from_slice(&n.to_be_bytes());
    }
    v
}

fn core_decode(b: &[u8]) -> Result<SymbolCore, StoreError> {
    let bad = || StoreError::Row("bad symbol_core row".into());
    let (name, rest) = split_lp(b).map_err(|_| bad())?;
    let (kind, rest) = split_lp(rest).map_err(|_| bad())?;
    let (path, rest) = split_lp(rest).map_err(|_| bad())?;
    if rest.len() != 32 {
        return Err(bad());
    }
    let artifact = Id(rest[..16].try_into().expect("16 bytes"));
    let n =
        |i: usize| u32::from_be_bytes(rest[16 + i * 4..20 + i * 4].try_into().expect("4 bytes"));
    Ok(SymbolCore {
        name: name.to_string(),
        kind: kind.to_string(),
        path: path.to_string(),
        artifact,
        span: Span {
            start_line: n(0),
            start_col: n(1),
            end_line: n(2),
            end_col: n(3),
        },
    })
}

/// `exports` scan prefix: the namespace's vocabulary id.
fn export_prefix(ns: Vid) -> Vec<u8> {
    ns.to_be_bytes().to_vec()
}

/// `exports` key: `vid(namespace) || key bytes`.
fn export_key(ns: Vid, key: &str) -> Vec<u8> {
    let mut out = export_prefix(ns);
    out.extend_from_slice(key.as_bytes());
    out
}

/// The distinct parent directories of the journal's final paths under `root`.
fn journal_parents(root: &Utf8Path, entries: &[JournalEntry]) -> BTreeSet<Utf8PathBuf> {
    entries
        .iter()
        .filter_map(|(_, fin)| root.join(fin).parent().map(Utf8Path::to_path_buf))
        .collect()
}

/// Scan prefix of the `refs_by_key` rows of one export key: `vid(namespace) || lp(key)`. The
/// length prefix keeps `fn:a` from matching `fn:ab`.
fn ref_by_key_prefix(ns: Vid, key: &str) -> Vec<u8> {
    let mut out = ns.to_be_bytes().to_vec();
    lp(&mut out, key);
    out
}

/// `refs_by_key` row key: the prefix, then the 16-byte artifact id and the big-endian index.
fn ref_by_key_key(ns: Vid, key: &str, r: RefId) -> Vec<u8> {
    let mut out = ref_by_key_prefix(ns, key);
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

/// `links_out` scan prefix: `from`, then the kind's vocabulary id when a kind is given.
fn link_prefix(from: Id, kind: Option<Vid>) -> Vec<u8> {
    let mut k = from.0.to_vec();
    if let Some(v) = kind {
        k.extend_from_slice(&v.to_be_bytes());
    }
    k
}

/// `links_out` key: `from || vid(kind) || to`; the `links_in` key is the mirror.
fn link_out_key(from: Id, kind: Vid, to: Id) -> Vec<u8> {
    let mut out = link_prefix(from, Some(kind));
    out.extend_from_slice(&to.0);
    out
}

/// The schema-9 order of link keys: `from || lp(kind) || to`. Rows are stored by vocabulary id
/// now, and every reader still answers in this order.
pub(crate) fn link_order_key(k: &LinkKey) -> Vec<u8> {
    let mut out = k.from.0.to_vec();
    lp(&mut out, &k.kind.0);
    out.extend_from_slice(&k.to.0);
    out
}

/// Kind strings in the order of their length-prefixed bytes.
fn kind_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// Link keys in `links_out` schema-9 key order.
fn link_cmp(a: &LinkKey, b: &LinkKey) -> std::cmp::Ordering {
    a.from
        .cmp(&b.from)
        .then_with(|| kind_cmp(&a.kind.0, &b.kind.0))
        .then_with(|| a.to.cmp(&b.to))
}

/// `stored` keys arriving at `to` plus the keys of `fans` (rows that reach `to`), sorted by
/// the schema-9 `links_in` key bytes and deduplicated.
fn merge_links_in(stored: Vec<LinkKey>, to: Id, fans: Vec<FanKey>) -> Vec<LinkKey> {
    let mut all: Vec<LinkKey> = stored
        .into_iter()
        .chain(fans.into_iter().map(|f| LinkKey {
            from: f.from,
            kind: f.kind,
            to,
        }))
        .collect();
    all.sort_by(|a, b| kind_cmp(&a.kind.0, &b.kind.0).then_with(|| a.from.cmp(&b.from)));
    all.dedup_by(|a, b| a.kind == b.kind && a.from == b.from);
    all
}

/// `(from, kind id, to)` of a `links_out` key; a `links_in` key reads as `(to, kind id, from)`.
fn decode_link_key(b: &[u8]) -> Result<(Id, Vid, Id), StoreError> {
    let bad = || StoreError::Row("malformed key".into());
    if b.len() != 34 {
        return Err(bad());
    }
    Ok((
        Id(b[..16].try_into().map_err(|_| bad())?),
        Vid::from_be_bytes(b[16..18].try_into().map_err(|_| bad())?),
        Id(b[18..].try_into().map_err(|_| bad())?),
    ))
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
    codec::enc(v)
}

fn de<T: for<'a> Deserialize<'a>>(b: &[u8]) -> Result<T, StoreError> {
    codec::dec(b)
}

pub(crate) fn db<E: Into<redb::Error>>(e: E) -> StoreError {
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
    DocOrd,
    DocByOrd,
    LexiconStats,
    Rank,
    Contracts,
    BindingsByArtifact,
    FanOut,
    FanIn,
    ExportsByTarget,
    BoundRefsByKey,
    SymbolCore,
    Vocab,
    VocabIx,
    Prov,
    ProvIx,
}

impl TableId {
    pub const ALL: [TableId; 33] = [
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
        TableId::DocOrd,
        TableId::DocByOrd,
        TableId::LexiconStats,
        TableId::Rank,
        TableId::Contracts,
        TableId::BindingsByArtifact,
        TableId::FanOut,
        TableId::FanIn,
        TableId::ExportsByTarget,
        TableId::BoundRefsByKey,
        TableId::SymbolCore,
        TableId::Vocab,
        TableId::VocabIx,
        TableId::Prov,
        TableId::ProvIx,
    ];

    /// The graph tables, in the order `dump_graph` reports them.
    const GRAPH: [TableId; 15] = [
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
        TableId::BoundRefsByKey,
        TableId::SymbolCore,
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
            TableId::DocOrd => "doc_ord",
            TableId::DocByOrd => "doc_by_ord",
            TableId::LexiconStats => "lexicon_stats",
            TableId::Rank => "rank",
            TableId::Contracts => "contracts",
            TableId::BindingsByArtifact => "bindings_by_artifact",
            TableId::FanOut => "fan_out",
            TableId::FanIn => "fan_in",
            TableId::ExportsByTarget => "exports_by_target",
            TableId::BoundRefsByKey => "bound_refs_by_key",
            TableId::SymbolCore => "symbol_core",
            TableId::Vocab => "vocab",
            TableId::VocabIx => "vocab_ix",
            TableId::Prov => "prov",
            TableId::ProvIx => "prov_ix",
        }
    }

    pub(crate) fn def(self) -> TableDefinition<'static, &'static [u8], &'static [u8]> {
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
    let hi = prefix_end(prefix);
    let it = match &hi {
        Some(e) => t.range(prefix..e.as_slice()),
        None => t.range(prefix..),
    }
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
    let it = match &hi {
        Some(e) => t.range(prefix..e.as_slice()),
        None => t.range(prefix..),
    }
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
    /// The dictionary ids this reader has resolved so far.
    #[doc(hidden)]
    fn memo(&self) -> &dict::Memo;
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

    /// The vocabulary id of `s`; `None` when no row ever used it.
    #[doc(hidden)]
    fn vid_of(&self, s: &str) -> Result<Option<Vid>, StoreError> {
        dict::vid_of(self, s)
    }

    /// The string of vocabulary id `id`.
    #[doc(hidden)]
    fn vocab_str(&self, id: Vid) -> Result<String, StoreError> {
        dict::vocab_str(self, id)
    }

    fn artifact(&self, id: Id) -> Result<Option<ArtifactRow>, StoreError> {
        self.raw_get(TableId::Artifacts, &id.0)?
            .map(|v| dec_artifact(self, id, &v))
            .transpose()
    }

    fn artifacts(&self) -> Result<Vec<(Id, ArtifactRow)>, StoreError> {
        self.raw_scan(TableId::Artifacts, &[])?
            .into_iter()
            .map(|(k, v)| {
                let id = Id(id_bytes(&k)?);
                Ok((id, dec_artifact(self, id, &v)?))
            })
            .collect()
    }

    fn symbol(&self, id: Id) -> Result<Option<SymbolRow>, StoreError> {
        self.raw_get(TableId::Symbols, &id.0)?
            .map(|v| dec_symbol(self, &v))
            .transpose()
    }

    fn symbols(&self) -> Result<Vec<(Id, SymbolRow)>, StoreError> {
        self.raw_scan(TableId::Symbols, &[])?
            .into_iter()
            .map(|(k, v)| Ok((Id(id_bytes(&k)?), dec_symbol(self, &v)?)))
            .collect()
    }

    /// Every symbol's core, sorted by id bytes, the order of [`Reader::symbols`] (FEAT-076).
    fn symbol_cores(&self) -> Result<Vec<(Id, SymbolCore)>, StoreError> {
        let mut out = Vec::new();
        for (k, v) in self.raw_scan(TableId::SymbolCore, &[])? {
            let id = Id(id_bytes(
                k.get(k.len().saturating_sub(16)..).unwrap_or(&[]),
            )?);
            out.push((id, core_decode(&v)?));
        }
        out.sort_by_key(|(id, _)| *id);
        Ok(out)
    }

    /// Ids of the symbols whose lowercased plain name equals `lower`, in id order.
    fn symbol_ids_by_plain_ci(&self, lower: &str) -> Result<Vec<Id>, StoreError> {
        let mut prefix = Vec::new();
        lp(&mut prefix, lower);
        self.raw_scan_keys(TableId::SymbolCore, &prefix)?
            .iter()
            .map(|k| Ok(Id(id_bytes(&k[prefix.len()..])?)))
            .collect()
    }

    /// Ids of the symbols whose lowercased plain name contains `lower`. Scans keys only.
    fn symbol_ids_plain_contains(&self, lower: &str) -> Result<Vec<Id>, StoreError> {
        let mut out = Vec::new();
        for k in self.raw_scan_keys(TableId::SymbolCore, &[])? {
            let (plain, rest) = split_lp(&k)?;
            if plain.contains(lower) {
                out.push(Id(id_bytes(rest)?));
            }
        }
        Ok(out)
    }

    /// The stored top of the rank, or `None` when absent. The caller checks
    /// [`Reader::rank_is_current`].
    fn rank_top(&self) -> Result<Option<RankTop>, StoreError> {
        let Some(v) = self.meta(RANK_TOP_META)? else {
            return Ok(None);
        };
        let bad = || StoreError::Row("bad rank_top".into());
        if v.len() < 8 || (v.len() - 8) % 24 != 0 {
            return Err(bad());
        }
        let total = u64::from_le_bytes(v[..8].try_into().expect("8 bytes"));
        let entries = v[8..]
            .as_chunks::<24>()
            .0
            .iter()
            .map(|c| {
                let id = Id(c[..16].try_into().expect("16 bytes"));
                let bits = u64::from_le_bytes(c[16..].try_into().expect("8 bytes"));
                (id, f64::from_bits(bits))
            })
            .collect();
        Ok(Some(RankTop { total, entries }))
    }

    /// Calls `f(from, kind, to, weight, strong)` once per link row, in [`Reader::links_all`]
    /// order, without decoding spans or provenance. `weight` equals [`crate::row_weight`] and
    /// `strong` the analytics row strength of the expanded row.
    fn for_each_link(&self, f: &mut dyn FnMut(Id, &str, Id, f64, bool)) -> Result<(), StoreError> {
        let (kinds, mut stored) = self.stored_briefs()?;
        let rank = kind_ranks(&kinds);
        stored.sort_by_key(|s| (s.0, rank[s.1 as usize], s.2));
        let fans = self.raw_scan(TableId::FanOut, &[])?;
        walk_links(self, &kinds, &stored, &fans, f)
    }

    /// Every stored link row as `(from, kind index, to, weight, strong)` in key order of the
    /// file, with the kind strings the indexes refer to. The one walk of `links_out` that the
    /// link walk and the rank edges share; it decodes no span and no provenance.
    #[doc(hidden)]
    fn stored_briefs(&self) -> Result<(Vec<String>, Vec<StoredBrief>), StoreError> {
        let mut kinds: Vec<String> = Vec::new();
        let mut index: HashMap<Vid, u32> = HashMap::new();
        let mut out = Vec::new();
        for (k, v) in self.raw_scan(TableId::LinksOut, &[])? {
            let (from, vid, to) = decode_link_key(&k)?;
            let ki = match index.get(&vid) {
                Some(i) => *i,
                None => {
                    let i = kinds.len() as u32;
                    kinds.push(self.vocab_str(vid)?);
                    index.insert(vid, i);
                    i
                }
            };
            let (w, s) = link_row_brief(&v, None)?;
            out.push((from, ki, to, w, s));
        }
        Ok((kinds, out))
    }

    /// The distinct kinds in `links_out` and `fan_out` keys, decoding no value.
    fn link_kinds(&self) -> Result<BTreeSet<String>, StoreError> {
        let mut out = BTreeSet::new();
        let mut vids: BTreeSet<Vid> = BTreeSet::new();
        for k in self.raw_scan_keys(TableId::LinksOut, &[])? {
            vids.insert(decode_link_key(&k)?.1);
        }
        for v in vids {
            out.insert(self.vocab_str(v)?);
        }
        let mut last: Option<Vec<u8>> = None;
        for k in self.raw_scan_keys(TableId::FanOut, &[])? {
            let (kind, _) = split_lp(
                k.get(16..)
                    .ok_or_else(|| StoreError::Row("malformed key".into()))?,
            )?;
            if last.as_deref() != Some(kind.as_bytes()) {
                out.insert(kind.to_string());
                last = Some(kind.as_bytes().to_vec());
            }
        }
        Ok(out)
    }

    /// Calls `f(from, kind, ref_index, resolved)` for every entry that came from a reference,
    /// stored or fan row, per fan row and not per target. A fan row whose key has no target
    /// calls nothing.
    fn for_each_ref_entry(&self, f: &mut dyn FnMut(Id, &str, u32, bool)) -> Result<(), StoreError> {
        let mut kinds: HashMap<Vid, String> = HashMap::new();
        let mut stored: Vec<(Id, Vid, u32, bool)> = Vec::new();
        for (k, v) in self.raw_scan(TableId::LinksOut, &[])? {
            let (from, vid, _) = decode_link_key(&k)?;
            for e in codec::link_brief(&v)? {
                if let LinkOrigin::Ref(i) = e.origin {
                    stored.push((from, vid, i, entry_strong(e.confidence, e.fanout)));
                }
            }
        }
        for (_, vid, _, _) in &stored {
            if !kinds.contains_key(vid) {
                kinds.insert(*vid, self.vocab_str(*vid)?);
            }
        }
        // The order of the schema-9 key: `from`, then the kind's length-prefixed bytes.
        stored.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| kind_cmp(&kinds[&a.1], &kinds[&b.1]))
        });
        for (from, vid, i, strong) in stored {
            f(from, &kinds[&vid], i, strong);
        }
        let mut counts: HashMap<ExportKey, usize> = HashMap::new();
        for (k, v) in self.raw_scan(TableId::FanOut, &[])? {
            let fk = fan::decode_fan_out_key(&k)?;
            let n = match counts.get(&fk.key) {
                Some(n) => *n,
                None => {
                    let n = self.export_count(&fk.key)?;
                    counts.insert(fk.key.clone(), n);
                    n
                }
            };
            if n == 0 {
                continue;
            }
            let n = u32::try_from(n).unwrap_or(u32::MAX);
            for e in codec::link_brief(&v)? {
                if let LinkOrigin::Ref(i) = e.origin {
                    f(fk.from, &fk.kind.0, i, entry_strong(e.confidence, Some(n)));
                }
            }
        }
        Ok(())
    }

    /// The stored `links_out` row of `key`, without fan rows (FEAT-077). The stitcher uses only
    /// the stored-only reads.
    fn stored_link(&self, key: &LinkKey) -> Result<Option<LinkRow>, StoreError> {
        let Some(vid) = self.vid_of(&key.kind.0)? else {
            return Ok(None);
        };
        self.raw_get(TableId::LinksOut, &link_out_key(key.from, vid, key.to))?
            .map(|v| dec_link(self, &v))
            .transpose()
    }

    /// Stored rows leaving `from` (optionally of one kind), sorted by key bytes; no fan rows.
    fn stored_links_out(
        &self,
        from: Id,
        kind: Option<&LinkKind>,
    ) -> Result<Vec<(LinkKey, LinkRow)>, StoreError> {
        let prefix = match kind {
            Some(k) => match self.vid_of(&k.0)? {
                Some(v) => link_prefix(from, Some(v)),
                None => return Ok(Vec::new()),
            },
            None => link_prefix(from, None),
        };
        let mut out: Vec<(LinkKey, LinkRow)> = self
            .raw_scan(TableId::LinksOut, &prefix)?
            .into_iter()
            .map(|(k, v)| Ok((self.link_key_of(&k)?, dec_link(self, &v)?)))
            .collect::<Result<_, StoreError>>()?;
        if kind.is_none() {
            out.sort_by(|a, b| link_cmp(&a.0, &b.0));
        }
        Ok(out)
    }

    /// The key of a `links_out` row.
    #[doc(hidden)]
    fn link_key_of(&self, k: &[u8]) -> Result<LinkKey, StoreError> {
        let (from, vid, to) = decode_link_key(k)?;
        Ok(LinkKey {
            from,
            kind: LinkKind(self.vocab_str(vid)?),
            to,
        })
    }

    /// [`Reader::symbol`] for many ids, in order; the reads share one table lookup.
    fn symbols_many(&self, ids: &[Id]) -> Result<Vec<Option<SymbolRow>>, StoreError> {
        let keys: Vec<Vec<u8>> = ids.iter().map(|i| i.0.to_vec()).collect();
        self.raw_get_many(TableId::Symbols, &keys)?
            .into_iter()
            .map(|v| v.map(|v| dec_symbol(self, &v)).transpose())
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
                    let (name, facts) = codec::symbol_name_facts(v)?;
                    Ok(symbol_brief(&name.canonical(), facts))
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
            .map(|v| dec_link(self, &v))
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
            .map(|(k, v)| Ok((fan::decode_fan_out_key(&k)?, dec_link(self, &v)?)))
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
            .map(|(k, v)| Ok((fan::decode_fan_out_key(&k)?, dec_link(self, &v)?)))
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
            let n = self.export_count(&fk.key)?;
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
            let n = self.export_count(&fk.key)?;
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
        let mut stored: Vec<(LinkKey, LinkRow)> = self
            .raw_scan(TableId::LinksOut, &[])?
            .into_iter()
            .map(|(k, v)| Ok((self.link_key_of(&k)?, dec_link(self, &v)?)))
            .collect::<Result<_, StoreError>>()?;
        stored.sort_by(|a, b| link_cmp(&a.0, &b.0));
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
        let stored: Vec<LinkKey> = match self.vid_of(&kind.0)? {
            Some(v) => self
                .raw_scan_keys(TableId::LinksOut, &link_prefix(from, Some(v)))?
                .iter()
                .map(|k| self.link_key_of(k))
                .collect::<Result<_, _>>()?,
            None => Vec::new(),
        };
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
        keys.sort_by_key(|k| k.to);
        keys.dedup();
        Ok(keys)
    }

    /// [`Reader::link_keys_out`] for many sources, in order.
    fn link_keys_out_many(
        &self,
        froms: &[Id],
        kind: &LinkKind,
    ) -> Result<Vec<Vec<LinkKey>>, StoreError> {
        let vid = self.vid_of(&kind.0)?;
        // Without a vocabulary id no stored row has this kind: the empty prefix list is skipped.
        let stored = match vid {
            Some(v) => {
                let prefixes: Vec<Vec<u8>> =
                    froms.iter().map(|f| link_prefix(*f, Some(v))).collect();
                self.raw_scan_keys_many(TableId::LinksOut, &prefixes)?
            }
            None => vec![Vec::new(); froms.len()],
        };
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
                .map(|k| self.link_key_of(k))
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
                let mut ks: Vec<LinkKey> = keys
                    .iter()
                    .map(|k| self.link_in_key_of(k))
                    .collect::<Result<_, _>>()?;
                ks.sort_by(|a, b| kind_cmp(&a.kind.0, &b.kind.0).then_with(|| a.from.cmp(&b.from)));
                Ok(ks)
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
        // A kind without a vocabulary id has no stored row: an empty key never matches.
        let mut raw: Vec<Vec<u8>> = Vec::with_capacity(keys.len());
        for k in keys {
            raw.push(match self.vid_of(&k.kind.0)? {
                Some(v) => link_out_key(k.from, v, k.to),
                None => Vec::new(),
            });
        }
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
        let mut extra: HashMap<usize, Vec<EntryBrief>> = HashMap::new();
        let mut rows: HashMap<FanKey, Vec<EntryBrief>> = HashMap::new();
        let mut counts: HashMap<ExportKey, usize> = HashMap::new();
        for ((i, fk), h) in probes.iter().zip(hit) {
            if h.is_none() {
                continue;
            }
            if !rows.contains_key(fk) {
                let raw = self
                    .raw_get(TableId::FanOut, &fan::fan_out_key(fk))?
                    .ok_or_else(|| StoreError::Row("fan_in without fan_out".into()))?;
                rows.insert(
                    fk.clone(),
                    codec::link_brief(&raw)?
                        .into_iter()
                        .map(|e| EntryBrief { fanout: None, ..e })
                        .collect(),
                );
            }
            if !counts.contains_key(&fk.key) {
                counts.insert(fk.key.clone(), self.export_count(&fk.key)?);
            }
            let n = u32::try_from(counts[&fk.key]).unwrap_or(u32::MAX);
            extra
                .entry(*i)
                .or_default()
                .extend(rows[fk].iter().map(|e| EntryBrief {
                    fanout: Some(n),
                    ..*e
                }));
        }
        let mut out = Vec::with_capacity(keys.len());
        for (i, v) in stored.into_iter().enumerate() {
            let mut entries: Option<Vec<EntryBrief>> = match v {
                Some(v) => Some(codec::link_brief(&v)?),
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
        let prefix = postings::prefix(term);
        let mut entries: Vec<postings::Entry> = Vec::new();
        for (k, v) in self.raw_scan(TableId::Postings, &prefix)? {
            if k.len() != prefix.len() + 4 {
                return Err(StoreError::Row("malformed posting".into()));
            }
            let base = postings::block_of_key(&k)? * postings::POSTING_BLOCK;
            entries.extend(postings::decode_block(base, &v)?);
        }
        let mut out = resolve_postings(self, &entries)?;
        out.sort_by_key(|p| (p.field, p.symbol));
        Ok(out)
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
        let mut by_term: BTreeMap<String, (u64, Vec<postings::Entry>)> = BTreeMap::new();
        for (k, v) in self.raw_scan(TableId::Postings, prefix.as_bytes())? {
            let nul = k.iter().position(|b| *b == 0).ok_or_else(bad)?;
            if k.len() != nul + 5 {
                return Err(bad());
            }
            if nul == prefix.len() {
                continue;
            }
            let term = String::from_utf8(k[..nul].to_vec()).map_err(|_| bad())?;
            let base = postings::block_of_key(&k)? * postings::POSTING_BLOCK;
            let entry = by_term.entry(term).or_default();
            for e in postings::decode_block(base, &v)? {
                entry.0 += u64::from(e.2);
                entry.1.push(e);
            }
        }
        let mut terms: Vec<(String, (u64, Vec<postings::Entry>))> = by_term.into_iter().collect();
        terms.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(&b.0)));
        terms.truncate(limit);
        // One lookup of the documents of every selected term.
        let all: Vec<postings::Entry> = terms
            .iter()
            .flat_map(|(_, (_, e))| e.iter().copied())
            .collect();
        let resolved = resolve_postings(self, &all)?;
        let mut at = 0;
        let mut out = Vec::with_capacity(terms.len());
        for (term, (_, entries)) in terms {
            let mut ps = resolved[at..at + entries.len()].to_vec();
            at += entries.len();
            ps.sort_by_key(|p| (p.field, p.symbol));
            out.push((term, ps));
        }
        Ok(out)
    }

    /// Per-field token counts of one indexed symbol.
    fn doc_len(&self, id: Id) -> Result<Option<[u32; LEXICON_FIELDS]>, StoreError> {
        let Some(ord) = self.raw_get(TableId::DocOrd, &id.0)? else {
            return Ok(None);
        };
        let ord = postings::decode_ord(&ord)?;
        let row = self
            .raw_get(TableId::DocByOrd, &ord.to_be_bytes())?
            .ok_or_else(|| StoreError::Row("doc_ord without doc_by_ord".into()))?;
        Ok(Some(postings::decode_doc(&row)?.1))
    }

    fn lexicon_stats(&self) -> Result<LexiconStats, StoreError> {
        match self.raw_get(TableId::LexiconStats, STATS_KEY)? {
            Some(v) => decode_stats(&v),
            None => Ok(LexiconStats::default()),
        }
    }

    /// Keys of the stored rows arriving at `to`, in `links_in` key order; no fan rows.
    fn stored_links_in(&self, to: Id) -> Result<Vec<LinkKey>, StoreError> {
        let mut out: Vec<LinkKey> = self
            .raw_scan_keys(TableId::LinksIn, &to.0)?
            .iter()
            .map(|k| self.link_in_key_of(k))
            .collect::<Result<_, _>>()?;
        out.sort_by(|a, b| kind_cmp(&a.kind.0, &b.kind.0).then_with(|| a.from.cmp(&b.from)));
        Ok(out)
    }

    /// The link a `links_in` key stands for, in link direction.
    #[doc(hidden)]
    fn link_in_key_of(&self, k: &[u8]) -> Result<LinkKey, StoreError> {
        let (to, vid, from) = decode_link_key(k)?;
        Ok(LinkKey {
            from,
            kind: LinkKind(self.vocab_str(vid)?),
            to,
        })
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

    /// The raw `exports` row of `key`, if stored.
    #[doc(hidden)]
    fn exports_raw(&self, key: &ExportKey) -> Result<Option<Vec<u8>>, StoreError> {
        match self.vid_of(&key.namespace)? {
            Some(ns) => self.raw_get(TableId::Exports, &export_key(ns, &key.key)),
            None => Ok(None),
        }
    }

    /// Targets exported under `key` (empty when none), sorted.
    fn exports(&self, key: &ExportKey) -> Result<Vec<Id>, StoreError> {
        Ok(self
            .exports_raw(key)?
            .map(|v| de(&v))
            .transpose()?
            .unwrap_or_default())
    }

    /// The number of targets exported under `key`, without decoding the ids.
    fn export_count(&self, key: &ExportKey) -> Result<usize, StoreError> {
        match self.exports_raw(key)? {
            Some(v) => codec::id_list_len(&v),
            None => Ok(0),
        }
    }

    /// Every `(key, targets)` in a namespace.
    fn exports_in(&self, namespace: &str) -> Result<Vec<(ExportKey, Vec<Id>)>, StoreError> {
        let Some(ns) = self.vid_of(namespace)? else {
            return Ok(Vec::new());
        };
        let prefix = export_prefix(ns);
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
        self.ref_rows(TableId::RefsByKey, key)
    }

    /// The references with a contract binding that name `key`, in key order.
    fn bound_refs_by_key(&self, key: &ExportKey) -> Result<Vec<RefId>, StoreError> {
        self.ref_rows(TableId::BoundRefsByKey, key)
    }

    #[doc(hidden)]
    fn ref_rows(&self, t: TableId, key: &ExportKey) -> Result<Vec<RefId>, StoreError> {
        let Some(ns) = self.vid_of(&key.namespace)? else {
            return Ok(Vec::new());
        };
        let prefix = ref_by_key_prefix(ns, &key.key);
        self.raw_scan_keys(t, &prefix)?
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

    /// The unresolved reference `r`, read from its artifact row.
    fn unresolved(&self, r: RefId) -> Result<Option<OutRef>, StoreError> {
        if self
            .raw_get(TableId::RefsUnresolved, &ref_key(r))?
            .is_none()
        {
            return Ok(None);
        }
        let art = self
            .artifact(r.artifact)?
            .ok_or_else(|| StoreError::Row("unresolved ref without artifact row".into()))?;
        art.refs
            .get(r.index as usize)
            .cloned()
            .map(Some)
            .ok_or_else(|| StoreError::Row("unresolved ref without artifact row".into()))
    }

    /// Unresolved references of one artifact (`Some`) or of all (`None`).
    fn unresolved_refs(&self, artifact: Option<Id>) -> Result<Vec<(RefId, OutRef)>, StoreError> {
        let prefix = artifact.map(|a| a.0.to_vec()).unwrap_or_default();
        let keys: Vec<RefId> = self
            .raw_scan_keys(TableId::RefsUnresolved, &prefix)?
            .iter()
            .map(|k| decode_ref_key(k))
            .collect::<Result<_, _>>()?;
        let mut out = Vec::with_capacity(keys.len());
        let mut current: Option<(Id, ArtifactRow)> = None;
        for r in keys {
            if current.as_ref().is_none_or(|(id, _)| *id != r.artifact) {
                let row = self
                    .artifact(r.artifact)?
                    .ok_or_else(|| StoreError::Row("unresolved ref without artifact row".into()))?;
                current = Some((r.artifact, row));
            }
            let row = &current.as_ref().expect("set above").1;
            let o = row
                .refs
                .get(r.index as usize)
                .cloned()
                .ok_or_else(|| StoreError::Row("unresolved ref without artifact row".into()))?;
            out.push((r, o));
        }
        Ok(out)
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

    /// Every graph table as its logical rows, in a fixed table order: keys in the schema-9
    /// encoding (length-prefixed strings) and values as compact JSON of the logical rows, sorted
    /// by those key bytes. Two caches that hold the same graph give equal dumps, whatever ids
    /// their dictionaries assigned.
    fn dump_graph(&self) -> Result<Vec<(&'static str, RawRows)>, StoreError> {
        fn js<T: Serialize>(v: &T) -> Result<Vec<u8>, StoreError> {
            serde_json::to_vec(v).map_err(|e| StoreError::Row(e.to_string()))
        }
        fn lp_ns_key(ns: &str, key: &[u8]) -> Vec<u8> {
            let mut k = Vec::new();
            lp(&mut k, ns);
            k.extend_from_slice(key);
            k
        }
        let mut out = Vec::new();
        for t in TableId::GRAPH {
            let mut rows: RawRows = Vec::new();
            match t {
                TableId::Artifacts => {
                    for (id, row) in self.artifacts()? {
                        rows.push((id.0.to_vec(), js(&row)?));
                    }
                }
                TableId::Symbols => {
                    for (id, row) in self.symbols()? {
                        rows.push((id.0.to_vec(), js(&row)?));
                    }
                }
                TableId::LinksOut => {
                    for (k, v) in self.raw_scan(t, &[])? {
                        let key = self.link_key_of(&k)?;
                        rows.push((link_order_key(&key), js(&dec_link(self, &v)?)?));
                    }
                }
                TableId::LinksIn => {
                    for k in self.raw_scan_keys(t, &[])? {
                        let key = self.link_in_key_of(&k)?;
                        let mirror = LinkKey {
                            from: key.to,
                            kind: key.kind,
                            to: key.from,
                        };
                        rows.push((link_order_key(&mirror), Vec::new()));
                    }
                }
                TableId::Exports => {
                    for (k, v) in self.raw_scan(t, &[])? {
                        let ns = Vid::from_be_bytes(
                            k.get(..2)
                                .and_then(|b| b.try_into().ok())
                                .ok_or_else(|| StoreError::Row("malformed key".into()))?,
                        );
                        let targets: Vec<Id> = de(&v)?;
                        rows.push((lp_ns_key(&self.vocab_str(ns)?, &k[2..]), js(&targets)?));
                    }
                }
                TableId::RefsByKey | TableId::BoundRefsByKey => {
                    for k in self.raw_scan_keys(t, &[])? {
                        let ns = Vid::from_be_bytes(
                            k.get(..2)
                                .and_then(|b| b.try_into().ok())
                                .ok_or_else(|| StoreError::Row("malformed key".into()))?,
                        );
                        rows.push((lp_ns_key(&self.vocab_str(ns)?, &k[2..]), Vec::new()));
                    }
                }
                TableId::RefsUnresolved => {
                    for (r, o) in self.unresolved_refs(None)? {
                        rows.push((ref_key(r), js(&o)?));
                    }
                }
                TableId::IndexErrors => {
                    for (k, v) in self.raw_scan(t, &[])? {
                        let e: Vec<ErrorRow> = de(&v)?;
                        rows.push((k, js(&e)?));
                    }
                }
                TableId::Contracts => {
                    for (id, row) in self.contracts()? {
                        rows.push((id.0.to_vec(), js(&row)?));
                    }
                }
                TableId::BindingsByArtifact => {
                    for (k, v) in self.raw_scan(t, &[])? {
                        let b: Vec<BindingRow> = de(&v)?;
                        rows.push((k, js(&b)?));
                    }
                }
                TableId::FanOut => {
                    for (k, v) in self.raw_scan(t, &[])? {
                        rows.push((k, js(&dec_link(self, &v)?)?));
                    }
                }
                _ => rows = self.raw_scan(t, &[])?,
            }
            rows.sort_by(|a, b| a.0.cmp(&b.0));
            out.push((t.name(), rows));
        }
        Ok(out)
    }
}

/// A decoded artifact row (`id` is the artifact's own id).
fn dec_artifact<R: Reader + ?Sized>(tx: &R, id: Id, v: &[u8]) -> Result<ArtifactRow, StoreError> {
    codec::artifact_from_wire(id, codec::dec(v)?, &dict::TxRes(tx))
}

fn dec_symbol<R: Reader + ?Sized>(tx: &R, v: &[u8]) -> Result<SymbolRow, StoreError> {
    codec::symbol_from_wire(codec::dec(v)?, &dict::TxRes(tx))
}

fn dec_link<R: Reader + ?Sized>(tx: &R, v: &[u8]) -> Result<LinkRow, StoreError> {
    codec::link_from_wire(codec::dec(v)?, &dict::TxRes(tx))
}

/// One stored link row, briefly: `(from, kind index, to, weight, strong)`.
#[doc(hidden)]
pub type StoredBrief = (Id, u32, Id, f64, bool);

/// The rank of each kind in the order of the kinds' length-prefixed bytes.
fn kind_ranks(kinds: &[String]) -> Vec<u32> {
    let mut order: Vec<usize> = (0..kinds.len()).collect();
    order.sort_by(|a, b| kind_cmp(&kinds[*a], &kinds[*b]));
    let mut rank = vec![0u32; kinds.len()];
    for (pos, i) in order.into_iter().enumerate() {
        rank[i] = pos as u32;
    }
    rank
}

/// The postings of `entries` (in the order given), with their symbols and lengths filled in from
/// `doc_by_ord`; one lookup per distinct ordinal.
fn resolve_postings<R: Reader + ?Sized>(
    tx: &R,
    entries: &[postings::Entry],
) -> Result<Vec<Posting>, StoreError> {
    let mut ords: Vec<u32> = entries.iter().map(|e| e.0).collect();
    ords.sort_unstable();
    ords.dedup();
    let keys: Vec<Vec<u8>> = ords.iter().map(|o| o.to_be_bytes().to_vec()).collect();
    let rows = tx.raw_get_many(TableId::DocByOrd, &keys)?;
    let mut docs: HashMap<u32, (Id, [u32; LEXICON_FIELDS])> = HashMap::with_capacity(ords.len());
    for (ord, row) in ords.iter().zip(rows) {
        let row = row.ok_or_else(|| StoreError::Row("posting without document".into()))?;
        docs.insert(*ord, postings::decode_doc(&row)?);
    }
    Ok(entries
        .iter()
        .map(|(ord, field, tf)| {
            let (symbol, doc_len) = docs[ord];
            Posting {
                field: *field,
                symbol,
                tf: *tf,
                doc_len,
            }
        })
        .collect())
}

/// Whether one link entry is strong: not inferred, and its fan-out at most one.
fn entry_strong(confidence: Confidence, fanout: Option<u32>) -> bool {
    confidence != Confidence::Inferred && fanout.is_none_or(|n| n <= 1)
}

/// The weight and strength of one encoded link row, without spans or provenance: the largest
/// [`crate::link_weight`] of its entries and whether any entry is strong, each entry's fan-out
/// replaced by `fanout_override` when given. The only row decoder the link walk uses.
fn link_row_brief(v: &[u8], fanout_override: Option<u32>) -> Result<(f64, bool), StoreError> {
    let mut weight = 0.0f64;
    let mut strong = false;
    for e in &codec::link_brief(v)? {
        let fanout = fanout_override.or(e.fanout);
        let w = match (e.confidence, fanout) {
            (Confidence::Inferred, Some(n)) if n > 1 => 1.0 / f64::from(n),
            _ => 1.0,
        };
        weight = weight.max(w);
        strong |= entry_strong(e.confidence, fanout);
    }
    Ok((weight, strong))
}

/// The `from || lp(kind)` prefix of a `links_out` or `fan_out` key.
fn group_prefix(k: &[u8]) -> Result<&[u8], StoreError> {
    let bad = || StoreError::Row("malformed key".into());
    let len = u32::from_be_bytes(k.get(16..20).ok_or_else(bad)?.try_into().expect("4 bytes"));
    k.get(..20 + len as usize).ok_or_else(bad)
}

/// Merges the stored link briefs (sorted by `(from, kind rank, to)`) and the raw `fan_out` rows
/// (in key order) by `(from, kind)` group, in [`Reader::links_all`] order.
fn walk_links(
    tx: &(impl Reader + ?Sized),
    kinds: &[String],
    stored: &[StoredBrief],
    fans: &[(Vec<u8>, Vec<u8>)],
    f: &mut dyn FnMut(Id, &str, Id, f64, bool),
) -> Result<(), StoreError> {
    let (mut si, mut fi) = (0usize, 0usize);
    let mut targets: HashMap<ExportKey, Vec<Id>> = HashMap::new();
    let mut group: HashMap<Id, (f64, bool)> = HashMap::new();
    let stored_prefix = |s: &StoredBrief| {
        let mut p = s.0.0.to_vec();
        lp(&mut p, &kinds[s.1 as usize]);
        p
    };
    while si < stored.len() || fi < fans.len() {
        let sp = stored.get(si).map(stored_prefix);
        let fp = fans.get(fi).map(|(k, _)| group_prefix(k)).transpose()?;
        let (prefix, from_stored): (Vec<u8>, bool) = match (sp, fp) {
            (Some(a), Some(b)) => {
                if a.as_slice() > b {
                    (b.to_vec(), false)
                } else {
                    (a, true)
                }
            }
            (Some(a), None) => (a, true),
            (None, Some(b)) => (b.to_vec(), false),
            (None, None) => break,
        };
        let s_end = if from_stored {
            si + stored[si..]
                .iter()
                .take_while(|s| s.0 == stored[si].0 && s.1 == stored[si].1)
                .count()
        } else {
            si
        };
        let from = Id(id_bytes(&prefix[..16])?);
        let (kind, _) = split_lp(&prefix[16..])?;
        let f_end = fi
            + fans[fi..]
                .iter()
                .take_while(|(k, _)| k.starts_with(&prefix))
                .count();
        if fi == f_end {
            for (_, _, to, w, s) in &stored[si..s_end] {
                f(from, kind, *to, *w, *s);
            }
        } else {
            group.clear();
            for (_, _, to, w, s) in &stored[si..s_end] {
                group.insert(*to, (*w, *s));
            }
            for (k, v) in &fans[fi..f_end] {
                let key = fan::decode_fan_out_key(k)?.key;
                if !targets.contains_key(&key) {
                    let t = tx.exports(&key)?;
                    targets.insert(key.clone(), t);
                }
                let ts = &targets[&key];
                let n = u32::try_from(ts.len()).unwrap_or(u32::MAX);
                let (w, s) = link_row_brief(v, Some(n))?;
                for to in ts {
                    let e = group.entry(*to).or_insert((w, s));
                    e.0 = e.0.max(w);
                    e.1 |= s;
                }
            }
            let mut ids: Vec<Id> = group.keys().copied().collect();
            ids.sort();
            for to in ids {
                let (w, s) = group[&to];
                f(from, kind, to, w, s);
            }
        }
        si = s_end;
        fi = f_end;
    }
    Ok(())
}

fn id_bytes(k: &[u8]) -> Result<[u8; 16], StoreError> {
    k.try_into()
        .map_err(|_| StoreError::Row("malformed key".into()))
}

/// MVCC snapshot of the cache.
pub struct ReadTx<'a> {
    txn: ReadTransaction,
    memo: dict::Memo,
    _cache: PhantomData<&'a ()>,
}

impl ReadTx<'_> {
    /// A snapshot over a transaction from any database handle (the read-only inspection uses
    /// one from a `ReadOnlyDatabase`).
    pub(crate) fn from_txn(txn: ReadTransaction) -> Self {
        ReadTx {
            txn,
            memo: dict::Memo::default(),
            _cache: PhantomData,
        }
    }
}

/// The engine's name for a read view of the graph.
pub type Snapshot<'a> = ReadTx<'a>;

impl Reader for ReadTx<'_> {
    fn memo(&self) -> &dict::Memo {
        &self.memo
    }

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
    /// Dictionary ids resolved or assigned in this transaction.
    memo: dict::Memo,
    /// Provenance reference counts to apply at commit.
    prov_delta: BTreeMap<Pid, i64>,
    /// Posting writes not yet applied; flushed before a postings read and at commit.
    postings: Mutex<PostingBuf>,
    /// Ordinals given up by `remove_doc_postings` in this transaction, until commit or reuse.
    released: BTreeMap<Id, u32>,
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
    fn memo(&self) -> &dict::Memo {
        &self.memo
    }

    fn raw_get(&self, t: TableId, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        if t == TableId::Postings {
            self.flush_postings()?;
        }
        get_in(&self.txn.open_table(t.def()).map_err(db)?, key)
    }

    fn raw_scan(&self, t: TableId, prefix: &[u8]) -> Result<RawRows, StoreError> {
        if t == TableId::Postings {
            self.flush_postings()?;
        }
        scan_in(&self.txn.open_table(t.def()).map_err(db)?, prefix)
    }

    fn raw_scan_keys(&self, t: TableId, prefix: &[u8]) -> Result<Vec<Vec<u8>>, StoreError> {
        if t == TableId::Postings {
            self.flush_postings()?;
        }
        scan_keys_in(&self.txn.open_table(t.def()).map_err(db)?, prefix)
    }

    /// A symbol whose postings were removed in this transaction is not indexed, though its
    /// ordinal rows stay until commit.
    fn doc_len(&self, id: Id) -> Result<Option<[u32; LEXICON_FIELDS]>, StoreError> {
        if self.released.contains_key(&id) {
            return Ok(None);
        }
        let Some(ord) = self.raw_get(TableId::DocOrd, &id.0)? else {
            return Ok(None);
        };
        let ord = postings::decode_ord(&ord)?;
        let row = self
            .raw_get(TableId::DocByOrd, &ord.to_be_bytes())?
            .ok_or_else(|| StoreError::Row("doc_ord without doc_by_ord".into()))?;
        Ok(Some(postings::decode_doc(&row)?.1))
    }
}

impl WriteTx<'_> {
    fn new(txn: WriteTransaction) -> Self {
        WriteTx {
            txn,
            _cache: PhantomData,
            touched: RankTouched::default(),
            memo: dict::Memo::default(),
            prov_delta: BTreeMap::new(),
            postings: Mutex::new(PostingBuf::default()),
            released: BTreeMap::new(),
        }
    }

    /// Applies the buffered posting writes.
    fn flush_postings(&self) -> Result<(), StoreError> {
        let mut buf = self.postings.lock().unwrap_or_else(PoisonError::into_inner);
        postings::flush(&mut buf, &self.txn)
    }

    /// The interner of this transaction: assigns vocabulary and provenance ids, and counts the
    /// provenance references the rows it encodes will hold.
    fn dict(&mut self) -> dict::TxDict<'_> {
        dict::TxDict {
            txn: &self.txn,
            memo: &self.memo,
            delta: &mut self.prov_delta,
        }
    }

    /// Count `pids` (the provenance ids of a row about to be replaced or removed) as released.
    fn release_pids(&mut self, pids: Vec<Pid>) {
        for p in pids {
            *self.prov_delta.entry(p).or_insert(0) -= 1;
        }
    }

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
        if let Some(old) = self.raw_get(TableId::Artifacts, &id.0)? {
            let pid = codec::artifact_pid(&old)?;
            self.release_pids(vec![pid]);
        }
        let wire = codec::artifact_to_wire(id, row, &mut self.dict())?;
        self.put(TableId::Artifacts, &id.0, &codec::enc(&wire)?)
    }

    pub fn remove_artifact_row(&mut self, id: Id) -> Result<(), StoreError> {
        if let Some(old) = self.raw_get(TableId::Artifacts, &id.0)? {
            let pid = codec::artifact_pid(&old)?;
            self.release_pids(vec![pid]);
        }
        self.del(TableId::Artifacts, &id.0)
    }

    pub fn put_symbol(&mut self, id: Id, row: &SymbolRow) -> Result<(), StoreError> {
        self.touch_symbol(id)?;
        let wire = codec::symbol_to_wire(row, &mut self.dict())?;
        self.put(TableId::Symbols, &id.0, &codec::enc(&wire)?)?;
        self.put(TableId::SymbolCore, &core_key(row, id), &core_value(row))
    }

    /// Writes the symbol row without its `symbol_core` row, which leaves the cache inconsistent.
    /// Only for the benchmark that prices the core row.
    #[doc(hidden)]
    pub fn put_symbol_row_only(&mut self, id: Id, row: &SymbolRow) -> Result<(), StoreError> {
        self.touch_symbol(id)?;
        let wire = codec::symbol_to_wire(row, &mut self.dict())?;
        self.put(TableId::Symbols, &id.0, &codec::enc(&wire)?)
    }

    pub fn remove_symbol(&mut self, id: Id) -> Result<(), StoreError> {
        self.touch_symbol(id)?;
        // The core row is keyed by the plain name, so the symbol is read to build its key.
        if let Some(row) = self.symbol(id)? {
            self.del(TableId::SymbolCore, &core_key(&row, id))?;
        }
        self.del(TableId::Symbols, &id.0)
    }

    /// Writes `links_out` and the matching `links_in` entry.
    pub fn put_link(&mut self, key: &LinkKey, row: &LinkRow) -> Result<(), StoreError> {
        self.touch_link(key)?;
        if let Some(vid) = self.vid_of(&key.kind.0)?
            && let Some(old) =
                self.raw_get(TableId::LinksOut, &link_out_key(key.from, vid, key.to))?
        {
            let pids = codec::link_pids(&old)?;
            self.release_pids(pids);
        }
        let mut dict = self.dict();
        let vid = codec::Interner::vid(&mut dict, &key.kind.0)?;
        let wire = codec::link_to_wire(row, &mut dict)?;
        self.put(
            TableId::LinksOut,
            &link_out_key(key.from, vid, key.to),
            &codec::enc(&wire)?,
        )?;
        self.put(TableId::LinksIn, &link_out_key(key.to, vid, key.from), &[])
    }

    /// Removes `links_out` and the matching `links_in` entry.
    pub fn remove_link(&mut self, key: &LinkKey) -> Result<(), StoreError> {
        self.touch_link(key)?;
        let Some(vid) = self.vid_of(&key.kind.0)? else {
            return Ok(());
        };
        let out = link_out_key(key.from, vid, key.to);
        if let Some(old) = self.raw_get(TableId::LinksOut, &out)? {
            let pids = codec::link_pids(&old)?;
            self.release_pids(pids);
        }
        self.del(TableId::LinksOut, &out)?;
        self.del(TableId::LinksIn, &link_out_key(key.to, vid, key.from))
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
        if let Some(old) = self.raw_get(TableId::FanOut, &fan::fan_out_key(key))? {
            let pids = codec::link_pids(&old)?;
            self.release_pids(pids);
        }
        let wire = codec::link_to_wire(row, &mut self.dict())?;
        self.put(TableId::FanOut, &fan::fan_out_key(key), &codec::enc(&wire)?)?;
        self.put(TableId::FanIn, &fan::fan_in_key(key), &[])
    }

    /// Delete the fan row and its `fan_in` row; absent is fine.
    pub fn remove_fan(&mut self, key: &FanKey) -> Result<(), StoreError> {
        self.touch_fan(key)?;
        if let Some(old) = self.raw_get(TableId::FanOut, &fan::fan_out_key(key))? {
            let pids = codec::link_pids(&old)?;
            self.release_pids(pids);
        }
        self.del(TableId::FanOut, &fan::fan_out_key(key))?;
        self.del(TableId::FanIn, &fan::fan_in_key(key))
    }

    /// An empty set removes the row. Also keeps `exports_by_target`: a row `(t, key)` exists
    /// exactly when `t` is in `targets` and `targets` has two or more entries.
    pub fn put_exports(&mut self, key: &ExportKey, targets: &[Id]) -> Result<(), StoreError> {
        let old_raw = self.exports_raw(key)?;
        let old: Vec<Id> = match &old_raw {
            Some(v) => de(v)?,
            None => Vec::new(),
        };
        self.touched.exports.entry(key.clone()).or_insert(old_raw);
        let no_rows: &[Id] = &[];
        let old_rows = if old.len() >= 2 { &old[..] } else { no_rows };
        let new_rows = if targets.len() >= 2 { targets } else { no_rows };
        for t in old_rows
            .iter()
            .filter(|t| new_rows.binary_search(t).is_err())
        {
            self.del(TableId::ExportsByTarget, &fan::by_target_key(*t, key))?;
        }
        for t in new_rows
            .iter()
            .filter(|t| old_rows.binary_search(t).is_err())
        {
            self.put(TableId::ExportsByTarget, &fan::by_target_key(*t, key), &[])?;
        }
        if targets.is_empty() {
            match self.vid_of(&key.namespace)? {
                Some(ns) => self.del(TableId::Exports, &export_key(ns, &key.key)),
                None => Ok(()),
            }
        } else {
            let ns = codec::Interner::vid(&mut self.dict(), &key.namespace)?;
            self.put(TableId::Exports, &export_key(ns, &key.key), &ser(&targets)?)
        }
    }

    /// Insert the row `(key, r)` into `t`; idempotent.
    fn add_ref_row(&mut self, t: TableId, key: &ExportKey, r: RefId) -> Result<(), StoreError> {
        let ns = codec::Interner::vid(&mut self.dict(), &key.namespace)?;
        self.put(t, &ref_by_key_key(ns, &key.key, r), &[])
    }

    /// Delete the row `(key, r)` from `t`; absent is fine.
    fn remove_ref_row(&mut self, t: TableId, key: &ExportKey, r: RefId) -> Result<(), StoreError> {
        match self.vid_of(&key.namespace)? {
            Some(ns) => self.del(t, &ref_by_key_key(ns, &key.key, r)),
            None => Ok(()),
        }
    }

    /// Insert the row `(key, r)`; idempotent.
    pub fn add_ref_by_key(&mut self, key: &ExportKey, r: RefId) -> Result<(), StoreError> {
        self.add_ref_row(TableId::RefsByKey, key, r)
    }

    /// Insert the bound-reference row `(key, r)`; idempotent.
    pub fn add_bound_ref_by_key(&mut self, key: &ExportKey, r: RefId) -> Result<(), StoreError> {
        self.add_ref_row(TableId::BoundRefsByKey, key, r)
    }

    /// Delete the bound-reference row `(key, r)`; absent is fine.
    pub fn remove_bound_ref_by_key(&mut self, key: &ExportKey, r: RefId) -> Result<(), StoreError> {
        self.remove_ref_row(TableId::BoundRefsByKey, key, r)
    }

    /// Delete the row `(key, r)`; absent is fine.
    pub fn remove_ref_by_key(&mut self, key: &ExportKey, r: RefId) -> Result<(), StoreError> {
        self.remove_ref_row(TableId::RefsByKey, key, r)
    }

    /// Marks the reference unresolved. The row holds only the key: the reference itself is read
    /// from its artifact row, so `_out` is not stored.
    pub fn put_unresolved(&mut self, r: RefId, _out: &OutRef) -> Result<(), StoreError> {
        self.put(TableId::RefsUnresolved, &ref_key(r), &[])
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
    /// symbol is already indexed. The symbol keeps the ordinal it has (or had earlier in this
    /// transaction); a new one is the next unused number.
    pub fn put_doc_postings(
        &mut self,
        id: Id,
        lens: [u32; LEXICON_FIELDS],
        terms: &[(String, u8, u32)],
    ) -> Result<(), StoreError> {
        let ord = match self.released.remove(&id) {
            Some(ord) => ord,
            None => match self.raw_get(TableId::DocOrd, &id.0)? {
                Some(v) => postings::decode_ord(&v)?,
                None => {
                    let next = match self.meta(postings::DOC_NEXT_ORD_META)? {
                        Some(v) => u32::from_le_bytes(
                            v.as_slice()
                                .try_into()
                                .map_err(|_| StoreError::Row("bad doc_next_ord".into()))?,
                        ),
                        None => 0,
                    };
                    let after = next
                        .checked_add(1)
                        .ok_or_else(|| StoreError::Row("posting ordinals exhausted".into()))?;
                    self.set_meta(postings::DOC_NEXT_ORD_META, &after.to_le_bytes())?;
                    next
                }
            },
        };
        self.put(TableId::DocOrd, &id.0, &postings::encode_ord(ord))?;
        self.put(
            TableId::DocByOrd,
            &ord.to_be_bytes(),
            &postings::encode_doc(id, &lens),
        )?;
        {
            let mut buf = self.postings.lock().unwrap_or_else(PoisonError::into_inner);
            for (term, field, tf) in terms {
                buf.add(term, ord, *field, *tf);
            }
        }
        let mut stats = self.lexicon_stats()?;
        stats.docs += 1;
        for (t, l) in stats.total_len.iter_mut().zip(lens) {
            *t += u64::from(l);
        }
        self.put(TableId::LexiconStats, STATS_KEY, &encode_stats(&stats))
    }

    /// Remove the postings `(term, field)` of one symbol and its length row, subtracting the
    /// lengths from the lexicon statistics. A symbol that is not indexed changes nothing. The
    /// ordinal is given back at commit unless the symbol is indexed again first.
    pub fn remove_doc_postings(
        &mut self,
        id: Id,
        terms: &[(String, u8)],
    ) -> Result<(), StoreError> {
        let Some(lens) = self.doc_len(id)? else {
            return Ok(());
        };
        let ord = postings::decode_ord(
            &self
                .raw_get(TableId::DocOrd, &id.0)?
                .ok_or_else(|| StoreError::Row("doc_len without doc_ord".into()))?,
        )?;
        {
            let mut buf = self.postings.lock().unwrap_or_else(PoisonError::into_inner);
            for (term, _) in terms {
                buf.remove(term, ord);
            }
        }
        self.released.insert(id, ord);
        let mut stats = self.lexicon_stats()?;
        stats.docs = stats.docs.saturating_sub(1);
        for (t, l) in stats.total_len.iter_mut().zip(lens) {
            *t = t.saturating_sub(u64::from(l));
        }
        self.put(TableId::LexiconStats, STATS_KEY, &encode_stats(&stats))
    }

    /// Applies the buffered postings and gives back the ordinals nobody claimed again.
    fn finish_postings(&mut self) -> Result<(), StoreError> {
        self.flush_postings()?;
        for (id, ord) in std::mem::take(&mut self.released) {
            self.del(TableId::DocOrd, &id.0)?;
            self.del(TableId::DocByOrd, &ord.to_be_bytes())?;
        }
        Ok(())
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
            if self.exports_raw(key)? != *was
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
        // The head of the previous rank no longer matches; `store_rank_top` writes the new one.
        self.del(TableId::Meta, RANK_TOP_META.as_bytes())?;
        self.set_rank_watermark(watermark)
    }

    /// Store the head of the rank beside it (FEAT-076). Call after [`WriteTx::store_rank`], in
    /// the same transaction; a reader accepts it only while the rank is current.
    pub fn store_rank_top(&mut self, top: &RankTop) -> Result<(), StoreError> {
        let mut v = Vec::with_capacity(8 + top.entries.len() * 24);
        v.extend_from_slice(&top.total.to_le_bytes());
        for (id, score) in &top.entries {
            v.extend_from_slice(&id.0);
            v.extend_from_slice(&score.to_bits().to_le_bytes());
        }
        self.set_meta(RANK_TOP_META, &v)
    }

    /// Record that the stored rank reflects `watermark`.
    pub fn set_rank_watermark(&mut self, watermark: u64) -> Result<(), StoreError> {
        self.set_meta(RANK_WATERMARK_META, &watermark.to_le_bytes())
    }

    /// Commits. A transaction that changed the rank's inputs also drops the stored rank's
    /// watermark, so no reader can take a rank that does not match the graph it reads.
    pub fn commit(mut self) -> Result<(), StoreError> {
        self.finish_postings()?;
        dict::apply_prov_deltas(&self.txn, &self.prov_delta)?;
        if self.rank_inputs_changed()? {
            self.del(TableId::Meta, RANK_WATERMARK_META.as_bytes())?;
        }
        COMMITS.with(|c| c.set(c.get() + 1));
        self.txn.commit().map_err(db)
    }
}

thread_local! {
    static COMMITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many write transactions the calling thread has committed. For tests that check a read
/// path writes nothing (FEAT-076 R-7).
#[doc(hidden)]
pub fn commits_on_this_thread() -> u64 {
    COMMITS.with(std::cell::Cell::get)
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

/// The redb handle behind a [`Cache`]: the single writer, or a read-only handle beside one
/// (FEAT-080).
enum Handle {
    Writer(Database),
    Reader(ReadOnlyDatabase),
}

pub struct Cache {
    db: RwLock<Handle>,
    report: SyncReport,
    /// `.cache/index.redb`.
    path: Utf8PathBuf,
}

const BACKOFF_START: Duration = Duration::from_millis(5);
const BACKOFF_MAX: Duration = Duration::from_millis(50);

/// How processes share the cache file (FEAT-080). One writer process and any number of read-only
/// handles where byte-range locks exist; the whole-file lock elsewhere, where `open_reader`
/// always answers `None`, which keeps readers waiting for the writer as before.
#[cfg(any(target_os = "linux", target_vendor = "apple", windows))]
const CONCURRENCY: ConcurrencyMode = ConcurrencyMode::SingleWriter;
#[cfg(not(any(target_os = "linux", target_vendor = "apple", windows)))]
const CONCURRENCY: ConcurrencyMode = ConcurrencyMode::ExclusiveWriter;

/// The builder of every writer open. A repair that runs past `deadline` is aborted, which
/// `open_db` reports as [`StoreError::Busy`]; the next caller with a longer deadline finishes it.
pub(crate) fn writer_builder(deadline: Instant) -> redb::Builder {
    let mut b = redb::Builder::new();
    b.set_concurrency_mode(CONCURRENCY);
    b.set_repair_callback(move |session| {
        if Instant::now() >= deadline {
            session.abort();
        }
    });
    b
}

/// The builder of every read-only open.
pub(crate) fn reader_builder() -> redb::Builder {
    let mut b = redb::Builder::new();
    b.set_concurrency_mode(CONCURRENCY);
    b
}

/// True when this build's mode lets a read-only handle share the file with a writer.
pub(crate) fn shares_with_writer() -> bool {
    CONCURRENCY != ConcurrencyMode::ExclusiveWriter
}

fn open_db(
    path: &Utf8PathBuf,
    busy_until: Instant,
    deadline: Instant,
) -> Result<(Database, bool), StoreError> {
    let mut wait = BACKOFF_START;
    let mut recreated = false;
    loop {
        match writer_builder(deadline).create(path.as_std_path()) {
            Ok(d) => return Ok((d, recreated)),
            Err(DatabaseError::DatabaseAlreadyOpen) => {
                let now = Instant::now();
                if now >= busy_until {
                    return Err(StoreError::Busy);
                }
                std::thread::sleep(wait.min(busy_until - now));
                wait = (wait * 2).min(BACKOFF_MAX);
            }
            Err(DatabaseError::RepairAborted) => return Err(StoreError::Busy),
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

/// The writer open: creates the file, and recreates it when its schema differs. The flag is
/// true when the database is new: the file was missing, corrupt or of another schema.
fn open_writer(
    path: &Utf8PathBuf,
    busy_until: Instant,
    deadline: Instant,
) -> Result<(Database, bool), StoreError> {
    let existed = path.exists();
    let (mut database, mut recreated) = open_db(path, busy_until, deadline)?;
    if !schema_current(&database)? {
        drop(database);
        std::fs::remove_file(path).map_err(|e| StoreError::io(path, e))?;
        (database, _) = open_db(path, busy_until, deadline)?;
        schema_current(&database)?;
        recreated = true;
    }
    Ok((database, recreated || !existed))
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
        Cache::open_or_rebuild_busy(dir, stitch, deadline, deadline)
    }

    /// As [`Cache::open_or_rebuild`], but a database held by another writer is retried only until
    /// `busy_until` (FEAT-080: `busy_until` already passed means one try); the repair bound and
    /// the sync use `deadline`.
    pub fn open_or_rebuild_busy(
        dir: &ProjectDir,
        stitch: &dyn Restitch,
        busy_until: Instant,
        deadline: Instant,
    ) -> Result<Cache, StoreError> {
        let cache_dir = dir.cache_dir();
        std::fs::create_dir_all(&cache_dir).map_err(|e| StoreError::io(&cache_dir, e))?;
        let path = cache_dir.join("index.redb");
        let (database, created) = open_writer(&path, busy_until, deadline)?;
        // This process holds the database now, so no other process is mid-rewrite: a rewrite
        // file that exists is an orphan.
        crate::compact::remove_orphan(&cache_dir);
        let mut cache = Cache {
            db: RwLock::new(Handle::Writer(database)),
            report: SyncReport::default(),
            path,
        };
        cache.report = cache.after_open(dir, stitch, deadline)?;
        // A database built from the shards by this open holds the rows in the order the rounds
        // wrote them: rewrite it sorted (the "sorted bulk load").
        if created && cache.report.replaced > 0 && !cache.report.deadline_hit {
            let (c, _) = cache.compact(dir, CompactMode::preferred(), deadline)?;
            cache = c;
        }
        Ok(cache)
    }

    /// What follows every writer open. An interrupted publish is finished before the shards are
    /// compared with the hashes.
    fn after_open(
        &self,
        dir: &ProjectDir,
        stitch: &dyn Restitch,
        deadline: Instant,
    ) -> Result<SyncReport, StoreError> {
        self.replay_journal(dir)?;
        self.sync(dir, stitch, deadline)
    }

    /// FEAT-080 R-1, R-6. A read-only handle beside any writer. Never writes, syncs or replays.
    /// `Ok(None)` when the handle cannot serve: no file, another schema, tables not yet created,
    /// repair needed or running, or a corrupt header. Other errors are [`StoreError`].
    pub fn open_reader(dir: &ProjectDir) -> Result<Option<Cache>, StoreError> {
        if !shares_with_writer() {
            return Ok(None);
        }
        let path = dir.cache_dir().join("index.redb");
        let database = match reader_builder().open_read_only(path.as_std_path()) {
            Ok(d) => d,
            Err(DatabaseError::DatabaseAlreadyOpen)
            | Err(DatabaseError::RepairAborted)
            | Err(DatabaseError::UpgradeRequired(_))
            | Err(DatabaseError::Storage(StorageError::Corrupted(_))) => return Ok(None),
            Err(DatabaseError::Storage(StorageError::Io(e)))
                if e.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(None);
            }
            // A writer has created the file but not initialised it yet.
            Err(_)
                if std::fs::metadata(path.as_std_path())
                    .map(|m| m.len() == 0)
                    .unwrap_or(false) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(db(e)),
        };
        let txn = database.begin_read().map_err(db)?;
        if !TableId::ALL.iter().all(|t| txn.open_table(t.def()).is_ok()) {
            return Ok(None);
        }
        let rx = ReadTx::from_txn(txn);
        match rx.meta("cache_schema")? {
            Some(v) if v == CACHE_SCHEMA.to_le_bytes() => {}
            _ => return Ok(None),
        }
        drop(rx);
        Ok(Some(Cache {
            db: RwLock::new(Handle::Reader(database)),
            report: SyncReport::default(),
            path,
        }))
    }

    /// True for a handle from [`Cache::open_reader`] that has not been upgraded.
    pub fn is_reader(&self) -> bool {
        matches!(
            *self.db.read().unwrap_or_else(PoisonError::into_inner),
            Handle::Reader(_)
        )
    }

    /// FEAT-080 R-4. Replace a read-only handle with the writer, then run what
    /// `open_or_rebuild` runs after its open. `Ok(false)` when another process holds the writer
    /// (the reader is kept). No [`ReadTx`] may be alive.
    pub fn try_upgrade(
        &self,
        dir: &ProjectDir,
        stitch: &dyn Restitch,
        deadline: Instant,
    ) -> Result<bool, StoreError> {
        if !self.is_reader() {
            return Ok(true);
        }
        let path = dir.cache_dir().join("index.redb");
        let database = match writer_builder(deadline).create(path.as_std_path()) {
            Ok(d) => d,
            Err(DatabaseError::DatabaseAlreadyOpen) => return Ok(false),
            Err(DatabaseError::RepairAborted) => return Err(StoreError::Busy),
            Err(e) => return Err(db(e)),
        };
        if !schema_current(&database)? {
            return Err(StoreError::Db("the cache schema changed".into()));
        }
        *self.db.write().unwrap_or_else(PoisonError::into_inner) = Handle::Writer(database);
        self.after_open(dir, stitch, deadline)?;
        Ok(true)
    }

    pub fn read(&self) -> Result<ReadTx<'_>, StoreError> {
        let txn = match &*self.db.read().unwrap_or_else(PoisonError::into_inner) {
            Handle::Writer(d) => d.begin_read(),
            Handle::Reader(d) => d.begin_read(),
        }
        .map_err(db)?;
        Ok(ReadTx::from_txn(txn))
    }

    pub fn write(&self) -> Result<WriteTx<'_>, StoreError> {
        let txn = match &*self.db.read().unwrap_or_else(PoisonError::into_inner) {
            Handle::Writer(d) => d.begin_write().map_err(db)?,
            Handle::Reader(_) => return Err(StoreError::Busy),
        };
        Ok(WriteTx::new(txn))
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
                // The temp file was synced before the publish commit that wrote this entry.
                if from.exists() {
                    std::fs::rename(&from, &to).map_err(|e| StoreError::io(&from, e))?;
                }
            }
            #[cfg(feature = "failpoints")]
            if i == 0 {
                crate::failpoint::hit("mid_renames");
            }
        }
        // One sync per distinct directory, after the last rename and before the journal is
        // cleared (ADR-0033). Entries already renamed by an interrupted replay count too.
        for parent in journal_parents(dir.root(), &entries) {
            if parent.is_dir() {
                std::fs::File::open(&parent)
                    .and_then(|f| f.sync_all())
                    .map_err(|e| StoreError::io(&parent, e))?;
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

    /// Bytes of `.cache/index.redb` on disk.
    pub fn file_bytes(&self) -> Result<u64, StoreError> {
        std::fs::metadata(&self.path)
            .map(|m| m.len())
            .map_err(|e| StoreError::io(&self.path, e))
    }

    /// File size right after the last compaction (`meta.compacted_bytes`), if any.
    pub fn compacted_bytes(&self) -> Result<Option<u64>, StoreError> {
        self.read()?
            .meta(COMPACTED_BYTES_META)?
            .map(u64_value)
            .transpose()
    }

    /// Whether `policy` asks for a compaction now (see [`CompactPolicy::due`]).
    pub fn compaction_due(&self, policy: &CompactPolicy) -> Result<bool, StoreError> {
        Ok(policy.due(self.file_bytes()?, self.compacted_bytes()?))
    }

    /// Compact the cache (R-6, R-7). Consumes the cache because a rewrite swaps the file.
    /// Errors before the swap are absorbed: the original cache comes back with
    /// [`CompactOutcome::Skipped`]. After the swap, a reopen that stays busy until `deadline`
    /// gives `Err(StoreError::Busy)`; the data is safe in the new file.
    ///
    /// A read-only handle (see [`Cache::open_reader`]) is never compacted.
    pub fn compact(
        self,
        dir: &ProjectDir,
        mode: CompactMode,
        deadline: Instant,
    ) -> Result<(Cache, CompactReport), StoreError> {
        let started = Instant::now();
        let Cache { db, report, path } = self;
        let handle = db.into_inner().unwrap_or_else(PoisonError::into_inner);
        let bytes_before = std::fs::metadata(&path).map_or(0, |m| m.len());
        let report_of = |outcome: CompactOutcome, after: u64| CompactReport {
            mode,
            outcome,
            bytes_before,
            bytes_after: after,
            millis: started.elapsed().as_millis() as u64,
        };
        let mut database = match handle {
            Handle::Writer(d) => d,
            reader @ Handle::Reader(_) => {
                let cache = Cache {
                    db: RwLock::new(reader),
                    report,
                    path,
                };
                let r = report_of(
                    CompactOutcome::Skipped("the cache is open read-only".into()),
                    bytes_before,
                );
                return Ok((cache, r));
            }
        };
        let tmp = dir.cache_dir().join(compact::REWRITE_FILE);
        let skipped = |database: Database, why: String, report: SyncReport, path: Utf8PathBuf| {
            let after = std::fs::metadata(&path).map_or(bytes_before, |m| m.len());
            let cache = Cache {
                db: RwLock::new(Handle::Writer(database)),
                report,
                path,
            };
            Ok((cache, report_of(CompactOutcome::Skipped(why), after)))
        };
        match mode {
            CompactMode::InPlace => {
                if let Err(e) = database.compact() {
                    return skipped(
                        database,
                        format!("in-place compaction failed: {e}"),
                        report,
                        path,
                    );
                }
            }
            CompactMode::Rewrite => {
                // Steps 1 to 5: any failure leaves the original file as it was.
                let built = std::fs::remove_file(&tmp)
                    .or_else(|e| {
                        if e.kind() == std::io::ErrorKind::NotFound {
                            Ok(())
                        } else {
                            Err(e)
                        }
                    })
                    .map_err(|e| format!("the rewrite file cannot be removed ({:?})", e.kind()))
                    .and_then(|()| {
                        compact::rewrite(&database, &tmp, deadline)
                            .map_err(|e| format!("rewrite failed: {e}"))
                    });
                if let Err(why) = built {
                    let _ = std::fs::remove_file(&tmp);
                    return skipped(database, why, report, path);
                }
                #[cfg(feature = "failpoints")]
                crate::failpoint::hit("before_rewrite_rename");
                // The rename happens while this process still holds the old file.
                if let Err(e) = compact::swap(&tmp, &path) {
                    let _ = std::fs::remove_file(&tmp);
                    return skipped(
                        database,
                        format!(
                            "the rewritten file cannot replace the cache ({:?})",
                            e.kind()
                        ),
                        report,
                        path,
                    );
                }
                #[cfg(feature = "failpoints")]
                crate::failpoint::hit("after_rewrite_rename");
                drop(database);
                database = open_db(&path, deadline, deadline)?.0;
            }
        }
        let cache = Cache {
            db: RwLock::new(Handle::Writer(database)),
            report,
            path,
        };
        if mode == CompactMode::InPlace {
            // The rewrite recorded its size in the new file before the swap.
            cache.record_compacted_bytes()?;
        }
        let after = cache.file_bytes()?;
        Ok((cache, report_of(CompactOutcome::Done, after)))
    }

    /// Writes `meta.compacted_bytes` as the size of the file. The write can itself grow the
    /// file, so it repeats until the stored number is the size it left (a few rounds at most).
    fn record_compacted_bytes(&self) -> Result<(), StoreError> {
        let mut size = self.file_bytes()?;
        for _ in 0..4 {
            let mut tx = self.write()?;
            tx.set_meta(COMPACTED_BYTES_META, &size.to_le_bytes())?;
            tx.commit()?;
            let now = self.file_bytes()?;
            if now == size {
                break;
            }
            size = now;
        }
        Ok(())
    }

    /// Compare the shard files with the stored hashes and stats on a read snapshot: true when
    /// the sets are equal and every file's `(size, mtime)` equals the stored one, so `sync`
    /// would change nothing. A shard stored without a stat counts as different.
    fn shards_unchanged(&self, dir: &ProjectDir, on_disk: &[ShardKey]) -> Result<bool, StoreError> {
        let rows = self.read()?.shard_hash_rows()?;
        if rows.len() != on_disk.len() {
            return Ok(false);
        }
        let stored: HashMap<&ShardKey, &Option<ShardStat>> =
            rows.iter().map(|(k, _, st)| (k, st)).collect();
        for key in on_disk {
            let Some(Some(stat)) = stored.get(key).copied() else {
                return Ok(false);
            };
            let file = dir.shard_path(&key.0, &key.1);
            match std::fs::metadata(&file) {
                Ok(m) if ShardStat::of(&m) == Some(*stat) => {}
                _ => return Ok(false),
            }
        }
        Ok(true)
    }

    fn sync(
        &self,
        dir: &ProjectDir,
        stitch: &dyn Restitch,
        deadline: Instant,
    ) -> Result<SyncReport, StoreError> {
        let mut report = SyncReport::default();
        let on_disk = dir.list_shards()?;
        if self.shards_unchanged(dir, &on_disk)? {
            return Ok(report);
        }
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

/// Create the tables and check `meta.cache_schema`. `false`: written by another schema. A cache
/// that already holds every table and the current schema is only read (FEAT-076 R-7).
fn schema_current(database: &Database) -> Result<bool, StoreError> {
    {
        let txn = database.begin_read().map_err(db)?;
        let all_tables = TableId::ALL.iter().all(|t| txn.open_table(t.def()).is_ok());
        if all_tables {
            let rx = ReadTx::from_txn(txn);
            match rx.meta("cache_schema")? {
                Some(v) if v == CACHE_SCHEMA.to_le_bytes() => return Ok(true),
                Some(_) => return Ok(false),
                None => {}
            }
        }
    }
    let txn = database.begin_write().map_err(db)?;
    let mut tx = WriteTx::new(txn);
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
    fn journal_parents_distinct() {
        let root = Utf8Path::new("/kx/projects/p");
        let entries: Vec<JournalEntry> = vec![
            ("shards/r/.tmp-1".into(), "shards/r/a.json".into()),
            ("shards/r/.tmp-2".into(), "shards/r/b.json".into()),
            ("shards/r/sub/.tmp-3".into(), "shards/r/sub/c.json".into()),
            (String::new(), "shards/r/sub/gone.json".into()),
        ];
        let parents = journal_parents(root, &entries);
        let want: BTreeSet<Utf8PathBuf> =
            ["/kx/projects/p/shards/r", "/kx/projects/p/shards/r/sub"]
                .into_iter()
                .map(Utf8PathBuf::from)
                .collect();
        assert_eq!(parents, want);
        assert!(journal_parents(root, &[]).is_empty());
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

    // ------------------------------------------------ dictionaries (FEAT-075 T4)

    fn prov_at(path: &str, rev: &str) -> Provenance {
        Provenance {
            repo: "r".into(),
            path: RelPath::new(path).unwrap(),
            revision: rev.repeat(40),
        }
    }

    fn link_at(path: &str, rev: &str) -> LinkRow {
        let mut e = entry(LinkOrigin::Intra, Confidence::Extracted, None);
        e.provenance = prov_at(path, rev);
        LinkRow { entries: vec![e] }
    }

    fn prov_rows(tx: &impl Reader) -> Vec<(Vec<u8>, crate::codec::WireProv)> {
        tx.raw_scan(TableId::Prov, &[])
            .unwrap()
            .into_iter()
            .map(|(k, v)| (k, crate::codec::dec(&v).unwrap()))
            .collect()
    }

    #[test]
    fn prov_interned_once() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        let mut tx = c.write().unwrap();
        for i in 1..=5u8 {
            tx.put_link(&lk(i, "calls", 9), &link_at("a.tx", "0"))
                .unwrap();
        }
        tx.commit().unwrap();
        let rx = c.read().unwrap();
        let rows = prov_rows(&rx);
        assert_eq!(rows.len(), 1, "one provenance row for five links");
        assert_eq!(rows[0].1.refs, 5);
        assert_eq!(count(&rx, TableId::ProvIx), 1);
        assert_eq!(rx.links_all().unwrap().len(), 5);
        assert_eq!(
            rx.links_out(nid(1), None).unwrap()[0].1.entries[0].provenance,
            prov_at("a.tx", "0")
        );
    }

    #[test]
    fn prov_refcount_tracks_writes() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        let mut tx = c.write().unwrap();
        tx.put_link(&lk(1, "calls", 2), &link_at("a.tx", "0"))
            .unwrap();
        tx.put_link(&lk(1, "calls", 3), &link_at("a.tx", "0"))
            .unwrap();
        tx.commit().unwrap();
        assert_eq!(prov_rows(&c.read().unwrap())[0].1.refs, 2);

        // Replacing a row moves its count to the new provenance.
        let mut tx = c.write().unwrap();
        tx.put_link(&lk(1, "calls", 2), &link_at("b.tx", "1"))
            .unwrap();
        tx.commit().unwrap();
        let rows = prov_rows(&c.read().unwrap());
        let by_path = |p: &str| {
            rows.iter()
                .find(|(_, w)| w.path.as_str() == p)
                .unwrap()
                .1
                .refs
        };
        assert_eq!((by_path("a.tx"), by_path("b.tx")), (1, 1));

        // Removing the last reference collects the row and its index entry.
        let mut tx = c.write().unwrap();
        tx.remove_link(&lk(1, "calls", 3)).unwrap();
        tx.commit().unwrap();
        let rx = c.read().unwrap();
        let rows = prov_rows(&rx);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1.path.as_str(), "b.tx");
        assert_eq!(count(&rx, TableId::ProvIx), 1);

        // A write and its removal inside one transaction leave nothing behind.
        let mut tx = c.write().unwrap();
        tx.put_link(&lk(4, "calls", 5), &link_at("z.tx", "2"))
            .unwrap();
        tx.remove_link(&lk(4, "calls", 5)).unwrap();
        tx.commit().unwrap();
        assert_eq!(prov_rows(&c.read().unwrap()).len(), 1);
    }

    #[test]
    fn aborted_tx_leaves_dictionary() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        let before_vocab = count(&c.read().unwrap(), TableId::Vocab);
        {
            let mut tx = c.write().unwrap();
            tx.put_link(&lk(1, "brand_new_kind", 2), &link_at("a.tx", "0"))
                .unwrap();
            assert_eq!(
                count(&tx, TableId::Prov),
                1,
                "visible inside the transaction"
            );
            // dropped without commit
        }
        let rx = c.read().unwrap();
        assert_eq!(count(&rx, TableId::Prov), 0);
        assert_eq!(count(&rx, TableId::ProvIx), 0);
        assert_eq!(count(&rx, TableId::Vocab), before_vocab);
        assert_eq!(count(&rx, TableId::VocabIx), before_vocab);
        assert!(rx.meta("prov_next").unwrap().is_none());
        // The ids restart where the committed state ended.
        drop(rx);
        let mut tx = c.write().unwrap();
        tx.put_link(&lk(1, "calls", 2), &link_at("a.tx", "0"))
            .unwrap();
        tx.commit().unwrap();
        let rows = prov_rows(&c.read().unwrap());
        assert_eq!(rows[0].0, 0u32.to_be_bytes().to_vec());
    }

    #[test]
    fn vocab_assign_and_lookup() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        let mut tx = c.write().unwrap();
        for (i, k) in ["calls", "zz", "a", "calls"].iter().enumerate() {
            tx.put_link(&lk(i as u8 + 1, k, 9), &link_at("a.tx", "0"))
                .unwrap();
        }
        tx.commit().unwrap();
        let rx = c.read().unwrap();
        assert_eq!(rx.vid_of("calls").unwrap(), Some(0));
        assert_eq!(rx.vid_of("zz").unwrap(), Some(1));
        assert_eq!(rx.vid_of("a").unwrap(), Some(2));
        assert_eq!(rx.vocab_str(1).unwrap(), "zz");
        assert!(rx.vocab_str(77).is_err(), "a dangling id is an error");
        let kinds: Vec<_> = rx.link_kinds().unwrap().into_iter().collect();
        assert_eq!(kinds, ["a", "calls", "zz"]);
        // Removing every row of a kind never frees its id.
        drop(rx);
        let mut tx = c.write().unwrap();
        tx.remove_link(&lk(2, "zz", 9)).unwrap();
        tx.commit().unwrap();
        assert_eq!(c.read().unwrap().vid_of("zz").unwrap(), Some(1));
    }

    #[test]
    fn vocab_full_is_error() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        {
            let mut tx = c.write().unwrap();
            tx.put(TableId::Vocab, &0xFFFFu16.to_be_bytes(), b"last")
                .unwrap();
            tx.put(TableId::VocabIx, b"last", &0xFFFFu16.to_be_bytes())
                .unwrap();
            tx.commit().unwrap();
        }
        let mut tx = c.write().unwrap();
        let err = tx
            .put_link(&lk(1, "one_too_many", 2), &link_at("a.tx", "0"))
            .unwrap_err();
        assert!(matches!(err, StoreError::Row(m) if m.contains("full")));
        // A string that has an id still works.
        tx.put_link(&lk(1, "last", 2), &link_at("a.tx", "0"))
            .unwrap();
    }

    #[test]
    fn unknown_vocab_reads_empty() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        let commits = commits_on_this_thread();
        let rx = c.read().unwrap();
        assert!(
            rx.links_out(nid(1), Some(&LinkKind("nope".into())))
                .unwrap()
                .is_empty()
        );
        assert!(
            rx.link_keys_out(nid(1), &LinkKind("nope".into()))
                .unwrap()
                .is_empty()
        );
        assert!(rx.exports_in("nope").unwrap().is_empty());
        assert!(rx.exports(&key("nope", "k")).unwrap().is_empty());
        assert_eq!(rx.export_count(&key("nope", "k")).unwrap(), 0);
        assert!(rx.refs_by_key(&key("nope", "k")).unwrap().is_empty());
        assert!(rx.bound_refs_by_key(&key("nope", "k")).unwrap().is_empty());
        assert!(rx.link(&lk(1, "nope", 2)).unwrap().is_none());
        assert_eq!(
            rx.link_briefs_many(&[lk(1, "nope", 2)]).unwrap(),
            vec![None]
        );
        drop(rx);
        // Writes of rows that were never there do not create vocabulary entries.
        let mut tx = c.write().unwrap();
        tx.remove_link(&lk(1, "nope", 2)).unwrap();
        tx.remove_ref_by_key(&key("nope", "k"), rid(1, 0)).unwrap();
        tx.put_exports(&key("nope", "k"), &[]).unwrap();
        tx.commit().unwrap();
        let rx = c.read().unwrap();
        assert_eq!(count(&rx, TableId::Vocab), 0);
        assert_eq!(commits_on_this_thread(), commits + 1);
    }

    // ------------------------------------------------ packed postings (FEAT-075 T7)

    fn terms(list: &[(&str, u8, u32)]) -> Vec<(String, u8, u32)> {
        list.iter()
            .map(|(t, f, n)| (t.to_string(), *f, *n))
            .collect()
    }

    fn ord_of(tx: &impl Reader, id: Id) -> u32 {
        let raw = tx.raw_get(TableId::DocOrd, &id.0).unwrap().unwrap();
        crate::postings::decode_ord(&raw).unwrap()
    }

    #[test]
    fn ordinal_kept_across_reindex() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        let mut tx = c.write().unwrap();
        tx.put_doc_postings(nid(1), [1, 0, 0, 0], &terms(&[("alpha", 0, 1)]))
            .unwrap();
        tx.put_doc_postings(nid(2), [2, 0, 0, 0], &terms(&[("alpha", 0, 2)]))
            .unwrap();
        tx.commit().unwrap();
        let rx = c.read().unwrap();
        let (o1, o2) = (ord_of(&rx, nid(1)), ord_of(&rx, nid(2)));
        assert_ne!(o1, o2);
        drop(rx);

        // Remove and index again in one transaction: the ordinal stays.
        let mut tx = c.write().unwrap();
        tx.remove_doc_postings(nid(1), &[("alpha".into(), 0)])
            .unwrap();
        assert_eq!(
            tx.doc_len(nid(1)).unwrap(),
            None,
            "removed inside the transaction"
        );
        tx.put_doc_postings(nid(1), [5, 0, 0, 0], &terms(&[("beta", 1, 3)]))
            .unwrap();
        tx.commit().unwrap();
        let rx = c.read().unwrap();
        assert_eq!(ord_of(&rx, nid(1)), o1);
        assert_eq!(ord_of(&rx, nid(2)), o2);
        assert_eq!(rx.doc_len(nid(1)).unwrap(), Some([5, 0, 0, 0]));
        assert_eq!(
            rx.postings("alpha").unwrap().len(),
            1,
            "only symbol 2 is left on alpha"
        );
        assert_eq!(rx.postings("beta").unwrap()[0].symbol, nid(1));
        assert_eq!(rx.lexicon_stats().unwrap().docs, 2);
        drop(rx);

        // Removed in one transaction, indexed in a later one: a new ordinal; none is reused.
        let mut tx = c.write().unwrap();
        tx.remove_doc_postings(nid(1), &[("beta".into(), 1)])
            .unwrap();
        tx.commit().unwrap();
        assert!(
            c.read()
                .unwrap()
                .raw_get(TableId::DocOrd, &nid(1).0)
                .unwrap()
                .is_none()
        );
        let mut tx = c.write().unwrap();
        tx.put_doc_postings(nid(1), [1, 0, 0, 0], &terms(&[("beta", 1, 1)]))
            .unwrap();
        tx.commit().unwrap();
        let rx = c.read().unwrap();
        let o1b = ord_of(&rx, nid(1));
        assert!(o1b > o1 && o1b > o2);
    }

    #[test]
    fn buffer_flushed_before_read() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        let mut tx = c.write().unwrap();
        tx.put_doc_postings(
            nid(1),
            [1, 0, 0, 0],
            &terms(&[("alpha", 0, 4), ("alphabet", 2, 1)]),
        )
        .unwrap();
        // Nothing is committed, yet the transaction reads its own postings.
        let ps = tx.postings("alpha").unwrap();
        assert_eq!(
            ps,
            vec![Posting {
                field: 0,
                symbol: nid(1),
                tf: 4,
                doc_len: [1, 0, 0, 0]
            }]
        );
        let pre = tx.postings_prefix("alpha", 8).unwrap();
        assert_eq!(pre.len(), 1, "the exact term is excluded");
        assert_eq!(pre[0].0, "alphabet");
        tx.remove_doc_postings(nid(1), &[("alpha".into(), 0), ("alphabet".into(), 2)])
            .unwrap();
        assert!(tx.postings("alpha").unwrap().is_empty());
        assert!(tx.raw_scan(TableId::Postings, &[]).unwrap().is_empty());
        tx.commit().unwrap();
        let rx = c.read().unwrap();
        assert!(rx.postings("alpha").unwrap().is_empty());
        assert_eq!(count(&rx, TableId::DocOrd), 0);
        assert_eq!(count(&rx, TableId::DocByOrd), 0);
    }

    #[test]
    fn postings_span_blocks_and_rank_terms() {
        let t = tempfile::tempdir().unwrap();
        let c = cache(&t);
        let mut tx = c.write().unwrap();
        for i in 0..2100u32 {
            let mut b = [0u8; 16];
            b[..4].copy_from_slice(&i.to_be_bytes());
            let tf = if i % 2 == 0 { 2 } else { 1 };
            tx.put_doc_postings(
                Id(b),
                [i, 0, 0, 0],
                &terms(&[("common", (i % 4) as u8, tf), ("commonly", 1, 1)]),
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let rx = c.read().unwrap();
        // 2,100 ordinals: three blocks for each of the two terms.
        assert_eq!(count(&rx, TableId::Postings), 6);
        let ps = rx.postings("common").unwrap();
        assert_eq!(ps.len(), 2100);
        assert!(
            ps.windows(2)
                .all(|w| (w[0].field, w[0].symbol) < (w[1].field, w[1].symbol))
        );
        assert!(
            ps.iter()
                .all(|p| p.doc_len[0] == u32::from_be_bytes(p.symbol.0[..4].try_into().unwrap()))
        );
        let pre = rx.postings_prefix("common", 8).unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0].1.len(), 2100);
    }
}
