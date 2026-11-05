//! FEAT-087: `why_use` lists who uses a symbol; `sources` name the owner of each section.

use camino::Utf8PathBuf;
use katalogr_model::insight::{FACT_RATIONALE, FACT_TEST, TEST_CASE};
use katalogr_model::{
    ArtifactName, Confidence, ContractName, ContractRole, Fact, Fragment, Id, LinkKind, Name,
    Provenance, RelPath, Segment, Span, SymbolName,
};
use katalogr_query::{Blame, Resolution, SymbolHit, attach_use, why, why_sources, why_use};
use katalogr_store::{
    ArtifactRow, BindingRow, Cache, ContractRow, Kindex, LinkEntry, LinkKey, LinkOrigin, LinkRow,
    ProjectConfig, ProjectName, Reader, RepoConfig, Restitch, StoreError, SymbolRow, WriteTx,
};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

struct NoShards;

impl Restitch for NoShards {
    fn replace_fragment(&self, _: &mut WriteTx, _: Fragment) -> Result<(), StoreError> {
        Ok(())
    }
    fn remove_artifact(&self, _: &mut WriteTx, _: &ArtifactName) -> Result<(), StoreError> {
        Ok(())
    }
}

fn span(line: u32) -> Span {
    Span {
        start_line: line,
        start_col: 1,
        end_line: line,
        end_col: 10,
    }
}

fn artifact() -> ArtifactName {
    ArtifactName {
        repo: "r".into(),
        path: RelPath::new("f.tx").unwrap(),
    }
}

fn sym(kind: &str, name: &str) -> SymbolName {
    SymbolName {
        artifact: artifact(),
        segments: vec![Segment {
            kind: kind.into(),
            name: name.into(),
        }],
        ordinal: 1,
    }
}

fn open(tmp: &tempfile::TempDir) -> Cache {
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
    Cache::open_or_rebuild(&dir, &NoShards, Instant::now() + Duration::from_secs(60)).unwrap()
}

fn put_sym(
    tx: &mut WriteTx,
    ids: &mut Vec<Id>,
    kind: &str,
    name: &str,
    line: u32,
    facts: BTreeMap<String, Fact>,
) -> Id {
    let n = sym(kind, name);
    let id = n.id();
    tx.put_symbol(
        id,
        &SymbolRow {
            name: n,
            artifact: artifact().id(),
            kind: kind.into(),
            span: span(line),
            facts,
        },
    )
    .unwrap();
    ids.push(id);
    id
}

fn link(tx: &mut WriteTx, from: Id, kind: &str, to: Id) {
    tx.put_link(
        &LinkKey {
            from,
            kind: LinkKind(kind.into()),
            to,
        },
        &LinkRow {
            entries: vec![LinkEntry {
                origin: LinkOrigin::Intra,
                confidence: Confidence::Extracted,
                span: Some(span(1)),
                provenance: Provenance {
                    repo: "r".into(),
                    path: RelPath::new("f.tx").unwrap(),
                    revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
                },
                fanout: None,
                produced_by: None,
            }],
        },
    )
    .unwrap();
}

/// `target` (line 1) with `callers` callers, `tests` test cases that call it, and two contracts.
fn graph(cache: &Cache, callers: usize, tests: usize) -> SymbolHit {
    let mut tx = cache.write().unwrap();
    let mut ids = Vec::new();
    let target = put_sym(&mut tx, &mut ids, "fn", "target", 1, BTreeMap::new());
    for i in 0..callers {
        let c = put_sym(
            &mut tx,
            &mut ids,
            "fn",
            &format!("caller{i:02}"),
            10 + i as u32,
            BTreeMap::new(),
        );
        link(&mut tx, c, "calls", target);
    }
    for i in 0..tests {
        let facts = BTreeMap::from([(FACT_TEST.to_string(), Fact::Str(TEST_CASE.to_string()))]);
        let t = put_sym(
            &mut tx,
            &mut ids,
            "fn",
            &format!("check{i:02}"),
            100 + i as u32,
            facts,
        );
        link(&mut tx, t, "calls", target);
    }
    let mut rows = Vec::new();
    for key in ["zeta", "alpha"] {
        let name = ContractName {
            kind: "topic".into(),
            key: key.into(),
        };
        let cid = Id::of(&name.canonical());
        tx.put_contract(
            cid,
            &ContractRow {
                name,
                providers: 1,
                consumers: 0,
                declarers: 0,
            },
        )
        .unwrap();
        rows.push(BindingRow {
            symbol: target,
            contract: cid,
            role: ContractRole::Provides,
        });
    }
    tx.put_bindings(artifact().id(), &rows).unwrap();
    tx.commit().unwrap();
    let read = cache.read().unwrap();
    let row = read.symbol(target).unwrap().unwrap();
    SymbolHit {
        name: row.name.canonical(),
        kind: row.kind,
        path: "f.tx".into(),
        span: row.span,
    }
}

