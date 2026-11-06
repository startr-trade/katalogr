//! Stored answers (FEAT-096): the reports a stored answer holds, and how one is asked for and
//! checked. Needs the `.tx` test extractor: `cargo test -p katalogr-dist --features
//! test-extractor` (a workspace test run enables it through the CLI crate's dev-dependency).
#![cfg(feature = "test-extractor")]

mod answers {
    use katalogr_analytics::{
        Community, CommunityReport, ContractKindCount, CoverageCounts, CoverageReport, CoverageRow,
        Cycle, CycleReport, DeadCandidate, DeadClass, DeadCodeReport, DeadConfidence, HubEntry,
        HubReport, KindCounts, Level, NodeHit, SuffixCount, UnclaimedRow,
    };
    use katalogr_model::{Confidence, Span};
    use katalogr_query::result::{Direction, MapEntry, PathStep, SymbolHit};
    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use std::collections::BTreeMap;

    /// Serialise, parse, serialise again: the bytes must not move.
    fn roundtrip<T: Serialize + DeserializeOwned>(what: &str, v: &T) {
        let first = serde_json::to_vec(v).unwrap();
        let back: T = serde_json::from_slice(&first).unwrap_or_else(|e| panic!("{what}: {e}"));
        let second = serde_json::to_vec(&back).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&first),
            String::from_utf8_lossy(&second),
            "{what}"
        );
    }

    /// Floats that expose a lossy parser: short, tiny, and a full 17-digit value.
    const FLOATS: [f64; 4] = [0.1, 1e-7, 0.123_456_789_012_345_68, 2.0 / 3.0];

    fn sym(n: usize) -> SymbolHit {
        SymbolHit {
            name: format!("pkg::item{n}"),
            kind: "function".into(),
            path: format!("src/f{n}.tx"),
            span: Span {
                start_line: 1,
                start_col: 1,
                end_line: 3,
                end_col: 2,
            },
        }
    }

    fn node(level: Level, n: usize, with_path: bool) -> NodeHit {
        NodeHit {
            level,
            name: format!("n{n}"),
            kind: "function".into(),
            path: with_path.then(|| format!("src/n{n}.tx")),
            line: with_path.then_some(n as u32 + 1),
        }
    }

    #[test]
    fn report_roundtrip_byte_identical() {
        for (i, f) in FLOATS.iter().enumerate() {
            let present = i % 2 == 0;
            let hubs = HubReport {
                level: Level::File,
                nodes: 9,
                mean_degree: *f,
                items: vec![HubEntry {
                    node: node(Level::File, i, present),
                    in_degree: *f,
                    out_degree: f * 3.0,
                    degree: f * 4.0,
                    z: -f,
                    rank: f / 7.0,
                    members: 2,
                }],
            };
            roundtrip("hubs", &hubs);

            let communities = CommunityReport {
                level: Level::Symbol,
                seed: 7,
                resolution: *f,
                modularity: f / 3.0,
                communities_total: 1,
                unclustered: 0,
                note: present.then(|| "a note".to_string()),
                items: vec![Community {
                    id: 0,
                    size: 2,
                    label: node(Level::Symbol, i, present),
                    dir: present.then(|| "src".to_string()),
                    cohesion: *f,
                    internal_weight: f * 2.0,
                    boundary_weight: f / 5.0,
                    top: vec![node(Level::Symbol, 1, present)],
                    bridges: Vec::new(),
                    files: 1,
                }],
            };
            roundtrip("communities", &communities);

            let cycles = CycleReport {
                level: Level::Package,
                include_inferred: present,
                cycles_total: 1,
                cyclic_nodes: 2,
                skipped_contracts: 0,
                items: vec![Cycle {
                    id: 0,
                    size: 2,
                    members: vec![node(Level::Package, 1, false)],
                    members_total: 2,
                    witness: vec![PathStep {
                        from: "a".into(),
                        kind: "calls".into(),
                        direction: Direction::Forward,
                        to: "b".into(),
                        confidence: Confidence::Inferred,
                    }],
                    confidence: Confidence::Extracted,
                }],
            };
            roundtrip("cycles", &cycles);

            let mut roots = BTreeMap::new();
            roots.insert("main".to_string(), 1);
            let dead = DeadCodeReport {
                roots: 1,
                roots_by_reason: roots,
                candidates_total: 1,
                high: 1,
                medium: 0,
                low: 0,
                note: present.then(|| "note".to_string()),
                items: vec![DeadCandidate {
                    symbol: sym(i),
                    class: DeadClass::WeaklyReached,
                    confidence: DeadConfidence::Medium,
                    incoming: 1,
                    weak_incoming: 1,
                }],
            };
            roundtrip("dead-code", &dead);

            let map = vec![MapEntry {
                symbol: sym(i),
                in_degree: *f,
                rank: f / 11.0,
            }];
            roundtrip("map", &map);

            let counts = CoverageCounts {
                files_indexed: 3,
                refs_total: 4,
                parse_error_rate: present.then_some(*f),
                resolution_rate: Some(f / 2.0),
                inferred_rate: None,
                unresolved_rate: present.then_some(1.0 - f),
                ..CoverageCounts::default()
            };
            let row = CoverageRow {
                repo: present.then(|| "r".to_string()),
                extension: None,
                language: Some("l".into()),
                counts: counts.clone(),
            };
            let coverage = CoverageReport {
                totals: counts,
                by_repo: vec![row.clone()],
                by_extension: Vec::new(),
                by_language: Vec::new(),
                groups: vec![row],
                by_link_kind: vec![KindCounts {
                    kind: "calls".into(),
                    total: 2,
                    resolved: 1,
                    inferred: 1,
                    unresolved: 0,
                }],
                dangling_contracts_by_kind: vec![ContractKindCount {
                    kind: "k".into(),
                    dangling: 1,
                }],
                unclaimed: vec![UnclaimedRow {
                    repo: "r".into(),
                    files: 1,
                    suffixes: vec![SuffixCount {
                        suffix: ".x".into(),
                        files: 1,
                    }],
                }],
                roots_missing: Vec::new(),
                metrics: Vec::new(),
            };
            roundtrip("coverage", &coverage);
        }
    }

    use katalogr_analytics::{
        CommunityOptions, CycleOptions, DeadCodeOptions, GraphOptions, HubOptions,
    };
    use katalogr_dist::{CoverageOptions, StoredQuery, validity};

    fn graph(level: Level, inferred: bool) -> GraphOptions {
        let mut g = GraphOptions::new(level, inferred);
        // Not in sorted order: the key sorts it.
        g.exclude_kinds = vec!["zeta".into(), "alpha".into()];
        g
    }

    fn samples() -> Vec<StoredQuery> {
        vec![
            StoredQuery::Hubs(HubOptions {
                graph: graph(Level::File, true),
                min_degree: 15.5,
                limit: 20,
            }),
            StoredQuery::Communities(CommunityOptions {
                graph: graph(Level::Symbol, false),
                seed: 42,
                resolution: 0.1,
                min_size: 3,
                members: 5,
                limit: 10,
                of: Some("pkg::item".into()),
            }),
            StoredQuery::Communities(CommunityOptions {
                graph: graph(Level::Package, false),
                seed: 1,
                resolution: 1.0,
                min_size: 3,
                members: 5,
                limit: 10,
                of: None,
            }),
            StoredQuery::Cycles(CycleOptions {
                graph: graph(Level::Package, true),
                members: 4,
                limit: 7,
            }),
            StoredQuery::DeadCode(DeadCodeOptions {
                code_kinds: BTreeMap::new(),
                exclude_kinds: vec!["b".into(), "a".into()],
                internal: true,
                min_confidence: DeadConfidence::Medium,
                path_prefix: Some("src/".into()),
                limit: 50,
            }),
            StoredQuery::DeadCode(DeadCodeOptions {
                code_kinds: BTreeMap::new(),
                exclude_kinds: Vec::new(),
                internal: false,
                min_confidence: DeadConfidence::High,
                path_prefix: None,
                limit: 5,
            }),
            StoredQuery::Map,
            StoredQuery::Coverage(CoverageOptions {
                repo: Some("app".into()),
                suffix_top: 20,
            }),
            StoredQuery::Coverage(CoverageOptions::default()),
        ]
    }

    #[test]
    fn view_query_roundtrip() {
        for q in samples() {
            let vq = q.to_view_query();
            let back = StoredQuery::from_view_query(&vq)
                .unwrap_or_else(|| panic!("{} did not come back", vq.op));
            assert_eq!(back.to_view_query(), vq, "{}", vq.op);
        }
        // The table's arguments, spelled out for one op.
        let hubs = samples()[0].to_view_query();
        assert_eq!(hubs.op, "hubs");
        assert_eq!(hubs.args["level"], "file");
        assert_eq!(hubs.args["exclude_kinds"], "[\"alpha\",\"zeta\"]");
        assert_eq!(hubs.args["min_degree"], "15.5");
        assert_eq!(hubs.args.len(), 5);
        assert!(samples()[8].to_view_query().args.get("repo").is_none());
        assert!(samples()[6].to_view_query().args.is_empty());
        // Unknown ops and bad values are refused.
        let bad = |op: &str, k: &str, v: &str| {
            let mut vq = samples()[0].to_view_query();
            vq.op = op.into();
            vq.args.insert(k.into(), v.into());
            StoredQuery::from_view_query(&vq).is_none()
        };
        assert!(bad("nope", "limit", "1"));
        assert!(bad("hubs", "limit", "many"));
        assert!(bad("hubs", "level", "galaxy"));
        let mut missing = samples()[0].to_view_query();
        missing.args.remove("limit");
        assert!(StoredQuery::from_view_query(&missing).is_none());
        // A budget or output flag is not part of a question.
        for q in samples() {
            assert!(!q.to_view_query().args.contains_key("budget"));
        }
    }

    #[test]
    fn validity_changes_with_each_input() {
        let mut inputs = BTreeMap::new();
        inputs.insert("index".to_string(), "i1".to_string());
        inputs.insert("listing".to_string(), "l1".to_string());
        let base = validity("b1", &inputs);
        assert_eq!(base.len(), 64);
        assert_eq!(base, validity("b1", &inputs), "stable");
        // Pinned to the documented form.
        let want = {
            use sha2::{Digest, Sha256};
            let text = r#"{"answer_format":1,"build":"b1","inputs":{"index":"i1","listing":"l1"}}"#;
            Sha256::digest(text.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        assert_eq!(base, want);
        assert_ne!(base, validity("b2", &inputs), "build");
        let mut other = inputs.clone();
        other.insert("index".into(), "i2".into());
        assert_ne!(base, validity("b1", &other), "index value");
        let mut other = inputs.clone();
        other.insert("listing".into(), "l2".into());
        assert_ne!(base, validity("b1", &other), "listing value");
        let mut other = inputs.clone();
        other.remove("listing");
        assert_ne!(base, validity("b1", &other), "input removed");
        let mut other = inputs.clone();
        other.insert("extra".into(), "x".into());
        assert_ne!(base, validity("b1", &other), "input added");
        let mut renamed = BTreeMap::new();
        renamed.insert("index2".to_string(), "i1".to_string());
        renamed.insert("listing".to_string(), "l1".to_string());
        assert_ne!(base, validity("b1", &renamed), "input renamed");
    }

    // ------------------------------------------------------------------ the facade

    use katalogr_dist::{
        AnswerRequest, Consistency, DistError, HistoryScope, Katalogr, Options, Outcome, Overrides,
        kindex_init, project_init,
    };
    use katalogr_store::{
        AnswerListing, AnswerStore, DirAnswerStore, PruneReport, PutOutcome, StoreError,
        StoredAnswer,
    };
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    fn git(root: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("run git");
        assert!(st.success(), "git {args:?} failed");
    }

    /// A graph with a hub, a cycle and an unreferenced symbol.
    const GRAPH: &str = "sym fn hub\nsym fn a\nsym fn b\nsym fn c\nsym fn lonely\n\
        link a calls hub\nlink b calls hub\nlink c calls hub\nlink a calls b\nlink b calls a\n";

    struct Fx {
        _tmp: tempfile::TempDir,
        kx: PathBuf,
        repo: PathBuf,
        k: Katalogr,
    }

    fn overrides(kx: &Path, context: Option<&str>) -> Overrides {
        Overrides {
            kindex: Some(kx.display().to_string()),
            project: None,
            context: context.map(str::to_string),
            ..Default::default()
        }
    }

    /// A committed git repo of `.tx` files and a project over it, indexed, with build `b1`.
    fn fx() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let kx = tmp.path().join("kx");
        let repo = tmp.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("g.tx"), GRAPH).unwrap();
        std::fs::write(repo.join("h.tx"), "sym fn other\nlink other calls hub\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "ignored.tx\n").unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "c1"]);
        kindex_init(&kx).unwrap();
        project_init(&kx, "demo", vec![("app".into(), repo.clone())], true).unwrap();
        let mut k = Katalogr::resolve(&repo, &overrides(&kx, None)).unwrap();
        k.index(None, true, None, &Options::default()).unwrap();
        k.set_answer_build(Some("b1".into()));
        Fx {
            _tmp: tmp,
            kx,
            repo,
            k,
        }
    }

    impl Fx {
        fn answers_dir(&self) -> PathBuf {
            self.kx.join("projects/demo/.cache/answers")
        }

        fn entries(&self) -> Vec<katalogr_store::AnswerListing> {
            DirAnswerStore::new(camino::Utf8PathBuf::from_path_buf(self.answers_dir()).unwrap())
                .list()
                .unwrap()
        }

        fn fresh_facade(&self, context: Option<&str>) -> Katalogr {
            let mut k = Katalogr::resolve(&self.repo, &overrides(&self.kx, context)).unwrap();
            k.set_answer_build(Some("b1".into()));
            k
        }

        fn commit(&self, msg: &str) {
            git(&self.repo, &["add", "-A"]);
            git(&self.repo, &["commit", "-q", "-m", msg]);
        }
    }

    fn hub_opts() -> HubOptions {
        HubOptions {
            graph: GraphOptions::new(Level::Symbol, true),
            min_degree: 0.0,
            limit: 20,
        }
    }

    fn community_opts() -> CommunityOptions {
        CommunityOptions {
            graph: GraphOptions::new(Level::Symbol, true),
            seed: 7,
            resolution: 1.0,
            min_size: 1,
            members: 5,
            limit: 10,
            of: None,
        }
    }

    fn cycle_opts() -> CycleOptions {
        CycleOptions {
            graph: GraphOptions::new(Level::Symbol, true),
            members: 5,
            limit: 10,
        }
    }

    fn dead_opts() -> DeadCodeOptions {
        DeadCodeOptions {
            code_kinds: BTreeMap::new(),
            exclude_kinds: GraphOptions::new(Level::Symbol, true).exclude_kinds,
            internal: false,
            min_confidence: DeadConfidence::Low,
            path_prefix: None,
            limit: 20,
        }
    }

    fn json<T: Serialize>(o: &Outcome<T>) -> String {
        match o {
            Outcome::Found(v) => serde_json::to_string(v).unwrap(),
            other => panic!("not found: {:?}", std::mem::discriminant(other)),
        }
    }

    /// The five first commands and `coverage`, as compact JSON of the report, in a fixed order.
    fn run_all(k: &Katalogr, o: &Options) -> Vec<String> {
        vec![
            json(&k.hubs(&hub_opts(), o).unwrap().outcome),
            json(&k.communities(&community_opts(), o).unwrap().outcome),
            json(&k.cycles(&cycle_opts(), o).unwrap().outcome),
            json(&k.dead_code(dead_opts(), o).unwrap().outcome),
            json(&k.map(o).unwrap().outcome),
        ]
    }

    fn opts() -> Options {
        Options::default()
    }

    /// A store in memory: the second implementation of the trait, as `answer_store` has.
    #[derive(Default)]
    struct MemoryStore(Mutex<BTreeMap<String, StoredAnswer>>);

    impl AnswerStore for MemoryStore {
        fn get(&self, v: &str) -> Result<Option<StoredAnswer>, StoreError> {
            Ok(self.0.lock().unwrap().get(v).cloned())
        }
        fn put(&self, a: &StoredAnswer) -> Result<PutOutcome, StoreError> {
            self.0
                .lock()
                .unwrap()
                .insert(a.header.view_id.clone(), a.clone());
            Ok(PutOutcome::Stored)
        }
        fn remove(&self, v: &str) -> Result<(), StoreError> {
            self.0.lock().unwrap().remove(v);
            Ok(())
        }
        fn list(&self) -> Result<Vec<AnswerListing>, StoreError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .values()
                .map(|a| AnswerListing {
                    header: a.header.clone(),
                    bytes: a.payload.len() as u64,
                })
                .collect())
        }
        fn prune(&self, _: u64) -> Result<PruneReport, StoreError> {
            Ok(PruneReport::default())
        }
    }

    #[test]
    fn second_call_is_a_hit() {
        let f = fx();
        let first = json(&f.k.hubs(&hub_opts(), &opts()).unwrap().outcome);
        let c = f.k.answer_counters();
        assert_eq!((c.hits, c.misses, c.writes), (0, 1, 1));
        let second = json(&f.k.hubs(&hub_opts(), &opts()).unwrap().outcome);
        assert_eq!(first, second, "a hit is the fresh answer");
        let c = f.k.answer_counters();
        assert_eq!((c.hits, c.misses, c.writes), (1, 1, 1));
    }

    #[test]
    fn each_first_command_hits() {
        let f = fx();
        let first = run_all(&f.k, &opts());
        let c = f.k.answer_counters();
        assert_eq!((c.hits, c.misses, c.writes), (0, 5, 5));
        // A new facade in a new process reads what the first wrote.
        let other = f.fresh_facade(None);
        let second = run_all(&other, &opts());
        assert_eq!(first, second);
        let c = other.answer_counters();
        assert_eq!((c.hits, c.misses, c.writes), (5, 0, 0));
    }

    #[test]
    fn hit_across_contexts() {
        let f = fx();
        let first = json(&f.k.hubs(&hub_opts(), &opts()).unwrap().outcome);
        let id = f.k.context_add_commit("app", "HEAD", &opts()).unwrap().id;
        let in_commit = f.fresh_facade(Some(&id));
        let second = json(&in_commit.hubs(&hub_opts(), &opts()).unwrap().outcome);
        assert_eq!(first, second);
        let c = in_commit.answer_counters();
        assert_eq!(
            (c.hits, c.misses),
            (1, 0),
            "the commit context hit the entry"
        );
    }

    #[test]
    fn miss_writes_entry() {
        let f = fx();
        assert!(f.entries().is_empty());
        let r = f.k.hubs(&hub_opts(), &opts()).unwrap();
        let entries = f.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].header.op, "hubs");
        assert_eq!(entries[0].header.args["limit"], "20");
        assert_eq!(
            Some(entries[0].header.view_id.clone()),
            r.report.view_id.or(Some(entries[0].header.view_id.clone()))
        );
    }

    #[test]
    fn no_match_not_stored() {
        let f = fx();
        let key = format!("v1-{}", "cd".repeat(16));
        let req = AnswerRequest {
            view_id: key,
            op: "custom",
            args: BTreeMap::new(),
            inputs: BTreeMap::new(),
            storable: true,
        };
        let mut warnings = Vec::new();
        let none: Outcome<Vec<String>> =
            f.k.through_store(
                &req,
                &mut warnings,
                || Ok(Outcome::NoMatch("x".into())),
                false,
            )
            .unwrap();
        assert!(matches!(none, Outcome::NoMatch(_)));
        assert!(f.entries().is_empty());
        assert_eq!(f.k.answer_counters().writes, 0);
        // A community of a node that is not there says so in its report, which is a found
        // report and is stored like any other.
        let mut c = community_opts();
        c.of = Some("nothing::here".into());
        f.k.communities(&c, &opts()).unwrap();
        assert_eq!(f.entries().len(), 1);
    }

    #[test]
    fn dirty_position_not_stored() {
        let f = fx();
        std::fs::write(f.repo.join("g.tx"), format!("{GRAPH}sym fn extra\n")).unwrap();
        let r = f.k.hubs(&hub_opts(), &opts()).unwrap();
        assert!(r.report.exact);
        assert!(r.report.positions[0].dirty.is_some());
        assert!(f.entries().is_empty());
        let c = f.k.answer_counters();
        assert_eq!((c.hits, c.misses, c.writes), (0, 0, 0));
    }

    #[test]
    fn not_exact_skips_store() {
        let f = fx();
        let o = Options {
            consistency: Some(Consistency::StaleOk),
            ..opts()
        };
        let r = f.k.hubs(&hub_opts(), &o).unwrap();
        assert!(!r.report.exact);
        assert!(f.entries().is_empty());
        assert_eq!(f.k.answer_counters(), Default::default());
    }

    #[test]
    fn new_commit_misses() {
        let f = fx();
        run_all(&f.k, &opts());
        std::fs::write(
            f.repo.join("h.tx"),
            "sym fn other\nsym fn more\nlink other calls hub\n",
        )
        .unwrap();
        f.commit("c2");
        let before = f.k.answer_counters();
        let r = f.k.hubs(&hub_opts(), &opts()).unwrap();
        assert!(r.report.exact);
        let c = f.k.answer_counters();
        assert_eq!((c.hits, c.misses), (before.hits, before.misses + 1));
        assert_eq!(f.entries().len(), 6, "the old entries stay");
        let again = f.k.hubs(&hub_opts(), &opts()).unwrap();
        assert_eq!(f.k.answer_counters().hits, before.hits + 1);
        assert_eq!(json(&r.outcome), json(&again.outcome));
    }

    #[test]
    fn profile_change_misses() {
        let f = fx();
        f.k.hubs(&hub_opts(), &opts()).unwrap();
        // A profile file changes how files are routed; every revision records it.
        let profiles = f.kx.join("projects/demo/profiles");
        std::fs::create_dir_all(&profiles).unwrap();
        std::fs::write(
            profiles.join("p.yaml"),
            "profile: p\nversion: 2\nselect:\n  - { extension: text, path: 'h.tx' }\n",
        )
        .unwrap();
        let before = f.k.answer_counters();
        f.k.hubs(&hub_opts(), &opts()).unwrap();
        let c = f.k.answer_counters();
        assert_eq!(c.hits, before.hits, "not served from the old entry");
        assert_eq!(c.misses, before.misses + 1);
    }

    #[test]
    fn setting_change_misses() {
        let f = fx();
        f.k.hubs(&hub_opts(), &opts()).unwrap();
        let mut other = hub_opts();
        other.min_degree = 1.5;
        f.k.hubs(&other, &opts()).unwrap();
        let c = f.k.answer_counters();
        assert_eq!((c.hits, c.misses, c.writes), (0, 2, 2));
        assert_eq!(f.entries().len(), 2);
        // The same options hit.
        f.k.hubs(&other, &opts()).unwrap();
        assert_eq!(f.k.answer_counters().hits, 1);
    }

    #[test]
    fn build_change_misses() {
        let mut f = fx();
        f.k.hubs(&hub_opts(), &opts()).unwrap();
        f.k.set_answer_build(Some("b2".into()));
        f.k.hubs(&hub_opts(), &opts()).unwrap();
        let c = f.k.answer_counters();
        assert_eq!((c.hits, c.misses, c.writes), (0, 2, 2));
        assert_eq!(f.entries().len(), 1, "the same key, replaced");
        f.k.hubs(&hub_opts(), &opts()).unwrap();
        assert_eq!(f.k.answer_counters().hits, 1);
    }

    #[test]
    fn ignored_claimed_file_misses() {
        let f = fx();
        f.k.hubs(&hub_opts(), &opts()).unwrap();
        // An ignored file is not admitted, so the index and the position stay: still a hit.
        std::fs::write(
            f.repo.join("ignored.tx"),
            "sym fn hidden\nlink hidden calls hub\n",
        )
        .unwrap();
        f.k.notify(&["ignored.tx".to_string()], false).unwrap();
        let before = f.k.answer_counters();
        f.k.hubs(&hub_opts(), &opts()).unwrap();
        assert_eq!(f.k.answer_counters().hits, before.hits + 1);
        // Once the file is tracked despite the rule, it is part of the index and of the key.
        git(&f.repo, &["add", "-f", "ignored.tx"]);
        git(&f.repo, &["commit", "-q", "-m", "track"]);
        let before = f.k.answer_counters();
        let r = f.k.hubs(&hub_opts(), &opts()).unwrap();
        assert!(r.report.exact);
        let c = f.k.answer_counters();
        assert_eq!(c.hits, before.hits);
        assert_eq!(c.misses, before.misses + 1);
    }

    #[test]
    fn history_index_misses() {
        let f = fx();
        // A commit that names an item, so history has a fragment to index.
        std::fs::write(
            f.repo.join("h.tx"),
            "sym fn other\nlink other calls hub\n# x\n",
        )
        .unwrap();
        git(&f.repo, &["add", "-A"]);
        git(
            &f.repo,
            &["commit", "-q", "-m", "change\n\nRefs: kanbanr:FEAT-001"],
        );
        f.k.hubs(&hub_opts(), &opts()).unwrap();
        let first = f.k.answer_counters();
        assert_eq!(first.writes, 1);
        f.k.index(None, false, Some(&HistoryScope::default()), &opts())
            .unwrap();
        let r = f.k.hubs(&hub_opts(), &opts()).unwrap();
        assert!(r.report.exact);
        let c = f.k.answer_counters();
        assert_eq!(c.hits, first.hits, "history fragments changed the index");
        assert_eq!(c.misses, first.misses + 1);
    }

    #[test]
    fn no_build_stores_nothing() {
        let mut f = fx();
        f.k.set_answer_build(None);
        run_all(&f.k, &opts());
        run_all(&f.k, &opts());
        assert!(!f.answers_dir().exists());
        assert_eq!(f.k.answer_counters(), Default::default());
    }

    #[test]
    fn zero_limit_disables() {
        let f = fx();
        let config = f.kx.join("projects/demo/config.yaml");
        let text = std::fs::read_to_string(&config).unwrap();
        std::fs::write(
            &config,
            text.replace("settings: {}", "settings:\n  answers.max_mib: 0"),
        )
        .unwrap();
        let mut k = f.fresh_facade(None);
        k.set_answer_build(Some("b1".into()));
        run_all(&k, &opts());
        assert!(!f.answers_dir().exists());
        assert_eq!(k.answer_counters(), Default::default());
    }

    #[test]
    fn fresh_recomputes_and_rewrites() {
        let f = fx();
        let first = json(&f.k.hubs(&hub_opts(), &opts()).unwrap().outcome);
        // Damage the stored payload without breaking the entry's shape.
        let e = &f.entries()[0];
        let path = f
            .answers_dir()
            .join(&e.header.view_id[3..5])
            .join(format!("{}.answer", e.header.view_id));
        let mut bytes = std::fs::read(&path).unwrap();
        let at = bytes.iter().position(|b| *b == b'\n').unwrap() + 1;
        bytes[at + 2] = b'#';
        std::fs::write(&path, &bytes).unwrap();
        let o = Options {
            fresh: true,
            ..opts()
        };
        let before = f.k.answer_counters();
        let again = json(&f.k.hubs(&hub_opts(), &o).unwrap().outcome);
        assert_eq!(first, again, "fresh is the computation, not the entry");
        let c = f.k.answer_counters();
        assert_eq!(c.hits, before.hits);
        assert_eq!(c.writes, before.writes + 1, "and the entry is rewritten");
        assert_eq!(std::fs::read(&path).unwrap().len(), bytes.len());
        assert_ne!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn kindex_status_stays_clean() {
        let f = fx();
        git(&f.kx, &["init", "-q"]);
        let before = std::process::Command::new("git")
            .current_dir(&f.kx)
            .args(["status", "--porcelain", "--untracked-files=all"])
            .output()
            .unwrap();
        run_all(&f.k, &opts());
        assert!(f.answers_dir().exists());
        let ignore = std::process::Command::new("git")
            .current_dir(&f.kx)
            .args(["check-ignore", "-q"])
            .arg(
                f.entries()
                    .first()
                    .map(|e| {
                        f.answers_dir()
                            .join(&e.header.view_id[3..5])
                            .join(format!("{}.answer", e.header.view_id))
                    })
                    .unwrap(),
            )
            .status()
            .unwrap();
        assert!(ignore.success(), "entries are git-ignored");
        let after = std::process::Command::new("git")
            .current_dir(&f.kx)
            .args(["status", "--porcelain", "--untracked-files=all"])
            .output()
            .unwrap();
        assert_eq!(before.stdout, after.stdout, "no new file shows in status");
    }

    #[test]
    fn entries_under_cache() {
        let f = fx();
        run_all(&f.k, &opts());
        let root = f.answers_dir();
        assert_eq!(root, f.kx.join("projects/demo/.cache/answers"));
        for e in f.entries() {
            let id = &e.header.view_id;
            assert!(id.starts_with("v1-"));
            assert!(root.join(&id[3..5]).join(format!("{id}.answer")).is_file());
        }
        // Nothing beside the project's own cache and config files was added.
        assert!(!f.kx.join("projects/demo/answers").exists());
    }

    #[test]
    fn stale_ok_exact_reads_stored() {
        // A stale-ok read does not reconcile, so it is never exact and never touches the store.
        // A read that waits, while another process holds the writer, takes the reader path and
        // still reads and writes entries without the processor lock.
        let f = fx();
        let waited = json(&f.k.hubs(&hub_opts(), &opts()).unwrap().outcome);
        let o = Options {
            consistency: Some(Consistency::StaleOk),
            ..opts()
        };
        let before = f.k.answer_counters();
        let stale = f.k.hubs(&hub_opts(), &o).unwrap();
        assert!(!stale.report.exact);
        assert_eq!(json(&stale.outcome), waited);
        assert_eq!(f.k.answer_counters(), before);
        let dir = {
            let kindex =
                katalogr_store::Kindex::open(camino::Utf8Path::from_path(&f.kx).unwrap()).unwrap();
            kindex
                .project(&katalogr_store::ProjectName::new("demo").unwrap())
                .unwrap()
        };
        let _writer = katalogr_store::Cache::open(
            &dir,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
        let reader = f.fresh_facade(None);
        let held = reader.hubs(&hub_opts(), &opts()).unwrap();
        assert!(held.report.exact);
        assert_eq!(json(&held.outcome), waited);
        assert_eq!(reader.answer_counters().hits, 1);
    }

    #[test]
    fn generic_request_round_trip() {
        let f = fx();
        let key = format!("v1-{}", "ab".repeat(16));
        let req = |storable| AnswerRequest {
            view_id: key.clone(),
            op: "custom",
            args: BTreeMap::from([("n".to_string(), "1".to_string())]),
            inputs: BTreeMap::from([("side".to_string(), "x".to_string())]),
            storable,
        };
        let mut calls = 0;
        let mut warnings = Vec::new();
        let mut go = |r: &AnswerRequest, fresh: bool, calls: &mut u32| {
            f.k.through_store(
                r,
                &mut warnings,
                || {
                    *calls += 1;
                    Ok(Outcome::Found(vec!["a".to_string(), "b".to_string()]))
                },
                fresh,
            )
            .unwrap()
        };
        let a = go(&req(true), false, &mut calls);
        let b = go(&req(true), false, &mut calls);
        assert_eq!(a, b);
        assert_eq!(calls, 1, "the second was served");
        // An input that differs is another validity: computed again.
        let mut other = req(true);
        other.inputs.insert("side".into(), "y".into());
        go(&other, false, &mut calls);
        assert_eq!(calls, 2);
        // Not storable: neither read nor written.
        go(&req(false), false, &mut calls);
        assert_eq!(calls, 3);
        // Fresh computes and still writes.
        go(&req(true), true, &mut calls);
        assert_eq!(calls, 4);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(f.entries().len(), 1);
        assert_eq!(f.entries()[0].header.op, "custom");
    }

    #[test]
    fn facade_runs_on_memory_store() {
        let mut f = fx();
        let mem = Arc::new(MemoryStore::default());
        f.k.set_answer_store(mem.clone());
        let first = run_all(&f.k, &opts());
        let second = run_all(&f.k, &opts());
        assert_eq!(first, second);
        let c = f.k.answer_counters();
        assert_eq!((c.hits, c.misses, c.writes), (5, 5, 5));
        assert_eq!(mem.list().unwrap().len(), 5);
        assert!(!f.answers_dir().exists(), "nothing went to disk");
    }

    #[test]
    fn corrupt_entry_recomputes() {
        let f = fx();
        let first = json(&f.k.hubs(&hub_opts(), &opts()).unwrap().outcome);
        let e = &f.entries()[0];
        let path = f
            .answers_dir()
            .join(&e.header.view_id[3..5])
            .join(format!("{}.answer", e.header.view_id));
        let bytes = std::fs::read(&path).unwrap();
        let at = bytes.iter().position(|b| *b == b'\n').unwrap() + 1;
        // Same length, not JSON.
        let mut bad = bytes[..at].to_vec();
        bad.extend(std::iter::repeat_n(b'x', bytes.len() - at));
        std::fs::write(&path, &bad).unwrap();
        let before = f.k.answer_counters();
        let again = json(&f.k.hubs(&hub_opts(), &opts()).unwrap().outcome);
        assert_eq!(first, again);
        let c = f.k.answer_counters();
        assert_eq!(c.unusable, before.unusable + 1);
        assert_eq!(c.hits, before.hits);
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "rewritten whole");
        // A truncated entry is a miss, not an error.
        std::fs::write(&path, &bytes[..bytes.len() - 3]).unwrap();
        let third = json(&f.k.hubs(&hub_opts(), &opts()).unwrap().outcome);
        assert_eq!(first, third);
    }

    #[test]
    fn unwritable_store_still_answers() {
        let f = fx();
        std::fs::create_dir_all(f.answers_dir().parent().unwrap()).unwrap();
        // A file where the folder should be.
        std::fs::write(f.answers_dir(), b"not a folder").unwrap();
        let r = f.k.hubs(&hub_opts(), &opts()).unwrap();
        assert!(matches!(r.outcome, Outcome::Found(_)));
        let stored: Vec<&String> = r
            .report
            .warnings
            .iter()
            .filter(|w| w.starts_with("stored answers:"))
            .collect();
        assert_eq!(stored.len(), 1, "{:?}", r.report.warnings);
        assert_eq!(f.k.answer_counters().writes, 0);
        let _: Option<DistError> = None;
    }

    #[test]
    fn coverage_hits() {
        let f = fx();
        let opts_c = CoverageOptions::default();
        let first = f.k.coverage(&opts_c, &opts()).unwrap();
        let again = f.fresh_facade(None).coverage(&opts_c, &opts()).unwrap();
        assert_eq!(json(&first.outcome), json(&again.outcome));
        assert_eq!(f.entries().len(), 1);
        assert_eq!(f.entries()[0].header.op, "coverage");
        let other = f.fresh_facade(None);
        other.coverage(&opts_c, &opts()).unwrap();
        assert_eq!(other.answer_counters().hits, 1);
    }

    #[test]
    fn coverage_ignored_file_misses() {
        let f = fx();
        f.k.coverage(&CoverageOptions::default(), &opts()).unwrap();
        // A new file that no extension claims: the listing changes, the index does not. The
        // position moves with it, so the miss shows the pair is keyed, not that the digest alone is.
        std::fs::write(f.repo.join("note.unclaimed"), "x").unwrap();
        git(&f.repo, &["add", "-f", "note.unclaimed"]);
        git(&f.repo, &["commit", "-q", "-m", "c2"]);
        let before = f.k.answer_counters();
        let r = f.k.coverage(&CoverageOptions::default(), &opts()).unwrap();
        assert!(matches!(r.outcome, Outcome::Found(_)));
        let c = f.k.answer_counters();
        assert_eq!(c.hits, before.hits);
        assert_eq!(c.misses, before.misses + 1);
    }
}
