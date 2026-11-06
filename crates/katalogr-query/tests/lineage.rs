mod support;

mod lineage {
    use super::support::*;
    use katalogr_model::{Confidence, Fact, Id, Name};
    use katalogr_query::result::cap;
    use katalogr_query::{LineageAnswer, LineageOptions, LineageReport, SymbolHit, lineage};
    use katalogr_store::{Cache, FanKey, WriteTx};
    use std::collections::{BTreeMap, BTreeSet};

    const R: &str = "r";

    fn build(f: impl FnOnce(&mut WriteTx)) -> (tempfile::TempDir, Cache) {
        let tmp = tempfile::tempdir().unwrap();
        let cache = open(&tmp, &[R]);
        {
            let mut tx = cache.write().unwrap();
            f(&mut tx);
            tx.commit().unwrap();
        }
        (tmp, cache)
    }

    fn opts() -> LineageOptions {
        LineageOptions {
            depth: 3,
            limit: 200,
            per_group: 10,
            code_kinds: BTreeMap::new(),
            fold_from: Some(50),
        }
    }

    fn code_opts() -> LineageOptions {
        LineageOptions {
            code_kinds: BTreeMap::from([("rs".to_string(), BTreeSet::from(["fn".to_string()]))]),
            ..opts()
        }
    }

    /// The hit of the symbol with this canonical name, or of the contract node.
    fn subject(cache: &Cache, name: &str) -> SymbolHit {
        let tx = cache.read().unwrap();
        use katalogr_store::Reader;
        if name.starts_with("contract:") {
            return SymbolHit {
                name: name.to_string(),
                kind: "contract".into(),
                path: String::new(),
                span: span(1, 1),
            };
        }
        let row = tx.symbol(Id::of(name)).unwrap().expect(name);
        SymbolHit {
            name: row.name.canonical(),
            kind: row.kind.clone(),
            path: row.name.artifact.path.as_str().to_string(),
            span: row.span,
        }
    }

    fn run_with(cache: &Cache, name: &str, o: &LineageOptions) -> LineageAnswer {
        let t = subject(cache, name);
        let tx = cache.read().unwrap();
        lineage(&tx, &t, o).unwrap()
    }

    fn run(cache: &Cache, name: &str) -> LineageReport {
        run_with(cache, name, &opts()).report
    }

    fn group_names(
        r: &LineageReport,
        code: bool,
    ) -> Vec<(String, String, Confidence, Vec<String>)> {
        let groups = if code { &r.code } else { &r.bindings };
        groups
            .iter()
            .map(|g| {
                (
                    g.kind.clone(),
                    g.node_kind.clone(),
                    g.confidence,
                    g.hits.iter().map(|h| h.node.name.clone()).collect(),
                )
            })
            .collect()
    }

    const ORDER: &str = "r:s.tx#type:Order";
    const COPY: &str = "r:c.tx#type:Order";
    const DOC: &str = "r:d.tx#doc:orders";
    const KEY: &str = "r:d.tx#doc:orders/key:type";
    const HANDLER: &str = "r:code.rs#fn:handler";

    /// The order type with fields, a definition document that uses it and provides a channel,
    /// and a function that consumes the channel.
    fn base(tx: &mut WriteTx) {
        let s = artifact_with(
            tx,
            R,
            "s.tx",
            vec![
                sym(&[("type", "Order")], "type"),
                field(&[("type", "Order"), ("field", "id")], "string", true, false),
                field(
                    &[("type", "Order"), ("field", "lines")],
                    "object",
                    false,
                    true,
                ),
                field(
                    &[("type", "Order"), ("field", "lines"), ("field", "sku")],
                    "string",
                    true,
                    false,
                ),
            ],
        );
        link(tx, R, "s.tx", s[0], "contains", s[1]);
        link(tx, R, "s.tx", s[0], "contains", s[2]);
        link(tx, R, "s.tx", s[2], "contains", s[3]);
        let shape = contract(tx, "shape", "Order", 0, 0, 1);
        link(tx, R, "s.tx", s[0], "declares", shape);
        let d = artifact_with(
            tx,
            R,
            "d.tx",
            vec![
                sym(&[("doc", "orders")], "doc"),
                sym(&[("doc", "orders"), ("key", "type")], "key"),
            ],
        );
        link(tx, R, "d.tx", d[0], "contains", d[1]);
        link(tx, R, "d.tx", d[1], "uses-x", s[0]);
        let chan = contract(tx, "channel", "orders", 1, 1, 0);
        link(tx, R, "d.tx", d[0], "provides", chan);
        let c = artifact_ext(
            tx,
            R,
            "code.rs",
            "rs",
            vec![sym(&[("fn", "handler")], "fn")],
        );
        link(tx, R, "code.rs", c[0], "consumes", chan);
    }

