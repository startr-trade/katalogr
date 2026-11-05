//! FEAT-076 R-1 to R-3: the `symbol_core` table follows the symbols, and name lookups through it
//! answer exactly as a scan of the whole symbols table did.
mod support;

use camino::Utf8PathBuf;
use katalogr_engine::Stitcher;
use katalogr_model::{
    ArtifactName, Confidence, ExtractInput, Extractor, Fragment, Id, LinkKind, Name, Provenance,
    RelPath,
};
use katalogr_query::{Resolution, SymbolHit};
use katalogr_store::cache::{RawRows, TableId};
use katalogr_store::{
    Cache, Kindex, LinkEntry, LinkKey, LinkOrigin, LinkRow, ProjectConfig, ProjectDir, ProjectName,
    Reader, RepoConfig, StoreError, SymbolRow,
};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use support::TestExtractor;

struct Env {
    _tmp: tempfile::TempDir,
    dir: ProjectDir,
    cache: Cache,
    stitcher: Stitcher,
}

fn project(root: Utf8PathBuf) -> ProjectDir {
    Kindex::init(&root)
        .unwrap()
        .create_project(
            &ProjectName::new("p").unwrap(),
            ProjectConfig {
                repos: vec![RepoConfig::new("r")],
                ..Default::default()
            },
        )
        .unwrap()
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let dir = project(Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap());
    let stitcher = Stitcher::new();
    let cache =
        Cache::open_or_rebuild(&dir, &stitcher, Instant::now() + Duration::from_secs(5)).unwrap();
    Env {
        _tmp: tmp,
        dir,
        cache,
        stitcher,
    }
}

fn extract(path: &str, text: &str) -> Fragment {
    let path = RelPath::new(path).unwrap();
    TestExtractor
        .extract(&ExtractInput {
            repo: "r",
            path: &path,
            revision: "ce013625030ba8dba906f756967f9e9ca394464a",
            content: text.as_bytes(),
            context: &[],
        })
        .unwrap()
}

impl Env {
    fn put(&self, path: &str, text: &str) {
        let mut tx = self.cache.write().unwrap();
        self.stitcher
            .replace_fragment(&mut tx, extract(path, text))
            .unwrap();
        tx.commit().unwrap();
    }

    fn remove(&self, path: &str) {
        let mut tx = self.cache.write().unwrap();
        self.stitcher
            .remove_artifact(&mut tx, &ArtifactName::new("r", path).unwrap())
            .unwrap();
        tx.commit().unwrap();
    }
}

/// Mixed case, non-ASCII, two symbols of one plain name, an alias and an impl.
const A: &str =
    "sym struct Foo\nsym fn foo\nsym fn FOO\nsym fn Éclair\nsym fn éCLAIR\nsym fn bar_foo\n";
const B: &str =
    "sym fn Foo\nsym use Alias\nsym fn Target\nlink Alias reexports Target\nsym fn Baz\n";

/// An `impl` symbol named like the struct `Foo` of `a.tx`, linked to it (`implements-for`).
fn add_impl(e: &Env) {
    let mut tx = e.cache.write().unwrap();
    let (struct_id, mut row) = tx
        .symbols()
        .unwrap()
        .into_iter()
        .find(|(_, r)| r.kind == "struct" && plain(r) == "Foo")
        .expect("struct Foo in a.tx");
    row.name.segments[0].kind = "impl".into();
    row.kind = "impl".into();
    let impl_id = row.name.id();
    tx.put_symbol(impl_id, &row).unwrap();
    let entry = LinkEntry {
        origin: LinkOrigin::Intra,
        confidence: Confidence::Extracted,
        span: Some(row.span),
        provenance: Provenance {
            repo: "r".into(),
            path: RelPath::new("a.tx").unwrap(),
            revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
        },
        fanout: None,
        produced_by: None,
    };
    tx.put_link(
        &LinkKey {
            from: impl_id,
            kind: LinkKind("implements-for".into()),
            to: struct_id,
        },
        &LinkRow {
            entries: vec![entry],
        },
    )
    .unwrap();
    tx.commit().unwrap();
}

fn dump(rows: &RawRows) -> Vec<(Vec<u8>, Vec<u8>)> {
    rows.clone()
}

// ---- the reference: the old full-scan tiers, copied

fn plain(row: &SymbolRow) -> String {
    row.name
        .segments
        .last()
        .map_or("", |s| s.name.as_str())
        .to_string()
}

fn hit(row: &SymbolRow) -> SymbolHit {
    SymbolHit {
        name: row.name.canonical(),
        kind: row.kind.clone(),
        path: row.name.artifact.path.as_str().to_string(),
        span: row.span,
    }
}

fn all_hits(tx: &impl Reader) -> Vec<(SymbolHit, String)> {
    let mut out: Vec<(SymbolHit, String)> = tx
        .symbols()
        .unwrap()
        .into_iter()
        .map(|(_, r)| (hit(&r), plain(&r)))
        .collect();
    out.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    out
}

