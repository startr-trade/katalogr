//! FEAT-076 R-4 and R-5: the link walk gives what `links_all` gives, in the same order and with
//! the same weights, without expanding fan rows.
mod support;

use camino::Utf8PathBuf;
use katalogr_engine::Stitcher;
use katalogr_model::{
    Confidence, ExportKey, ExtractInput, Extractor, Fragment, Id, LinkKind, Name, Provenance,
    RelPath,
};
use katalogr_store::{
    Cache, FanKey, Kindex, LinkEntry, LinkOrigin, LinkRow, ProjectConfig, ProjectName, Reader,
    RepoConfig, row_weight,
};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use support::TestExtractor;

struct Env {
    _tmp: tempfile::TempDir,
    cache: Cache,
    stitcher: Stitcher,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
    let dir = Kindex::init(&root)
        .unwrap()
        .create_project(
            &ProjectName::new("p").unwrap(),
            ProjectConfig {
                repos: vec![RepoConfig::new("r")],
                ..Default::default()
            },
        )
        .unwrap();
    let stitcher = Stitcher::new();
    let cache =
        Cache::open_or_rebuild(&dir, &stitcher, Instant::now() + Duration::from_secs(5)).unwrap();
    Env {
        _tmp: tmp,
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
}

fn sym(path: &str, name: &str) -> Id {
    extract(path, &format!("sym fn {name}\n")).symbols[0]
        .name
        .id()
}

/// Two keys with overlapping targets, a stored link that overlaps one target, several kinds,
/// a single-target reference, an unresolved one, and a second source with its own fan row.
fn graph() -> Env {
    let e = env();
    e.put(
        "x.tx",
        "sym fn X\nexport ns:k X\nexport ns:j X\nexport ns:x X\n",
    );
    e.put(
        "y.tx",
        "sym fn Y\nexport ns:k Y\nexport ns:j Y\nexport ns:s Y\n",
    );
    e.put("w.tx", "sym fn W\nexport ns:k W\nref W uses ns:s\n");
    e.put(
        "z.tx",
        "sym fn Z\nsym fn Z2\nref Z calls ns:k\nref Z calls ns:j\nref Z calls ns:x\n\
link Z calls Z2\nlink Z2 uses Z\nref Z2 reads ns:k\nref Z2 reads ns:nope\n",
    );
    e
}

/// Analytics' rule, copied: any entry that is not inferred and has a fan-out of one at most.
fn strong(row: &LinkRow) -> bool {
    row.entries
        .iter()
        .any(|e| e.confidence != Confidence::Inferred && e.fanout.is_none_or(|n| n <= 1))
}

type Walked = Vec<(Id, String, Id, u64, bool)>;

fn walked(tx: &impl Reader) -> Walked {
    let mut out = Vec::new();
    tx.for_each_link(&mut |from, kind, to, w, s| {
        out.push((from, kind.to_string(), to, w.to_bits(), s))
    })
    .unwrap();
    out
}

fn expanded(tx: &impl Reader) -> Walked {
    tx.links_all()
        .unwrap()
        .into_iter()
        .map(|(k, row)| {
            (
                k.from,
                k.kind.0,
                k.to,
                row_weight(&row).to_bits(),
                strong(&row),
            )
        })
        .collect()
}

mod link_walk {
    use super::*;

    #[test]
    fn matches_links_all_order_and_weight() {
        let e = graph();
        let rx = e.cache.read().unwrap();
        assert_eq!(
            rx.fans_all().unwrap().len(),
            3,
            "the fixture stores fan rows"
        );
        let got = walked(&rx);
        assert!(got.len() > 8);
        assert_eq!(got, expanded(&rx));
        let kinds: Vec<String> = rx.link_kinds().unwrap().into_iter().collect();
        assert_eq!(kinds, ["calls", "reads", "uses"]);
    }