mod why {
    use super::*;

    #[test]
    fn use_lists_callers_tests_contracts() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = open(&tmp);
        let target = graph(&cache, 2, 1);
        let u = why_use(&cache.read().unwrap(), &target, 10).unwrap();
        let callers: Vec<&str> = u.callers.iter().map(|c| c.caller.name.as_str()).collect();
        // A test that calls the symbol is also a caller.
        assert_eq!(
            callers,
            [
                "r:f.tx#fn:caller00",
                "r:f.tx#fn:caller01",
                "r:f.tx#fn:check00"
            ]
        );
        assert_eq!(u.callers_total, 3);
        assert!(u.callers.iter().all(|c| c.depth == 1));
        assert_eq!(u.tests.len(), 1);
        assert_eq!(u.tests[0].test.name, "r:f.tx#fn:check00");
        assert_eq!(u.tests_total, 1);
        // Sorted, canonical.
        assert_eq!(u.contracts, ["contract:topic:alpha", "contract:topic:zeta"]);
    }

    #[test]
    fn use_capped_counts_totals() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = open(&tmp);
        let target = graph(&cache, 5, 4);
        let read = cache.read().unwrap();
        let full = why_use(&read, &target, 100).unwrap();
        let cut = why_use(&read, &target, 3).unwrap();
        assert_eq!((cut.callers.len(), cut.callers_total), (3, 9));
        assert_eq!((cut.tests.len(), cut.tests_total), (3, 4));
        // The cut keeps the head of the full order.
        assert_eq!(cut.callers[..], full.callers[..3]);
        assert_eq!(cut.tests[..], full.tests[..3]);
        assert_eq!(cut.contracts, full.contracts);
    }

    #[test]
    fn sources_name_owners() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = open(&tmp);
        let target = graph(&cache, 1, 0);
        // The symbol gets a comment note and a link to an item, and the artifact row is stored.
        let mut tx = cache.write().unwrap();
        let mut ids = Vec::new();
        let item = put_sym(&mut tx, &mut ids, "item", "story", 50, BTreeMap::new());
        let me = Id::of(&target.name);
        let mut row = tx.symbol(me).unwrap().unwrap();
        row.facts.insert(
            FACT_RATIONALE.into(),
            Fact::List(vec![Fact::Map(BTreeMap::from([
                ("marker".to_string(), Fact::Str("WHY".into())),
                ("line".to_string(), Fact::Int(2)),
                ("text".to_string(), Fact::Str("because".into())),
            ]))]),
        );
        tx.put_symbol(me, &row).unwrap();
        link(&mut tx, me, "refs-item", item);
        let mut all: Vec<Id> = tx.symbols().unwrap().into_iter().map(|(i, _)| i).collect();
        all.sort();
        tx.put_artifact(
            artifact().id(),
            &ArtifactRow {
                name: artifact(),
                extension: "tx".into(),
                provenance: Provenance {
                    repo: "r".into(),
                    path: RelPath::new("f.tx").unwrap(),
                    revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
                },
                facts: BTreeMap::new(),
                symbols: all,
                exports: Vec::new(),
                refs: Vec::new(),
            },
        )
        .unwrap();
        tx.commit().unwrap();

        let read = cache.read().unwrap();
        let path = RelPath::new("f.tx").unwrap();
        let Resolution::One(mut w) = why(&read, "r", &path, 1, &Blame::NotRequested).unwrap()
        else {
            panic!("one symbol at line 1")
        };
        let owners = |w: &katalogr_query::WhyResult| -> Vec<(String, String, String)> {
            w.sources
                .iter()
                .map(|s| (s.section.clone(), s.owner.clone(), s.source.clone()))
                .collect()
        };
        let row = |a: &str, b: &str| (a.to_string(), b.to_string(), "index".to_string());
        assert_eq!(
            owners(&w),
            [
                row("symbol", "index"),
                row("notes", "index"),
                row("rationale", "board")
            ]
        );
        let used = why_use(&read, &w.symbol, 10).unwrap();
        attach_use(&mut w, used);
        assert_eq!(
            owners(&w),
            [
                row("symbol", "index"),
                row("used_by", "index"),
                row("notes", "index"),
                row("rationale", "board")
            ]
        );
        assert_eq!(why_sources(&w), w.sources);
        // With commits present the owner is `both`.
        w.commits.push(katalogr_query::CommitHit {
            sha: "a".repeat(40),
            summary: "s".into(),
            items: vec![],
        });
        let src = why_sources(&w);
        let commits = src.iter().find(|s| s.section == "commits").unwrap();
        assert_eq!(
            (commits.owner.as_str(), commits.source.as_str()),
            ("both", "index")
        );
    }
}