fn original_of(tx: &impl Reader, alias: &SymbolHit) -> Option<SymbolHit> {
    let reexports = LinkKind("reexports".into());
    let mut seen = BTreeSet::new();
    let mut at = Id::of(&alias.name);
    seen.insert(at);
    loop {
        let out = tx.links_out(at, Some(&reexports)).unwrap();
        let [(key, _)] = out.as_slice() else { break };
        if !seen.insert(key.to) {
            return None;
        }
        at = key.to;
    }
    if at == Id::of(&alias.name) {
        return None;
    }
    tx.symbol(at).unwrap().map(|r| hit(&r))
}

fn collapse(tx: &impl Reader, found: Vec<SymbolHit>) -> Vec<SymbolHit> {
    let implements = LinkKind("implements-for".into());
    let mut out: Vec<SymbolHit> = Vec::new();
    for c in &found {
        let mut c = c.clone();
        if ["use", "reexport"].contains(&c.kind.as_str()) {
            if let Some(orig) = original_of(tx, &c) {
                c = orig;
            }
        } else if c.kind == "impl" {
            let mut into_type = false;
            for (key, _) in tx.links_out(Id::of(&c.name), Some(&implements)).unwrap() {
                into_type |= found.iter().any(|t| {
                    ["struct", "enum", "union", "trait"].contains(&t.kind.as_str())
                        && t.path == c.path
                        && Id::of(&t.name) == key.to
                });
            }
            if into_type {
                continue;
            }
        }
        if !out.iter().any(|o| o.name == c.name) {
            out.push(c);
        }
    }
    out
}

fn old_resolve(tx: &impl Reader, text: &str) -> Resolution<SymbolHit> {
    let all = all_hits(tx);
    let lower = text.to_lowercase();
    for tier in 0..3 {
        let mut found: Vec<SymbolHit> = all
            .iter()
            .filter(|(h, n)| match tier {
                0 => h.name == text,
                1 => n == text,
                _ => n.to_lowercase() == lower,
            })
            .map(|(h, _)| h.clone())
            .collect();
        if !found.is_empty() {
            found = collapse(tx, found);
        }
        match found.len() {
            0 => continue,
            1 => return Resolution::One(found.remove(0)),
            _ => return Resolution::Ambiguous(found),
        }
    }
    Resolution::None
}

fn old_find(tx: &impl Reader, text: &str) -> Vec<SymbolHit> {
    let lower = text.to_lowercase();
    all_hits(tx)
        .into_iter()
        .filter(|(h, n)| h.name == text || n.to_lowercase().contains(&lower))
        .map(|(h, _)| h)
        .collect()
}

/// A reader that records the tables scanned.
struct Spy<'a, R: Reader> {
    inner: &'a R,
    scanned: RefCell<Vec<TableId>>,
}

impl<R: Reader> Reader for Spy<'_, R> {
    fn memo(&self) -> &katalogr_store::Memo {
        self.inner.memo()
    }
    fn raw_get(&self, t: TableId, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.raw_get(t, key)
    }
    fn raw_scan(&self, t: TableId, prefix: &[u8]) -> Result<RawRows, StoreError> {
        self.scanned.borrow_mut().push(t);
        self.inner.raw_scan(t, prefix)
    }
    fn raw_scan_keys(&self, t: TableId, prefix: &[u8]) -> Result<Vec<Vec<u8>>, StoreError> {
        self.scanned.borrow_mut().push(t);
        self.inner.raw_scan_keys(t, prefix)
    }
    fn raw_get_many(
        &self,
        t: TableId,
        keys: &[Vec<u8>],
    ) -> Result<Vec<Option<Vec<u8>>>, StoreError> {
        self.inner.raw_get_many(t, keys)
    }
}

fn fixture() -> Env {
    let e = env();
    e.put("a.tx", A);
    e.put("b.tx", B);
    e.put("c.tx", "sym fn Foo\nsym fn aFoo\nsym enum Baz\n");
    add_impl(&e);
    e
}

/// Every plain name and canonical name in the graph, case variants, and some misses.
fn probes(tx: &impl Reader) -> Vec<String> {
    let mut v: BTreeSet<String> = BTreeSet::new();
    for (_, r) in tx.symbols().unwrap() {
        let p = plain(&r);
        v.insert(p.clone());
        v.insert(p.to_uppercase());
        v.insert(p.to_lowercase());
        v.insert(r.name.canonical());
    }
    for s in ["nothing", "", "o", "FOO", "éclair", "ÉCLAIR", "ba"] {
        v.insert(s.to_string());
    }
    v.into_iter().collect()
}

mod symbol_core {
    use super::*;

