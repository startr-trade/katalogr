mod scoring {
    use katalogr_analytics::eval::*;
    use katalogr_analytics::metrics::{Better, MetricSample, Tolerance, metric_key};
    use std::collections::BTreeMap;
    use std::path::Path;

    fn write(dir: &Path, name: &str, text: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    const GOOD: &str = r#"
format: 1
set: core
version: 2
description: test
repos:
  - {name: app, path: repos/app}
queries:
  - id: q01
    op: search
    input: ledger balance
    scope: {repos: [app], paths: ["src/**"]}
    tags: [phrase]
    evidence:
      - {name: "app:src/ledger.rs#fn:balance", grade: 3}
      - {name: "app:docs/ledger.md", grade: 1}
  - id: q02
    op: map
    evidence:
      - {name: "app:src/ledger.rs#fn:balance", grade: 2}
  - id: q03
    op: explain
    input: zebra
    route: no-match
"#;

    #[test]
    fn golden_set_parses() {
        let tmp = tempfile::tempdir().unwrap();
        let f = write(tmp.path(), "golden.yaml", GOOD);
        let (set, dir) = load_set(&f).unwrap();
        assert_eq!(set.set, "core");
        assert_eq!(set.version, 2);
        assert_eq!(set.queries.len(), 3);
        assert_eq!(set.queries[0].op, Op::Search);
        assert_eq!(set.queries[0].route, Route::Found);
        assert_eq!(set.queries[1].input, None);
        assert_eq!(set.queries[2].route, Route::NoMatch);
        assert_eq!(set.repos[0].name, "app");
        assert_eq!(dir, tmp.path());
        let all = load_sets(tmp.path()).unwrap();
        assert_eq!(all.len(), 1);
    }

    fn bad(body: &str) -> String {
        let tmp = tempfile::tempdir().unwrap();
        let text = format!("format: 1\nset: s\nversion: 1\nqueries:\n{body}");
        let f = write(tmp.path(), "golden.yaml", &text);
        load_set(&f).expect_err("must be refused").to_string()
    }

    #[test]
    fn golden_set_rejects_invalid() {
        let e = bad("  - {id: q1, op: search, input: x, evidence: [{name: a, grade: 4}]}\n");
        assert!(
            e.contains("golden.yaml") && e.contains("q1") && e.contains("grade"),
            "{e}"
        );
        let e = bad(
            "  - {id: q1, op: search, input: x, evidence: [{name: a, grade: 1}]}\n  - {id: q1, op: map, evidence: [{name: a, grade: 1}]}\n",
        );
        assert!(e.contains("q1") && e.contains("duplicate"), "{e}");
        let e = bad(
            "  - {id: q2, op: search, input: x, route: no-match, evidence: [{name: a, grade: 1}]}\n",
        );
        assert!(e.contains("q2") && e.contains("evidence"), "{e}");
        let e = bad("  - {id: q3, op: pack, input: x}\n");
        assert!(e.contains("pack"), "{e}");
        let e = bad(
            "  - {id: q4, op: search, input: x, colour: red, evidence: [{name: a, grade: 1}]}\n",
        );
        assert!(e.contains("colour") || e.contains("unknown field"), "{e}");
        let e = bad("  - {id: q5, op: search, evidence: [{name: a, grade: 1}]}\n");
        assert!(e.contains("q5") && e.contains("input"), "{e}");
        let e = bad("  - {id: q6, op: map, input: x, evidence: [{name: a, grade: 1}]}\n");
        assert!(e.contains("q6") && e.contains("input"), "{e}");
        let e = bad(
            "  - {id: q7, op: search, input: x, scope: {paths: ['[']}, evidence: [{name: a, grade: 1}]}\n",
        );
        assert!(e.contains("q7") && e.contains("glob"), "{e}");
        let e = bad(
            "  - {id: q8, op: search, input: x, evidence: [{name: a, grade: 1}, {name: a, grade: 2}]}\n",
        );
        assert!(e.contains("q8") && e.contains("duplicate"), "{e}");
        let tmp = tempfile::tempdir().unwrap();
        let f = write(
            tmp.path(),
            "golden.yaml",
            "format: 1\nset: s\nversion: 1\nrepos:\n  - {name: a, path: ../x}\nqueries: []\n",
        );
        let e = load_set(&f).unwrap_err().to_string();
        assert!(e.contains(".."), "{e}");
        let f = write(
            tmp.path(),
            "golden.yaml",
            "format: 2\nset: s\nversion: 1\nqueries: []\n",
        );
        assert!(load_set(&f).unwrap_err().to_string().contains("format"));
    }

    fn query(id: &str, op: Op, ev: &[(&str, u8)]) -> GoldenQuery {
        GoldenQuery {
            id: id.to_string(),
            op,
            input: Some("x".to_string()),
            route: if ev.is_empty() {
                Route::NoMatch
            } else {
                Route::Found
            },
            scope: None,
            tags: vec![],
            evidence: ev
                .iter()
                .map(|(n, g)| Evidence {
                    name: n.to_string(),
                    grade: *g,
                })
                .collect(),
        }
    }

    fn item(name: &str, tokens: u64) -> RankedItem {
        RankedItem {
            name: name.to_string(),
            path: None,
            tokens,
        }
    }

    fn run(items: Vec<RankedItem>) -> QueryRun {
        QueryRun {
            route: Route::Found,
            items,
            missing_evidence: vec![],
        }
    }

    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-6, "{a} != {b}");
    }

    #[test]
    fn hit_mrr_ndcg_hand_computed() {
        // Grades 3, 2, 1 at ranks 1, 3, 6; gains 7, 3, 1.
        let q = query("q", Op::Search, &[("a", 3), ("b", 2), ("c", 1)]);
        let items = vec![
            item("a", 1),
            item("x1", 1),
            item("b", 1),
            item("x2", 1),
            item("x3", 1),
            item("c", 1),
        ];
        let r = score_query("s", &q, &run(items), &[1, 5, 10]);
        assert_eq!(r.hit["1"], 1.0);
        assert_eq!(r.hit["5"], 1.0);
        assert_eq!(r.rr, Some(1.0));
        // ndcg@5 = (7 + 3/log2(4)) / (7 + 3/log2(3) + 1/log2(4))
        let dcg5 = 7.0 + 3.0 / 4f64.log2();
        let idcg5 = 7.0 + 3.0 / 3f64.log2() + 1.0 / 4f64.log2();
        near(r.ndcg["5"], dcg5 / idcg5);
        near(r.ndcg["1"], 1.0);
        // ndcg@10 adds the grade-1 item at rank 6: 1 / log2(7).
        let dcg10 = dcg5 + 1.0 / 7f64.log2();
        near(r.ndcg["10"], dcg10 / idcg5);
        assert_eq!(r.relevant_in_k, 3);
        // A miss at rank 1 only.
        let r = score_query("s", &q, &run(vec![item("x", 1), item("b", 1)]), &[1, 5]);
        assert_eq!(r.hit["1"], 0.0);
        assert_eq!(r.hit["5"], 1.0);
        assert_eq!(r.rr, Some(0.5));
        // A query without evidence has no rank metrics.
        let r = score_query("s", &query("n", Op::Search, &[]), &run(vec![]), &[1]);
        assert!(r.hit.is_empty() && r.rr.is_none());
    }

    fn result(
        set: &str,
        id: &str,
        op: Op,
        ev: &[(&str, u8)],
        items: Vec<RankedItem>,
    ) -> QueryResult {
        score_query(set, &query(id, op, ev), &run(items), &[1, 5])
    }

    fn val(ms: &[MetricSample], key: &str) -> f64 {
        ms.iter()
            .find(|m| metric_key(m) == key)
            .unwrap_or_else(|| {
                panic!(
                    "no {key} in {:?}",
                    ms.iter().map(metric_key).collect::<Vec<_>>()
                )
            })
            .value
    }

    #[test]
    fn aggregate_groups_and_labels() {
        let rs = vec![
            result(
                "a",
                "q1",
                Op::Search,
                &[("a:x#fn:f", 3)],
                vec![item("a:x#fn:f", 10)],
            ),
            result("a", "q2", Op::Map, &[("a:x#fn:f", 3)], vec![item("z", 10)]),
            result(
                "b",
                "q1",
                Op::Search,
                &[("b:y#fn:g", 3), ("c:y#fn:h", 1)],
                vec![item("b:y#fn:g", 10)],
            ),
        ];
        let ms = aggregate(&rs, &[1, 5]);
        let keys: Vec<String> = ms.iter().map(metric_key).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        near(val(&ms, "retrieval.hit@1{op=search,set=a}"), 1.0);
        near(val(&ms, "retrieval.hit@1{op=map,set=a}"), 0.0);
        near(val(&ms, "retrieval.hit@1{set=a}"), 0.5);
        near(val(&ms, "retrieval.hit@1"), 2.0 / 3.0);
        near(val(&ms, "retrieval.hit@1{scope=cross}"), 1.0);
        near(val(&ms, "retrieval.hit@1{scope=within}"), 0.5);
        near(val(&ms, "retrieval.queries"), 3.0);
        assert_eq!(
            ms.iter()
                .find(|m| m.name == "retrieval.queries")
                .unwrap()
                .better,
            Better::Info
        );
        assert!(ms.iter().any(|m| metric_key(m) == "retrieval.mrr{set=b}"));
    }

    #[test]
    fn tokens_per_relevant_hit_micro_average() {
        let rs = vec![
            result(
                "s",
                "q1",
                Op::Search,
                &[("a", 3)],
                vec![item("a", 10), item("x", 30)],
            ),
            result("s", "q2", Op::Search, &[("b", 3)], vec![item("x", 20)]),
            result(
                "s",
                "q3",
                Op::Search,
                &[("c", 3), ("d", 2)],
                vec![item("c", 5), item("d", 5)],
            ),
        ];
        let ms = aggregate(&rs, &[5]);
        // Tokens 40 + 20 + 10 over 1 + 0 + 2 relevant items.
        near(val(&ms, "retrieval.tokens_per_relevant_hit"), 70.0 / 3.0);
        // No relevant item at all: no sample, no infinity.
        let none = vec![result(
            "s",
            "q",
            Op::Search,
            &[("a", 3)],
            vec![item("x", 9)],
        )];
        let ms = aggregate(&none, &[5]);
        assert!(
            ms.iter()
                .all(|m| m.name != "retrieval.tokens_per_relevant_hit")
        );
    }

    #[test]
    fn route_and_scope_scored() {
        let mut q = query("q", Op::Search, &[("app:src/a.rs#fn:a", 3)]);
        q.scope = Some(ScopeExpect {
            repos: vec!["app".to_string()],
            paths: vec!["src/**".to_string()],
        });
        let items = vec![
            RankedItem {
                name: "app:src/a.rs#fn:a".to_string(),
                path: Some("src/a.rs".to_string()),
                tokens: 1,
            },
            // A contract never leaks.
            item("contract:wasm:other", 1),
            // A hex id never leaks.
            item("00ff00ff00ff", 1),
            // A wrong repository leaks.
            RankedItem {
                name: "other:src/b.rs#fn:b".to_string(),
                path: Some("src/b.rs".to_string()),
                tokens: 1,
            },
            // A path outside the glob leaks.
            RankedItem {
                name: "app:docs/c.md".to_string(),
                path: Some("docs/c.md".to_string()),
                tokens: 1,
            },
            // Unknown path: no leak.
            item("app:src/d.rs#fn:d", 1),
        ];
        let r = score_query("s", &q, &run(items), &[10]);
        assert_eq!(r.scope_leaks, Some(2));
        assert!(r.route_ok);
        let wrong = QueryRun {
            route: Route::Ambiguous,
            items: vec![],
            missing_evidence: vec![],
        };
        let r2 = score_query("s", &q, &wrong, &[10]);
        assert!(!r2.route_ok);
        assert_eq!(r2.route_actual, Route::Ambiguous);
        let ms = aggregate(&[r, r2], &[10]);
        near(val(&ms, "retrieval.scope_leak_rate"), 2.0 / 6.0);
        near(val(&ms, "retrieval.route_accuracy"), 0.5);
    }

    fn report(
        sets: &[(&str, u32)],
        metrics: Vec<MetricSample>,
        queries: Vec<QueryResult>,
    ) -> EvalReport {
        EvalReport {
            format: BASELINE_FORMAT,
            mode: EvalMode::Fixture,
            ks: vec![1, 5],
            params: describe(&EvalParams::default()),
            sets: sets
                .iter()
                .map(|(s, v)| SetSummary {
                    set: s.to_string(),
                    version: *v,
                    queries: 1,
                })
                .collect(),
            metrics,
            queries,
            comparison: None,
        }
    }

    #[test]
    fn compare_flags_regressions() {
        let tol = default_tolerance;
        let s = |name: &str, v: f64, b: Better| MetricSample::new(name, v, b).label("set", "a");
        let hit = result("a", "q1", Op::Search, &[("x", 3)], vec![item("x", 10)]);
        let miss = result("a", "q1", Op::Search, &[("x", 3)], vec![item("y", 10)]);
        let base = report(
            &[("a", 1), ("gone", 1)],
            vec![
                s("retrieval.hit@5", 1.0, Better::Higher),
                s("retrieval.tokens_per_relevant_hit", 100.0, Better::Lower),
            ],
            vec![hit.clone()],
        );
        // No change.
        let c = compare_report(&base, &base, &tol).unwrap();
        assert!(c.failed == !c.missing_sets.is_empty());
        let same = report(&[("a", 1)], base.metrics.clone(), vec![hit.clone()]);
        let c = compare_report(&base, &same, &tol).unwrap();
        assert_eq!(c.missing_sets, vec!["gone".to_string()]);
        assert!(c.failed);
        let base = report(&[("a", 1)], base.metrics.clone(), vec![hit.clone()]);
        assert!(!compare_report(&base, &base, &tol).unwrap().failed);
        // Higher drops: metric and query regression.
        let cur = report(
            &[("a", 1)],
            vec![
                s("retrieval.hit@5", 0.5, Better::Higher),
                s("retrieval.tokens_per_relevant_hit", 109.0, Better::Lower),
            ],
            vec![miss.clone()],
        );
        let c = compare_report(&base, &cur, &tol).unwrap();
        assert!(c.failed);
        assert_eq!(c.metrics.regressions.len(), 1, "109 is within 10%");
        assert!(c.queries.iter().any(|r| r.metric == "hit@5"));
        assert!(c.queries.iter().any(|r| r.metric == "route") == false);
        // Lower rises past 10 %.
        let cur = report(
            &[("a", 1)],
            vec![
                s("retrieval.hit@5", 1.0, Better::Higher),
                s("retrieval.tokens_per_relevant_hit", 111.0, Better::Lower),
            ],
            vec![hit.clone()],
        );
        let c = compare_report(&base, &cur, &tol).unwrap();
        assert_eq!(c.metrics.regressions.len(), 1);
        assert!(c.failed);
        // Within tolerance: passes.
        let cur = report(
            &[("a", 1)],
            vec![
                s("retrieval.hit@5", 1.0, Better::Higher),
                s("retrieval.tokens_per_relevant_hit", 109.0, Better::Lower),
            ],
            vec![hit.clone()],
        );
        assert!(!compare_report(&base, &cur, &tol).unwrap().failed);
        // New set: reported, no failure. Version change: failure.
        let cur = report(
            &[("a", 1), ("b", 1)],
            base.metrics.clone(),
            vec![hit.clone()],
        );
        let c = compare_report(&base, &cur, &tol).unwrap();
        assert_eq!(c.new_sets, vec!["b".to_string()]);
        assert!(!c.failed);
        let cur = report(&[("a", 2)], base.metrics.clone(), vec![hit.clone()]);
        let c = compare_report(&base, &cur, &tol).unwrap();
        assert_eq!(c.version_mismatch, vec!["a".to_string()]);
        assert!(c.failed);
        // Other parameters: refused.
        let mut cur = report(&[("a", 1)], base.metrics.clone(), vec![hit]);
        cur.params.insert("search.k1".to_string(), 2.0);
        let e = compare_report(&base, &cur, &tol).unwrap_err().to_string();
        assert!(e.contains("search.k1"), "{e}");
        let _ = Tolerance { abs: 0.0, rel: 0.0 };
    }

    #[test]
    fn sweep_grid_and_parse() {
        assert!(parse_sweep("search.nope=1").is_err());
        assert!(parse_sweep("search.k1").is_err());
        assert!(parse_sweep("search.k1=").is_err());
        assert!(parse_sweep("search.k1=abc").is_err());
        assert!(parse_sweep("search.k1=-1").is_err());
        assert!(parse_sweep("search.k1=inf").is_err());
        assert!(parse_sweep("search.min_prefix=3.5").is_err());
        assert_eq!(
            parse_sweep("search.min_prefix=3,4.0").unwrap().values,
            vec![3.0, 4.0]
        );
        let base = EvalParams::default();
        let g = grid(
            &base,
            &[
                parse_sweep("search.k1=1.0,1.2").unwrap(),
                parse_sweep("search.b=0.5,0.6,0.75").unwrap(),
            ],
        )
        .unwrap();
        assert_eq!(g.len(), 6);
        let pairs: Vec<(f64, f64)> = g.iter().map(|p| (p.search.k1, p.search.b)).collect();
        assert_eq!(
            pairs,
            vec![
                (1.0, 0.5),
                (1.0, 0.6),
                (1.0, 0.75),
                (1.2, 0.5),
                (1.2, 0.6),
                (1.2, 0.75)
            ]
        );
        assert!(g.iter().any(|p| *p == base));
        // Duplicate key and the 256 limit.
        let k = parse_sweep("search.k1=1,2").unwrap();
        assert!(grid(&base, &[k.clone(), k]).is_err());
        let many = |key: &str, n: usize| SweepParam {
            key: key.to_string(),
            values: (0..n).map(|i| i as f64).collect(),
        };
        assert!(grid(&base, &[many("search.k1", 16), many("search.b", 16)]).is_ok());
        let e = grid(&base, &[many("search.k1", 16), many("search.b", 17)]).unwrap_err();
        assert!(e.to_string().contains("256"));
        // describe covers every key.
        let d: BTreeMap<String, f64> = describe(&base);
        assert_eq!(d.len(), PARAM_KEYS.len());
        let mut p = base.clone();
        apply(&mut p, "search.weight.contract", 5.0).unwrap();
        assert_eq!(p.search.weights[3], 5.0);
    }
}
