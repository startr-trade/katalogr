//! Coverage computation over hand-built caches (FEAT-062).

mod coverage_calc {
    use camino::Utf8PathBuf;
    use katalogr_analytics::metrics::{Better, metric_key};
    use katalogr_analytics::{CoverageInput, CoverageReport, coverage};
    use katalogr_model::{
        ArtifactName, Confidence, ContractName, ContractRole, ExportKey, Fragment, Id, LinkKind,
        Name, OutRef, Provenance, RelPath,
    };
    use katalogr_store::{
        ArtifactRow, BindingRow, Cache, ContractRow, ErrorRow, Kindex, LinkEntry, LinkKey,
        LinkOrigin, LinkRow, ProjectConfig, ProjectName, RefId, RepoConfig, Restitch, StoreError,
        WriteTx,
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

    struct Fx {
        _tmp: tempfile::TempDir,
        cache: Cache,
    }

    fn fx() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let dir = Kindex::init(&root)
            .unwrap()
            .create_project(
                &ProjectName::new("cov").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("a"), RepoConfig::new("b")],
                    ..Default::default()
                },
            )
            .unwrap();
        let cache =
            Cache::open_or_rebuild(&dir, &NoShards, Instant::now() + Duration::from_secs(30))
                .unwrap();
        Fx { _tmp: tmp, cache }
    }

    fn path(p: &str) -> RelPath {
        RelPath::new(p).unwrap()
    }

    fn aname(repo: &str, p: &str) -> ArtifactName {
        ArtifactName {
            repo: repo.into(),
            path: path(p),
        }
    }

    fn prov(repo: &str, p: &str) -> Provenance {
        Provenance {
            repo: repo.into(),
            path: path(p),
            revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
        }
    }

    fn out_ref(from: Id) -> OutRef {
        OutRef {
            from,
            kind: LinkKind("calls".into()),
            span: None,
            candidates: vec![ExportKey {
                namespace: "ns".into(),
                key: "k".into(),
            }],
            confidence: Confidence::Extracted,
            origin: None,
            binds: None,
        }
    }

    /// An artifact with `nrefs` references, all from one made-up symbol id.
    fn put_artifact(tx: &mut WriteTx, repo: &str, p: &str, ext: &str, nrefs: usize) -> (Id, Id) {
        let name = aname(repo, p);
        let id = name.id();
        let from = Id::of(&format!("{repo}:{p}#sym"));
        tx.put_artifact(
            id,
            &ArtifactRow {
                name,
                extension: ext.into(),
                provenance: prov(repo, p),
                facts: BTreeMap::new(),
                symbols: Vec::new(),
                exports: Vec::new(),
                refs: (0..nrefs).map(|_| out_ref(from)).collect(),
            },
        )
        .unwrap();
        (id, from)
    }

    fn put_ref_link(
        tx: &mut WriteTx,
        repo: &str,
        p: &str,
        from: Id,
        to: &str,
        index: u32,
        (confidence, fanout): (Confidence, Option<u32>),
    ) {
        tx.put_link(
            &LinkKey {
                from,
                kind: LinkKind("calls".into()),
                to: Id::of(to),
            },
            &LinkRow {
                entries: vec![LinkEntry {
                    origin: LinkOrigin::Ref(index),
                    confidence,
                    span: None,
                    provenance: prov(repo, p),
                    fanout,
                    produced_by: None,
                }],
            },
        )
        .unwrap();
    }

    fn input<'a>(
        language: &'a dyn Fn(&str, &RelPath) -> String,
        claim: &'a dyn Fn(&RelPath) -> Option<(String, String)>,
    ) -> CoverageInput<'a> {
        CoverageInput {
            language,
            claim,
            unclaimed: BTreeMap::new(),
            roots_missing: Vec::new(),
            exclude_prefixes: vec!["@history/".into()],
            repo: None,
            suffix_top: 20,
        }
    }

    fn lang(ext: &str, _: &RelPath) -> String {
        format!("{ext}-lang")
    }

    fn claim_x(p: &RelPath) -> Option<(String, String)> {
        p.as_str()
            .ends_with(".x")
            .then(|| ("xx".to_string(), "x-lang".to_string()))
    }

    fn run(f: &Fx, mut i: CoverageInput) -> CoverageReport {
        let tx = f.cache.read().unwrap();
        i.suffix_top = 20;
        coverage(&tx, &i).unwrap()
    }

    #[test]
    fn refs_classified() {
        let f = fx();
        {
            let mut tx = f.cache.write().unwrap();
            // Five references: one target, two targets, inferred with one target, no target,
            // and one that is neither linked nor recorded unresolved.
            let (aid, from) = put_artifact(&mut tx, "a", "src/one.x", "xx", 5);
            put_ref_link(
                &mut tx,
                "a",
                "src/one.x",
                from,
                "t1",
                0,
                (Confidence::Extracted, None),
            );
            put_ref_link(
                &mut tx,
                "a",
                "src/one.x",
                from,
                "t2a",
                1,
                (Confidence::Inferred, Some(2)),
            );
            put_ref_link(
                &mut tx,
                "a",
                "src/one.x",
                from,
                "t2b",
                1,
                (Confidence::Inferred, Some(2)),
            );
            put_ref_link(
                &mut tx,
                "a",
                "src/one.x",
                from,
                "t3",
                2,
                (Confidence::Inferred, Some(1)),
            );
            tx.put_unresolved(
                RefId {
                    artifact: aid,
                    index: 3,
                },
                &out_ref(from),
            )
            .unwrap();
            // Another repository, another extension: one resolved reference.
            let (_, from_b) = put_artifact(&mut tx, "b", "lib/two.y", "yy", 1);
            put_ref_link(
                &mut tx,
                "b",
                "lib/two.y",
                from_b,
                "t9",
                0,
                (Confidence::Extracted, Some(1)),
            );
            // A history commit is not a file.
            put_artifact(&mut tx, "a", "@history/abc", "hist", 2);
            tx.commit().unwrap();
        }
        let l = |e: &str, p: &RelPath| lang(e, p);
        let r = run(&f, input(&l, &claim_x));
        let t = &r.totals;
        assert_eq!(t.files_indexed, 2);
        assert_eq!(t.refs_total, 6);
        assert_eq!(t.refs_resolved, 2);
        assert_eq!(t.refs_inferred, 2);
        assert_eq!(t.refs_unresolved, 2);
        assert_eq!(t.refs_unlinked, 1);
        assert_eq!(t.resolution_rate, Some(0.333333));
        assert_eq!(t.inferred_rate, Some(0.333333));
        assert_eq!(t.unresolved_rate, Some(0.333333));
        assert_eq!(r.groups.len(), 2);
        assert_eq!(r.groups[0].repo.as_deref(), Some("a"));
        assert_eq!(r.groups[0].extension.as_deref(), Some("xx"));
        assert_eq!(r.groups[0].language.as_deref(), Some("xx-lang"));
        assert_eq!(r.groups[0].counts.refs_total, 5);
        assert_eq!(r.by_repo.len(), 2);
        assert_eq!(r.by_extension.len(), 2);
        assert_eq!(r.by_language.len(), 2);
        assert_eq!(r.by_repo[1].counts.refs_resolved, 1);
        assert_eq!(r.by_link_kind.len(), 1);
        assert_eq!(r.by_link_kind[0].kind, "calls");
        assert_eq!(r.by_link_kind[0].total, 6);

        // One repo only.
        let mut i = input(&l, &claim_x);
        i.repo = Some("b".into());
        let r = run(&f, i);
        assert_eq!(r.totals.files_indexed, 1);
        assert_eq!(r.totals.refs_total, 1);
        // No references, no rates.
        let tmp = fx();
        let r = run(&tmp, input(&l, &claim_x));
        assert_eq!(r.totals.resolution_rate, None);
    }

    #[test]
    fn dangling_contracts_counted() {
        let f = fx();
        {
            let mut tx = f.cache.write().unwrap();
            let (a1, s1) = put_artifact(&mut tx, "a", "p.x", "xx", 0);
            let (a2, s2) = put_artifact(&mut tx, "b", "c.y", "yy", 0);
            let contract = |kind: &str, key: &str| ContractName {
                kind: kind.into(),
                key: key.into(),
            };
            let put = |tx: &mut WriteTx, kind: &str, key: &str, p: u32, c: u32, d: u32| {
                let name = contract(kind, key);
                let id = Id::of(&format!("contract:{kind}:{key}"));
                tx.put_contract(
                    id,
                    &ContractRow {
                        name,
                        providers: p,
                        consumers: c,
                        declarers: d,
                    },
                )
                .unwrap();
                id
            };
            let provider_only = put(&mut tx, "wasm", "only-p", 1, 0, 0);
            let consumer_only = put(&mut tx, "wasm", "only-c", 0, 1, 0);
            let both = put(&mut tx, "rest", "both", 1, 1, 0);
            let declares = put(&mut tx, "rest", "decl", 0, 0, 1);
            let also_dangling = put(&mut tx, "queue", "q", 0, 2, 0);
            let b = |symbol, contract, role| BindingRow {
                symbol,
                contract,
                role,
            };
            tx.put_bindings(
                a1,
                &[
                    b(s1, provider_only, ContractRole::Provides),
                    b(s1, both, ContractRole::Provides),
                    b(s1, declares, ContractRole::Declares),
                    b(s1, also_dangling, ContractRole::Consumes),
                ],
            )
            .unwrap();
            tx.put_bindings(
                a2,
                &[
                    b(s2, consumer_only, ContractRole::Consumes),
                    b(s2, both, ContractRole::Consumes),
                    b(s2, also_dangling, ContractRole::Consumes),
                ],
            )
            .unwrap();
            tx.commit().unwrap();
        }
        let l = |e: &str, p: &RelPath| lang(e, p);
        let r = run(&f, input(&l, &claim_x));
        assert_eq!(r.totals.contracts_dangling, 3);
        let kinds: Vec<(&str, u64)> = r
            .dangling_contracts_by_kind
            .iter()
            .map(|k| (k.kind.as_str(), k.dangling))
            .collect();
        assert_eq!(kinds, [("queue", 1), ("wasm", 2)]);
        let by_repo: Vec<(&str, u64)> = r
            .by_repo
            .iter()
            .map(|g| (g.repo.as_deref().unwrap(), g.counts.contracts_dangling))
            .collect();
        // `queue/q` is bound by both repos but counted once in the total.
        assert_eq!(by_repo, [("a", 2), ("b", 2)]);
        let mut i = input(&l, &claim_x);
        i.repo = Some("b".into());
        let r = run(&f, i);
        assert_eq!(r.totals.contracts_dangling, 2);
    }

    #[test]
    fn parse_errors_unsupported_failed() {
        let f = fx();
        {
            let mut tx = f.cache.write().unwrap();
            put_artifact(&mut tx, "a", "ok.x", "xx", 0);
            put_artifact(&mut tx, "a", "bad.x", "xx", 0);
            let row = |m: &str| ErrorRow {
                span: None,
                message: m.into(),
            };
            // Two parse errors on an indexed file.
            tx.put_index_errors(
                "a",
                &path("bad.x"),
                &[row("parse error: x"), row("parse error: y")],
            )
            .unwrap();
            // No artifact: unsupported, failed, and unclaimed by any extension.
            tx.put_index_errors("a", &path("refused.x"), &[row("unsupported: too new")])
                .unwrap();
            tx.put_index_errors("a", &path("broken.x"), &[row("io error")])
                .unwrap();
            tx.put_index_errors("b", &path("mystery.zzz"), &[row("io error")])
                .unwrap();
            tx.put_index_errors("a", &path("@history/abc"), &[row("io error")])
                .unwrap();
            tx.commit().unwrap();
        }
        let l = |e: &str, p: &RelPath| lang(e, p);
        let r = run(&f, input(&l, &claim_x));
        let t = &r.totals;
        assert_eq!(t.files_indexed, 2);
        assert_eq!(t.files_parse_errors, 1);
        assert_eq!(t.parse_errors, 2);
        assert_eq!(t.parse_error_rate, Some(0.5));
        assert_eq!(t.files_unsupported, 1);
        assert_eq!(t.files_failed, 2);
        let none = r
            .groups
            .iter()
            .find(|g| g.extension.as_deref() == Some("(none)"))
            .unwrap();
        assert_eq!(none.repo.as_deref(), Some("b"));
        assert_eq!(none.counts.files_failed, 1);
    }

    #[test]
    fn metrics_emitted() {
        let f = fx();
        {
            let mut tx = f.cache.write().unwrap();
            let (_, from) = put_artifact(&mut tx, "a", "one.x", "xx", 2);
            put_ref_link(
                &mut tx,
                "a",
                "one.x",
                from,
                "t1",
                0,
                (Confidence::Extracted, None),
            );
            tx.commit().unwrap();
        }
        let l = |e: &str, p: &RelPath| lang(e, p);
        let mut i = input(&l, &claim_x);
        i.unclaimed.insert(
            "a".into(),
            vec![path("x.svg"), path("y.svg"), path("Makefile")],
        );
        let r = run(&f, i);
        assert_eq!(r.totals.files_unclaimed, 3);
        assert_eq!(r.unclaimed[0].files, 3);
        let sfx: Vec<(&str, u64)> = r.unclaimed[0]
            .suffixes
            .iter()
            .map(|s| (s.suffix.as_str(), s.files))
            .collect();
        assert_eq!(sfx, [(".svg", 2), ("(none)", 1)]);
        let get = |key: &str| r.metrics.iter().find(|m| metric_key(m) == key);
        let m = get("coverage.resolution_rate").unwrap();
        assert_eq!((m.value, m.better), (0.5, Better::Higher));
        let m = get("coverage.unresolved_rate").unwrap();
        assert_eq!((m.value, m.better), (0.5, Better::Lower));
        assert_eq!(get("coverage.files.indexed").unwrap().better, Better::Info);
        assert_eq!(get("coverage.files.unclaimed").unwrap().value, 3.0);
        assert_eq!(get("coverage.files.failed").unwrap().better, Better::Lower);
        assert_eq!(get("coverage.contracts.dangling").unwrap().value, 0.0);
        // Labelled rollups; unclaimed only for the totals and repos.
        assert!(get("coverage.resolution_rate{repo=a}").is_some());
        assert!(get("coverage.resolution_rate{extension=xx}").is_some());
        assert!(get("coverage.resolution_rate{language=xx-lang}").is_some());
        assert!(get("coverage.files.unclaimed{repo=a}").is_some());
        assert!(get("coverage.files.unclaimed{extension=xx}").is_none());
        // No parse-error rate without indexed files is not exercised here; with files it is 0.
        assert_eq!(get("coverage.parse_error_rate").unwrap().value, 0.0);
        // Sorted by key.
        let keys: Vec<String> = r.metrics.iter().map(metric_key).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }
}