    #[test]
    fn overlap_with_stored_row_keeps_max() {
        let e = graph();
        let rx = e.cache.read().unwrap();
        let (z, x, y, w) = (
            sym("z.tx", "Z"),
            sym("x.tx", "X"),
            sym("y.tx", "Y"),
            sym("w.tx", "W"),
        );
        let got = walked(&rx);
        let find = |to: Id| {
            got.iter()
                .find(|g| g.0 == z && g.1 == "calls" && g.2 == to)
                .map(|g| (f64::from_bits(g.3), g.4))
                .unwrap()
        };
        // X: stored (weight 1, strong) and reached by both keys.
        assert_eq!(find(x), (1.0, true));
        // Y: both keys (3 and 2 targets): the larger weight, 1/2; weak.
        assert_eq!(find(y), (0.5, false));
        // W: only the three-target key.
        assert_eq!(find(w), (1.0 / 3.0, false));
    }

    #[test]
    fn zero_target_fan_emits_nothing() {
        let e = graph();
        let z = sym("z.tx", "Z");
        let before = walked(&e.cache.read().unwrap());
        let mut refs_before = Vec::new();
        e.cache
            .read()
            .unwrap()
            .for_each_ref_entry(&mut |f, k, i, r| refs_before.push((f, k.to_string(), i, r)))
            .unwrap();
        let provenance = Provenance {
            repo: "r".into(),
            path: RelPath::new("z.tx").unwrap(),
            revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
        };
        let mut tx = e.cache.write().unwrap();
        tx.put_fan(
            &FanKey {
                from: z,
                kind: LinkKind("calls".into()),
                key: ExportKey {
                    namespace: "ns".into(),
                    key: "no-target".into(),
                },
            },
            &LinkRow {
                entries: vec![LinkEntry {
                    origin: LinkOrigin::Ref(9),
                    confidence: Confidence::Inferred,
                    span: None,
                    provenance,
                    fanout: None,
                    produced_by: None,
                }],
            },
        )
        .unwrap();
        tx.commit().unwrap();
        let rx = e.cache.read().unwrap();
        assert_eq!(walked(&rx), before);
        assert_eq!(walked(&rx), expanded(&rx));
        let mut refs_after = Vec::new();
        rx.for_each_ref_entry(&mut |f, k, i, r| refs_after.push((f, k.to_string(), i, r)))
            .unwrap();
        assert_eq!(refs_after, refs_before);
    }

    #[test]
    fn ref_entries_match_links_all() {
        let e = graph();
        let rx = e.cache.read().unwrap();
        let mut want: BTreeMap<(Id, String, u32), bool> = BTreeMap::new();
        for (key, row) in rx.links_all().unwrap() {
            for en in &row.entries {
                let LinkOrigin::Ref(i) = en.origin else {
                    continue;
                };
                let resolved =
                    en.confidence != Confidence::Inferred && en.fanout.is_none_or(|n| n <= 1);
                let slot = want
                    .entry((key.from, key.kind.0.clone(), i))
                    .or_insert(resolved);
                *slot |= resolved;
            }
        }
        let mut got: BTreeMap<(Id, String, u32), bool> = BTreeMap::new();
        rx.for_each_ref_entry(&mut |f, k, i, r| {
            let slot = got.entry((f, k.to_string(), i)).or_insert(r);
            *slot |= r;
        })
        .unwrap();
        assert_eq!(got, want);
        assert!(got.len() >= 4);
    }

    #[test]
    fn map_in_degree_identical() {
        let e = graph();
        let rx = e.cache.read().unwrap();
        let mut degree: BTreeMap<Id, f64> = BTreeMap::new();
        for (key, row) in rx.links_all().unwrap() {
            *degree.entry(key.to).or_default() += row_weight(&row);
        }
        let entries = katalogr_query::map(&rx).unwrap();
        assert!(entries.iter().any(|m| m.in_degree > 0.0));
        for m in &entries {
            let want = degree.get(&Id::of(&m.symbol.name)).copied().unwrap_or(0.0);
            assert_eq!(
                m.in_degree,
                (want * 100.0).round() / 100.0,
                "{}",
                m.symbol.name
            );
        }
    }
}