    #[test]
    fn declarers_listed() {
        let (_t, c) = build(base);
        let r = run(&c, ORDER);
        assert_eq!(r.subject.name, ORDER);
        assert_eq!(r.subject_contracts, ["contract:shape:Order"]);
        assert_eq!(
            r.declared_in
                .iter()
                .map(|h| h.name.as_str())
                .collect::<Vec<_>>(),
            [ORDER]
        );
    }

    /// `base` plus a second declarer of the same contract.
    fn with_copy(tx: &mut WriteTx) {
        base(tx);
        let k = artifact_with(
            tx,
            R,
            "c.tx",
            vec![
                sym(&[("type", "Order")], "type"),
                field(&[("type", "Order"), ("field", "id")], "string", true, false),
            ],
        );
        link(tx, R, "c.tx", k[0], "contains", k[1]);
        let shape = contract(tx, "shape", "Order", 0, 0, 2);
        link(tx, R, "c.tx", k[0], "declares", shape);
    }

    #[test]
    fn copies_listed() {
        let (_t, c) = build(with_copy);
        for from in [ORDER, COPY] {
            let r = run(&c, from);
            assert_eq!(
                r.declared_in
                    .iter()
                    .map(|h| h.name.as_str())
                    .collect::<Vec<_>>(),
                [COPY, ORDER],
                "{from}"
            );
        }
    }

    #[test]
    fn contract_name_is_a_target() {
        let (_t, c) = build(with_copy);
        let r = run(&c, "contract:shape:Order");
        assert_eq!(r.subject.kind, "contract");
        assert_eq!(r.subject_contracts, ["contract:shape:Order"]);
        assert_eq!(r.declared_in.len(), 2);
        // The key that uses the type is reached from the declarer.
        assert!(group_names(&r, false).iter().any(|g| g.3 == [KEY]));
        // A contract with ends but no declarer still lists its ends, one hop out.
        let r = run(&c, "contract:channel:orders");
        let all: Vec<_> = group_names(&r, false)
            .into_iter()
            .flat_map(|g| g.3)
            .collect();
        assert!(all.contains(&HANDLER.to_string()), "{all:?}");
        assert!(all.contains(&DOC.to_string()), "{all:?}");
    }