    fn cores_match_symbols(tx: &impl Reader) {
        let symbols = tx.symbols().unwrap();
        let cores = tx.symbol_cores().unwrap();
        assert_eq!(symbols.len(), cores.len());
        for ((id, row), (cid, core)) in symbols.iter().zip(&cores) {
            assert_eq!(id, cid);
            assert_eq!(core.name, row.name.canonical());
            assert_eq!(core.kind, row.kind);
            assert_eq!(core.path, row.name.artifact.path.as_str());
            assert_eq!(core.artifact, row.artifact);
            assert_eq!(core.span, row.span);
        }
    }

    #[test]
    fn row_follows_put_and_remove() {
        let e = env();
        e.put("a.tx", A);
        e.put("b.tx", B);
        cores_match_symbols(&e.cache.read().unwrap());
        assert!(!e.cache.read().unwrap().symbol_cores().unwrap().is_empty());

        // A changed file replaces its rows.
        e.put("a.tx", "sym fn Foo2\nsym fn foo\n");
        cores_match_symbols(&e.cache.read().unwrap());
        let rx = e.cache.read().unwrap();
        let ids = rx.symbol_ids_by_plain_ci("foo").unwrap();
        assert_eq!(
            rx.symbols_many(&ids).unwrap().len(),
            2,
            "foo (a.tx), Foo (b.tx)"
        );
        drop(rx);

        // A removed file takes them away.
        e.remove("a.tx");
        e.remove("b.tx");
        let rx = e.cache.read().unwrap();
        assert!(rx.symbol_cores().unwrap().is_empty());
        assert!(rx.raw_scan(TableId::SymbolCore, &[]).unwrap().is_empty());
        drop(rx);

        // Removing an absent symbol writes nothing.
        let mut tx = e.cache.write().unwrap();
        tx.remove_symbol(Id::of("nothing")).unwrap();
        tx.commit().unwrap();
        assert!(e.cache.read().unwrap().symbol_cores().unwrap().is_empty());
    }

    #[test]
    fn rebuild_matches_incremental() {
        let e = env();
        e.put("a.tx", A);
        e.put("b.tx", B);
        e.put("c.tx", "sym fn Gone\n");
        e.put("a.tx", "sym struct Foo\nsym fn Other\n");
        e.remove("c.tx");

        // The same final shards, stitched from scratch.
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(Utf8PathBuf::from_path_buf(tmp.path().join("kx2")).unwrap());
        dir.write_shard(&extract("a.tx", "sym struct Foo\nsym fn Other\n"))
            .unwrap();
        dir.write_shard(&extract("b.tx", B)).unwrap();
        let rebuilt = Cache::open_or_rebuild(
            &dir,
            &Stitcher::new(),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        let table = |c: &Cache, t: TableId| dump(&c.read().unwrap().raw_scan(t, &[]).unwrap());
        assert_eq!(
            table(&e.cache, TableId::SymbolCore),
            table(&rebuilt, TableId::SymbolCore)
        );
        // The symbol rows hold kind ids that depend on the history, so compare the rows.
        assert_eq!(
            e.cache.read().unwrap().symbols().unwrap(),
            rebuilt.read().unwrap().symbols().unwrap()
        );
        let _ = &e.dir;
    }

    #[test]
    fn resolve_matches_full_scan() {
        let e = fixture();
        let rx = e.cache.read().unwrap();
        let ps = probes(&rx);
        assert!(ps.len() > 20);
        let mut kinds = [false; 3];
        for p in &ps {
            let new = katalogr_query::resolve(&rx, p).unwrap();
            assert_eq!(new, old_resolve(&rx, p), "resolve {p:?}");
            kinds[match new {
                Resolution::One(_) => 0,
                Resolution::Ambiguous(_) => 1,
                Resolution::None => 2,
            }] = true;
        }
        assert_eq!(
            kinds, [true; 3],
            "one, ambiguous and none are all exercised"
        );
    }

    #[test]
    fn resolve_scans_no_symbol_table() {
        let e = fixture();
        let rx = e.cache.read().unwrap();
        let spy = Spy {
            inner: &rx,
            scanned: RefCell::new(Vec::new()),
        };
        for p in probes(&rx) {
            katalogr_query::resolve(&spy, &p).unwrap();
        }
        assert!(!spy.scanned.borrow().contains(&TableId::Symbols));
    }

    #[test]
    fn find_matches_full_scan() {
        let e = fixture();
        let rx = e.cache.read().unwrap();
        for p in probes(&rx) {
            assert_eq!(
                katalogr_query::find(&rx, &p).unwrap(),
                old_find(&rx, &p),
                "find {p:?}"
            );
        }
    }

    #[test]
    fn find_scans_no_symbol_table() {
        let e = fixture();
        let rx = e.cache.read().unwrap();
        let spy = Spy {
            inner: &rx,
            scanned: RefCell::new(Vec::new()),
        };
        for p in probes(&rx) {
            katalogr_query::find(&spy, &p).unwrap();
        }
        assert!(!spy.scanned.borrow().contains(&TableId::Symbols));
    }
}
