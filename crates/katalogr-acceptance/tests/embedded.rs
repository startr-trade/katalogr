//! FEAT-017: SQL in Rust and TypeScript code, bound to the tables the repository's migrations
//! create, with the repository's configured default schema (R-13, R-22, R-23, R-25).

mod embedded {
    use camino::{Utf8Path, Utf8PathBuf};
    use katalogr_acceptance::{full_registry, index_case};
    use katalogr_engine::{
        Consistency, Engine, EventKind, NewEvent, QueryOptions, Scope, canonical_graph,
    };
    use katalogr_model::{ContractName, Name, RelPath};
    use katalogr_store::{Kindex, ProjectConfig, ProjectName, RepoConfig};
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::{Duration, Instant};

    fn fixtures() -> Utf8PathBuf {
        Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
    }

    /// `(from, kind, to, confidence, origin)` of every link in a canonical graph.
    type Link = (String, String, String, String, String);

    fn links(graph: &str) -> BTreeSet<Link> {
        let mut out = BTreeSet::new();
        for line in graph.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            if v["link"] == true {
                let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
                out.insert((s("from"), s("kind"), s("to"), s("confidence"), s("origin")));
            }
        }
        out
    }

    fn contract(key: &str) -> String {
        ContractName {
            kind: "sql_table".into(),
            key: key.into(),
        }
        .id()
        .to_hex()
    }

    fn incoming(links: &BTreeSet<Link>, key: &str) -> Vec<(String, String, String, String)> {
        let c = contract(key);
        links
            .iter()
            .filter(|l| l.2 == c)
            .map(|l| (l.0.clone(), l.1.clone(), l.3.clone(), l.4.clone()))
            .collect()
    }

    fn consumers(links: &BTreeSet<Link>, key: &str) -> Vec<String> {
        incoming(links, key)
            .into_iter()
            .filter(|l| l.1 == "consumes")
            .map(|l| l.0)
            .collect()
    }

    fn providers(links: &BTreeSet<Link>, key: &str) -> Vec<String> {
        incoming(links, key)
            .into_iter()
            .filter(|l| l.1 == "provides")
            .map(|l| l.0)
            .collect()
    }

    #[test]
    fn rust_queries_bind_tables() {
        let g = index_case(&full_registry, &fixtures().join("rust/embedded-sql")).unwrap();
        let l = links(&g);
        let inc = incoming(&l, "public.orders");
        assert!(
            inc.contains(&(
                "fx:src/lib.rs#fn:load_orders".into(),
                "consumes".into(),
                "Inferred".into(),
                "intra".into()
            )),
            "{inc:#?}"
        );
        assert_eq!(
            providers(&l, "public.orders"),
            vec!["fx:migrations/V1__init.sql#table:public.orders"]
        );
        // the CALL literal binds a routine; prose and URL literals bind nothing
        let routine = ContractName {
            kind: "sql_routine".into(),
            key: "app.purge".into(),
        }
        .id()
        .to_hex();
        assert!(
            l.iter()
                .any(|x| x.0 == "fx:src/lib.rs#fn:purge" && x.2 == routine)
        );
        for f in ["fn:greeting", "fn:orders_url"] {
            let from = format!("fx:src/lib.rs#{f}");
            assert!(
                !l.iter().any(|x| x.0 == from && x.1 == "consumes"),
                "{f} binds nothing"
            );
        }
    }

    #[test]
    fn ts_queries_bind_tables() {
        let g = index_case(&full_registry, &fixtures().join("ts/embedded-sql")).unwrap();
        let l = links(&g);
        let inc = incoming(&l, "public.orders");
        assert!(
            inc.contains(&(
                "fx:src/db.ts#fn:loadOrders".into(),
                "consumes".into(),
                "Inferred".into(),
                "intra".into()
            )),
            "{inc:#?}"
        );
        assert_eq!(
            providers(&l, "public.orders"),
            vec!["fx:migrations/V1__init.sql#table:public.orders"]
        );
        for f in ["fn:ordersUrl"] {
            let from = format!("fx:src/db.ts#{f}");
            assert!(!l.iter().any(|x| x.0 == from && x.1 == "consumes"));
        }
    }

    #[test]
    fn literals_and_migrations_share_contract() {
        let g = index_case(&full_registry, &fixtures().join("embedded/schema-app")).unwrap();
        let l = links(&g);
        let mut c = consumers(&l, "app.orders");
        c.sort();
        assert_eq!(
            c,
            vec![
                "fx:src/db.rs#fn:load_orders".to_string(),
                "fx:web/db.ts#fn:loadOrders".to_string()
            ]
        );
        assert_eq!(
            providers(&l, "app.orders"),
            vec!["fx:src/main/resources/db/migration/V1__init.sql#table:app.orders"]
        );
        for other in ["public.orders", "other.orders"] {
            assert!(incoming(&l, other).is_empty(), "{other}");
        }
    }

    // ---- engine-driven scenarios ---------------------------------------------------------

    struct Env {
        _tmp: tempfile::TempDir,
        work: Utf8PathBuf,
        engine: Engine,
    }

    fn open(work_files: &[(String, String)]) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        for (p, t) in work_files {
            let f = work.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, t).unwrap();
        }
        let dir = Kindex::init(&root.join("kx"))
            .unwrap()
            .create_project(
                &ProjectName::new("p").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let roots = BTreeMap::from([("r".to_string(), work.clone())]);
        let engine = Engine::open(
            dir,
            full_registry(),
            roots,
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
        Env {
            _tmp: tmp,
            work,
            engine,
        }
    }

    impl Env {
        fn drain(&self) -> String {
            let a = self
                .engine
                .with_consistency(
                    &QueryOptions {
                        consistency: Consistency::WaitAll,
                        timeout: Duration::from_secs(120),
                        scope: Scope::Project,
                    },
                    |tx| canonical_graph(tx),
                )
                .unwrap();
            assert!(!a.stale);
            a.value
        }

        fn index(&self) -> String {
            self.engine.inbox().append(&NewEvent::rescan("r")).unwrap();
            self.drain()
        }

        fn event(&self, path: &str, kind: EventKind) {
            self.engine
                .inbox()
                .append(&NewEvent::file("r", &RelPath::new(path).unwrap(), kind))
                .unwrap();
        }

        fn write(&self, path: &str, text: &str) {
            std::fs::write(self.work.join(path), text).unwrap();
            self.event(path, EventKind::Changed);
        }

        fn revision(&self, path: &str) -> String {
            self.engine
                .dir()
                .read_shard("r", &RelPath::new(path).unwrap())
                .unwrap()
                .unwrap()
                .provenance
                .revision
        }
    }

    fn copy_case(case: &str) -> Vec<(String, String)> {
        fn walk(dir: &Utf8Path, base: &Utf8Path, out: &mut Vec<(String, String)>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = Utf8PathBuf::from_path_buf(e.path()).unwrap();
                if p.is_dir() {
                    walk(&p, base, out);
                } else if p.file_name() != Some("expected.yaml") {
                    out.push((
                        p.strip_prefix(base).unwrap().to_string(),
                        std::fs::read_to_string(&p).unwrap(),
                    ));
                }
            }
        }
        let base = fixtures().join(case);
        let mut out = Vec::new();
        walk(&base, &base, &mut out);
        out
    }

    #[test]
    fn schema_change_reextracts_hosts() {
        let e = open(&copy_case("embedded/schema-app"));
        let g = e.index();
        assert!(!consumers(&links(&g), "app.orders").is_empty());
        let yml = "src/main/resources/application.yml";
        e.write(yml, "spring:\n  flyway:\n    default-schema: billing\n");
        let l = links(&e.drain());
        let mut c = consumers(&l, "billing.orders");
        c.sort();
        assert_eq!(
            c,
            vec![
                "r:src/db.rs#fn:load_orders".to_string(),
                "r:web/db.ts#fn:loadOrders".to_string()
            ]
        );
        assert_eq!(
            providers(&l, "billing.orders"),
            vec!["r:src/main/resources/db/migration/V1__init.sql#table:billing.orders"]
        );
        assert!(incoming(&l, "app.orders").is_empty());
        // the shard records where the binding came from
        let shard = e
            .engine
            .dir()
            .read_shard("r", &RelPath::new("src/db.rs").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(shard.bindings.len(), 1);
        assert_eq!(shard.bindings[0].origin.as_deref(), Some("embedded:sql"));
        // the config goes away: everything is `public` again (R-21)
        std::fs::remove_file(e.work.join(yml)).unwrap();
        e.event(yml, EventKind::Deleted);
        let l = links(&e.drain());
        assert_eq!(consumers(&l, "public.orders").len(), 2);
        assert_eq!(providers(&l, "public.orders").len(), 1);
        assert!(incoming(&l, "billing.orders").is_empty());
    }

    #[test]
    fn config_churn_bounds_host_reextraction() {
        const N: usize = 500;
        let mut files: Vec<(String, String)> = Vec::new();
        for i in 0..N {
            files.push((
                format!("src/m{i}.rs"),
                format!("pub fn f{i}() -> String {{\n    sqlx::query(\"SELECT id FROM orders WHERE n = {i}\")\n}}\n"),
            ));
            files.push((
                format!("web/m{i}.ts"),
                format!("export function f{i}() {{\n  return db.query(\"SELECT id FROM orders WHERE n = {i}\");\n}}\n"),
            ));
        }
        let conf = |url: &str, schema: &str, comment: &str| {
            format!("# {comment}\nflyway.url={url}\nflyway.defaultSchema={schema}\n")
        };
        files.push((
            "flyway.conf".into(),
            conf("jdbc:postgresql://h/db", "app", "start"),
        ));
        files.push((
            "db/V1__init.sql".into(),
            "CREATE TABLE orders (id bigint PRIMARY KEY);\n".into(),
        ));
        let e = open(&files);
        e.index();
        let hosts: Vec<String> = (0..N)
            .flat_map(|i| [format!("src/m{i}.rs"), format!("web/m{i}.ts")])
            .collect();
        let snapshot = |e: &Env| -> Vec<String> { hosts.iter().map(|h| e.revision(h)).collect() };
        let changed = |a: &[String], b: &[String]| a.iter().zip(b).filter(|(x, y)| x != y).count();
        let mut before = snapshot(&e);
        let mut sql_rev = e.revision("db/V1__init.sql");
        let mut sql_changes = 0;
        for n in 0..19 {
            // Unrelated edits: a comment, a URL, and a dialect flip (the SQL context changes,
            // the default schema does not).
            let text = match n % 3 {
                0 => conf("jdbc:postgresql://h/db", "app", &format!("edit {n}")),
                1 => conf(&format!("jdbc:postgresql://h{n}/db"), "app", "url"),
                _ => {
                    let d = if n % 2 == 0 { "postgresql" } else { "mysql" };
                    conf(&format!("jdbc:{d}://h/db"), "app", "dialect")
                }
            };
            e.write("flyway.conf", &text);
            e.drain();
            let after = snapshot(&e);
            assert_eq!(
                changed(&before, &after),
                0,
                "edit {n} re-extracted host files"
            );
            before = after;
            let r = e.revision("db/V1__init.sql");
            if r != sql_rev {
                sql_changes += 1;
            }
            sql_rev = r;
        }
        assert!(sql_changes >= 1, "a context change re-extracts SQL files");
        // The schema edit changes every host revision.
        e.write(
            "flyway.conf",
            &conf("jdbc:postgresql://h/db", "billing", "schema"),
        );
        e.drain();
        let after = snapshot(&e);
        assert_eq!(changed(&before, &after), 2 * N);
        before = after;
        // Five edits coalesced into one batch, one of them a schema change.
        for (k, schema) in ["billing", "billing", "ledger", "ledger", "ledger"]
            .iter()
            .enumerate()
        {
            e.write(
                "flyway.conf",
                &conf("jdbc:postgresql://h/db", schema, &format!("batch {k}")),
            );
        }
        e.drain();
        let after = snapshot(&e);
        assert_eq!(changed(&before, &after), 2 * N);
    }
}