    #[test]
    fn fields_nested_with_facts() {
        let (_t, c) = build(base);
        let r = run(&c, ORDER);
        type Row = (String, u32, Option<String>, Option<bool>, Option<bool>);
        let got: Vec<Row> = r
            .fields
            .iter()
            .map(|f| {
                (
                    f.node.name.rsplit('#').next().unwrap().to_string(),
                    f.depth,
                    f.field_type.clone(),
                    f.required,
                    f.many,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (
                    "type:Order/field:id".to_string(),
                    1,
                    Some("string".into()),
                    Some(true),
                    Some(false)
                ),
                (
                    "type:Order/field:lines".to_string(),
                    1,
                    Some("object".into()),
                    Some(false),
                    Some(true)
                ),
                (
                    "type:Order/field:lines/field:sku".to_string(),
                    2,
                    Some("string".into()),
                    Some(true),
                    Some(false)
                ),
            ]
        );
    }

    #[test]
    fn field_mapping_listed() {
        let (_t, c) = build(|tx| {
            base(tx);
            let m = artifact_with(
                tx,
                R,
                "m.tx",
                vec![Sym {
                    segs: vec![("key", "wire")],
                    kind: "key",
                    facts: vec![
                        ("wire_format", Fact::Str("wf".into())),
                        ("other", Fact::Str("no".into())),
                    ],
                }],
            );
            let id = Id::of("r:s.tx#type:Order/field:id");
            link_conf(tx, R, "m.tx", m[0], "maps-x", id, Confidence::Declared);
        });
        let r = run(&c, ORDER);
        let f = r
            .fields
            .iter()
            .find(|f| f.node.name.ends_with("field:id"))
            .unwrap();
        assert_eq!(f.mapped_total, 1);
        let m = &f.mapped_by[0];
        assert_eq!(
            (m.kind.as_str(), m.confidence),
            ("maps-x", Confidence::Declared)
        );
        assert_eq!(m.node.name, "r:m.tx#key:wire");
        assert_eq!(
            m.facts,
            BTreeMap::from([("wire_format".to_string(), "wf".to_string())])
        );
    }

    #[test]
    fn field_subject_names_owner() {
        let (_t, c) = build(base);
        let r = run(&c, "r:s.tx#type:Order/field:id");
        assert_eq!(r.owner.as_ref().map(|o| o.name.as_str()), Some(ORDER));
        // The owner's contract and bindings count.
        assert_eq!(r.subject_contracts, ["contract:shape:Order"]);
        assert!(r.boundaries.contains(&"owner-level".to_string()));
        assert!(group_names(&r, false).iter().any(|g| g.3 == [KEY]));
    }

    fn with_line(tx: &mut WriteTx) {
        base(tx);
        let l = artifact_with(tx, R, "l.tx", vec![sym(&[("type", "Line")], "type")]);
        // Order.lines uses Line.
        link(
            tx,
            R,
            "s.tx",
            Id::of("r:s.tx#type:Order/field:lines"),
            "uses-type",
            l[0],
        );
    }

    #[test]
    fn types_used_and_used_by() {
        let (_t, c) = build(with_line);
        let r = run(&c, ORDER);
        let uses: Vec<_> = r
            .related
            .iter()
            .map(|x| {
                (
                    x.relation.as_str(),
                    x.node.name.as_str(),
                    x.through_field.as_deref(),
                )
            })
            .collect();
        assert_eq!(uses, [("uses", "r:l.tx#type:Line", Some("lines"))]);
        let r = run(&c, "r:l.tx#type:Line");
        let used_by: Vec<_> = r
            .related
            .iter()
            .map(|x| {
                (
                    x.relation.as_str(),
                    x.node.name.as_str(),
                    x.through_field.as_deref(),
                )
            })
            .collect();
        assert_eq!(used_by, [("used-by", ORDER, Some("lines"))]);
        // A field that uses the type is related, not a binding.
        assert!(group_names(&r, false).is_empty());
    }

    #[test]
    fn extends_both_ways() {
        let (_t, c) = build(|tx| {
            base(tx);
            let b = artifact_with(tx, R, "b.tx", vec![sym(&[("type", "Base")], "type")]);
            link(tx, R, "s.tx", Id::of(ORDER), "extends", b[0]);
        });
        let r = run(&c, ORDER);
        assert_eq!(
            r.related
                .iter()
                .map(|x| (x.relation.as_str(), x.node.name.as_str()))
                .collect::<Vec<_>>(),
            [("extends", "r:b.tx#type:Base")]
        );
        let r = run(&c, "r:b.tx#type:Base");
        assert_eq!(
            r.related
                .iter()
                .map(|x| (x.relation.as_str(), x.node.name.as_str()))
                .collect::<Vec<_>>(),
            [("extended-by", ORDER)]
        );
    }

    #[test]
    fn bindings_grouped_by_kind_node_and_confidence() {
        let (_t, c) = build(|tx| {
            base(tx);
            let e = artifact_with(
                tx,
                R,
                "e.tx",
                vec![
                    sym(&[("key", "a")], "key"),
                    sym(&[("key", "b")], "key"),
                    sym(&[("item", "c")], "item"),
                ],
            );
            link_conf(
                tx,
                R,
                "e.tx",
                e[0],
                "uses-x",
                Id::of(ORDER),
                Confidence::Extracted,
            );
            link_conf(
                tx,
                R,
                "e.tx",
                e[1],
                "uses-x",
                Id::of(ORDER),
                Confidence::Inferred,
            );
            link_conf(
                tx,
                R,
                "e.tx",
                e[2],
                "uses-x",
                Id::of(ORDER),
                Confidence::Extracted,
            );
        });
        let r = run(&c, ORDER);
        let g: Vec<_> = group_names(&r, false)
            .into_iter()
            .filter(|g| g.0 == "uses-x")
            .map(|g| (g.1, g.2, g.3.len()))
            .collect();
        // The key of the definition document uses the type too, so `key` holds two.
        assert_eq!(
            g,
            [
                ("item".to_string(), Confidence::Extracted, 1),
                ("key".to_string(), Confidence::Extracted, 2),
                ("key".to_string(), Confidence::Inferred, 1),
            ]
        );
    }

    #[test]
    fn any_link_kind_followed() {
        let (_t, c) = build(|tx| {
            base(tx);
            let e = artifact_with(tx, R, "e.tx", vec![sym(&[("thing", "t")], "thing")]);
            link(tx, R, "e.tx", e[0], "kind_a", Id::of(ORDER));
            // Kinds that are not dependencies stay out.
            link(tx, R, "e.tx", e[0], "touches", Id::of(ORDER));
        });
        let r = run(&c, ORDER);
        let kinds: Vec<String> = r.bindings.iter().map(|g| g.kind.clone()).collect();
        assert!(kinds.contains(&"kind_a".to_string()), "{kinds:?}");
        assert!(!kinds.contains(&"touches".to_string()), "{kinds:?}");
        assert!(!kinds.contains(&"contains".to_string()));
    }

    #[test]
    fn holder_found_through_contains() {
        let (_t, c) = build(base);
        let r = run(&c, ORDER);
        let hit = r
            .bindings
            .iter()
            .flat_map(|g| &g.hits)
            .find(|h| h.node.name == KEY)
            .expect("the key");
        assert_eq!(hit.holder.as_deref(), Some(DOC));
        assert_eq!(hit.depth, 0);
    }

    #[test]
    fn through_contract_reaches_other_side() {
        let (_t, c) = build(base);
        // Without code kinds the consumer is an ordinary binding, one hop out, through the
        // channel the holder document provides.
        let r = run(&c, ORDER);
        let h = r
            .bindings
            .iter()
            .find(|g| g.kind == "consumes")
            .and_then(|g| g.hits.first())
            .expect("consumer");
        assert_eq!(h.node.name, HANDLER);
        assert_eq!(h.depth, 1);
        assert_eq!(h.via.as_deref(), Some("contract:channel:orders"));
        assert!(
            r.contracts
                .iter()
                .any(|c| c.contract == "contract:channel:orders")
        );
    }

    #[test]
    fn code_listed_apart() {
        let (_t, c) = build(base);
        let r = run_with(&c, ORDER, &code_opts()).report;
        assert!(
            r.bindings
                .iter()
                .all(|g| g.hits.iter().all(|h| h.node.name != HANDLER))
        );
        let code = group_names(&r, true);
        assert_eq!(code.len(), 1);
        assert_eq!(
            (code[0].0.as_str(), code[0].3.clone()),
            ("consumes", vec![HANDLER.to_string()])
        );
        assert!(!r.boundaries.contains(&"no-code".to_string()));
    }

    #[test]
    fn code_ends_a_path() {
        let build_graph = |tx: &mut WriteTx| {
            base(tx);
            // The handler also consumes a second channel that another document provides.
            let o = artifact_with(tx, R, "o.tx", vec![sym(&[("doc", "other")], "doc")]);
            let other = contract(tx, "channel", "other", 1, 1, 0);
            link(tx, R, "o.tx", o[0], "provides", other);
            link(tx, R, "code.rs", Id::of(HANDLER), "consumes", other);
        };
        let (_t, c) = build(build_graph);
        let names = |r: &LineageReport| -> Vec<String> {
            r.bindings
                .iter()
                .chain(&r.code)
                .flat_map(|g| g.hits.iter().map(|h| h.node.name.clone()))
                .collect()
        };
        let plain = run(&c, ORDER);
        assert!(names(&plain).contains(&"r:o.tx#doc:other".to_string()));
        let coded = run_with(&c, ORDER, &code_opts()).report;
        assert!(names(&coded).contains(&HANDLER.to_string()));
        assert!(!names(&coded).contains(&"r:o.tx#doc:other".to_string()));
    }

    #[test]
    fn tests_marked() {
        let (_t, c) = build(|tx| {
            base(tx);
            mark_test(tx, Id::of(HANDLER), "test");
        });
        let r = run_with(&c, ORDER, &code_opts()).report;
        let h = &r.code[0].hits[0];
        assert_eq!(h.role.as_deref(), Some("test"));
    }

    #[test]
    fn depth_cap_partial() {
        // order -> doc channel -> one consumer hop -> its holder's channel -> a second hop.
        let build_graph = |tx: &mut WriteTx| {
            base(tx);
            let o = artifact_with(tx, R, "o.tx", vec![sym(&[("doc", "far")], "doc")]);
            let far = contract(tx, "channel", "far", 1, 1, 0);
            link(tx, R, "o.tx", o[0], "provides", far);
            link(tx, R, "code.rs", Id::of(HANDLER), "consumes", far);
        };
        let (_t, c) = build(build_graph);
        // The handler is a plain symbol here: it consumes the next channel too.
        let one = run_with(&c, ORDER, &LineageOptions { depth: 1, ..opts() }).report;
        let caps: Vec<_> = one.partial.iter().map(|p| p.cap.as_str()).collect();
        assert!(caps.contains(&cap::DEPTH), "{caps:?}");
        let three = run(&c, ORDER);
        assert!(
            three.partial.iter().all(|p| p.cap != cap::DEPTH),
            "{:?}",
            three.partial
        );
        let all: Vec<_> = three
            .bindings
            .iter()
            .flat_map(|g| g.hits.iter().map(|h| h.node.name.clone()))
            .collect();
        assert!(all.contains(&"r:o.tx#doc:far".to_string()), "{all:?}");
    }

    /// `n` keys using the type, `kind` named by the argument.
    fn many_keys(tx: &mut WriteTx, n: usize, kind: &str) {
        base(tx);
        let names: Vec<&'static str> = (0..n)
            .map(|i| &*Box::leak(format!("k{i:03}").into_boxed_str()))
            .collect();
        let syms: Vec<Sym> = names.iter().map(|n| sym(&[("key", *n)], "key")).collect();
        let ids = artifact_with(tx, R, "e.tx", syms);
        for id in ids {
            link(tx, R, "e.tx", id, kind, Id::of(ORDER));
        }
    }

    #[test]
    fn limit_partial() {
        let (_t, c) = build(|tx| many_keys(tx, 10, "uses-y"));
        let r = run_with(
            &c,
            ORDER,
            &LineageOptions {
                limit: 4,
                per_group: 4,
                ..opts()
            },
        )
        .report;
        let hits: usize = r.bindings.iter().map(|g| g.hits.len()).sum();
        assert_eq!(hits, 4);
        let p = r
            .partial
            .iter()
            .find(|p| p.cap == cap::LIMIT)
            .expect("limit partial");
        assert_eq!((p.at, p.shown), (4, 4));
        assert!(p.total.unwrap() >= 10);
        assert_eq!(r.total, p.total);
    }

    #[test]
    fn per_group_partial() {
        let (_t, c) = build(|tx| many_keys(tx, 5, "uses-y"));
        let r = run_with(
            &c,
            ORDER,
            &LineageOptions {
                per_group: 2,
                ..opts()
            },
        )
        .report;
        let g = r.bindings.iter().find(|g| g.kind == "uses-y").unwrap();
        assert_eq!((g.total, g.hits.len()), (5, 2));
        let p = r
            .partial
            .iter()
            .find(|p| p.cap == cap::PER_GROUP)
            .expect("per-group partial");
        assert_eq!((p.at, p.shown, p.total), (2, 2, Some(5)));
    }

    /// 60 targets share a key; `r0..r2` call `t0` through it by an inferred fan.
    fn fan_graph(tx: &mut WriteTx) {
        use katalogr_model::{ExportKey, LinkKind};
        let names: Vec<&'static str> = (0..60)
            .map(|i| &*Box::leak(format!("t{i}").into_boxed_str()))
            .collect();
        let mut syms: Vec<Sym> = names.iter().map(|n| sym(&[("type", *n)], "type")).collect();
        for r in ["r0", "r1", "r2"] {
            syms.push(sym(&[("key", r)], "key"));
        }
        let ids = artifact_with(tx, R, "f.tx", syms);
        let mut targets: Vec<Id> = ids[..60].to_vec();
        targets.sort();
        let key = ExportKey {
            namespace: "m".into(),
            key: "k".into(),
        };
        tx.put_exports(&key, &targets).unwrap();
        for from in &ids[60..] {
            let mut row = link_row(R, "f.tx");
            row.entries[0].confidence = Confidence::Inferred;
            tx.put_fan(
                &FanKey {
                    from: *from,
                    kind: LinkKind("uses-y".into()),
                    key: key.clone(),
                },
                &row,
            )
            .unwrap();
        }
    }

    #[test]
    fn fan_folds() {
        let (_t, c) = build(fan_graph);
        let a = run_with(&c, "r:f.tx#type:t0", &opts());
        assert_eq!(a.folded.len(), 1, "{:?}", a.folded);
        assert_eq!(
            (
                a.folded[0].key.as_str(),
                a.folded[0].hits,
                a.folded[0].kind.as_str()
            ),
            ("m:k", 3, "uses-y")
        );
        assert!(a.report.bindings.iter().all(|g| g.kind != "uses-y"));
    }

    #[test]
    fn expand_shows_all() {
        let (_t, c) = build(fan_graph);
        let a = run_with(
            &c,
            "r:f.tx#type:t0",
            &LineageOptions {
                fold_from: None,
                per_group: 200,
                ..opts()
            },
        );
        assert!(a.folded.is_empty());
        let g = a
            .report
            .bindings
            .iter()
            .find(|g| g.kind == "uses-y")
            .expect("group");
        assert_eq!((g.total, g.hits.len()), (3, 3));
        assert!(a.report.partial.is_empty(), "{:?}", a.report.partial);
    }

    #[test]
    fn output_is_deterministic() {
        // The same graph written in two orders gives the same answer, byte for byte.
        let one = build(|tx| {
            with_copy(tx);
            many_keys_extra(tx, false);
        });
        let two = build(|tx| {
            many_keys_extra(tx, true);
            with_copy(tx);
        });
        let a = run_with(&one.1, ORDER, &code_opts());
        let b = run_with(&two.1, ORDER, &code_opts());
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
        assert_eq!(
            format!("{a:?}"),
            format!("{:?}", run_with(&one.1, ORDER, &code_opts()))
        );
    }

    fn many_keys_extra(tx: &mut WriteTx, first: bool) {
        let _ = first;
        let ids = artifact_with(
            tx,
            R,
            "x.tx",
            vec![sym(&[("key", "x1")], "key"), sym(&[("key", "x2")], "key")],
        );
        link(tx, R, "x.tx", ids[0], "uses-y", Id::of(ORDER));
        link_conf(
            tx,
            R,
            "x.tx",
            ids[1],
            "uses-y",
            Id::of(ORDER),
            Confidence::Inferred,
        );
    }

    #[test]
    fn boundary_codes() {
        // A symbol that binds nothing: no contract, no fields, no code.
        let (_t, c) = build(|tx| {
            artifact_with(tx, R, "z.tx", vec![sym(&[("thing", "z")], "thing")]);
        });
        let r = run(&c, "r:z.tx#thing:z");
        assert_eq!(r.boundaries, ["no-code", "no-contract", "no-fields"]);
        // The base type has fields and a contract nobody consumes: open, and no code yet.
        let (_t2, c2) = build(base);
        let r = run(&c2, ORDER);
        assert_eq!(r.boundaries, ["no-code", "open-contract"]);
        let mut sorted = r.boundaries.clone();
        sorted.sort();
        assert_eq!(sorted, r.boundaries);
    }

    #[test]
    fn open_contract_listed() {
        let (_t, c) = build(base);
        let r = run(&c, ORDER);
        let shape = r
            .contracts
            .iter()
            .find(|c| c.contract == "contract:shape:Order")
            .unwrap();
        assert_eq!(shape.open.as_deref(), Some("no-consumer"));
        let chan = r
            .contracts
            .iter()
            .find(|c| c.contract == "contract:channel:orders")
            .unwrap();
        assert_eq!(chan.open, None);
        assert!(
            r.contracts
                .windows(2)
                .all(|w| w[0].contract < w[1].contract)
        );
    }
}
