//! FEAT-017 SQL extension tests.

use camino::{Utf8Path, Utf8PathBuf};
use katalogr_ext_sql::{SqlContext, SqlExtractor};
use katalogr_model::{
    ArtifactName, ExtractError, ExtractInput, Extractor, Fact, Fragment, Id, Name, RelPath,
};
use std::collections::BTreeMap;

fn fixtures() -> Utf8PathBuf {
    Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sql")
}

fn walk(dir: &Utf8Path, base: &Utf8Path, out: &mut Vec<(String, Vec<u8>)>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| Utf8PathBuf::from_path_buf(e.path()).unwrap())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, base, out);
        } else if p.file_name() != Some("expected.yaml") {
            out.push((
                p.strip_prefix(base).unwrap().to_string(),
                std::fs::read(&p).unwrap(),
            ));
        }
    }
}

/// The files of a text corpus, as `(path, bytes)`.
type Files = Vec<(String, Vec<u8>)>;

fn case_files(case: &str) -> Files {
    let dir = fixtures().join(case);
    let mut out = Vec::new();
    walk(&dir, &dir, &mut out);
    out
}

/// What the engine would do for the SQL extension: build the context from the config files,
/// then extract every handled file.
fn extract_all(files: &Files) -> BTreeMap<String, (Fragment, Vec<ExtractError>)> {
    let ext = SqlExtractor;
    let cfg: Vec<(RelPath, Vec<u8>)> = files
        .iter()
        .map(|(p, b)| (RelPath::new(p).unwrap(), b.clone()))
        .filter(|(p, _)| ext.is_context_file(p))
        .collect();
    let ctx = ext.build_context(&cfg).unwrap();
    let mut out = BTreeMap::new();
    for (p, b) in files {
        let rp = RelPath::new(p).unwrap();
        if !ext.handles(&rp) {
            continue;
        }
        let r = ext
            .extract_with_errors(&ExtractInput {
                repo: "fx",
                path: &rp,
                revision: "rev",
                content: b,
                context: &ctx,
            })
            .unwrap();
        out.insert(p.clone(), r);
    }
    out
}

fn case(name: &str) -> BTreeMap<String, (Fragment, Vec<ExtractError>)> {
    extract_all(&case_files(name))
}

/// `symbol role contract confidence` lines of a fragment, sorted; the symbol is its name after
/// `#` (empty for the artifact).
fn binding_lines(f: &Fragment) -> Vec<String> {
    let mut names: BTreeMap<Id, String> = BTreeMap::new();
    names.insert(f.artifact.name.id(), String::new());
    for s in &f.symbols {
        let c = s.name.canonical();
        names.insert(s.name.id(), c.split_once('#').unwrap().1.to_string());
    }
    let mut v: Vec<String> = f
        .bindings
        .iter()
        .map(|b| {
            format!(
                "{} {} {} {:?}{}",
                names[&b.symbol],
                b.role.as_str(),
                b.contract.canonical(),
                b.confidence,
                b.origin
                    .as_deref()
                    .map(|o| format!(" {o}"))
                    .unwrap_or_default()
            )
        })
        .collect();
    v.sort();
    v
}

fn has(lines: &[String], want: &str) -> bool {
    lines.iter().any(|l| l == want)
}

fn sym<'a>(f: &'a Fragment, suffix: &str) -> &'a katalogr_model::SymbolNode {
    f.symbols
        .iter()
        .find(|s| s.name.canonical().ends_with(&format!("#{suffix}")))
        .unwrap_or_else(|| {
            panic!(
                "no symbol {suffix}; have {:?}",
                f.symbols
                    .iter()
                    .map(|s| s.name.canonical())
                    .collect::<Vec<_>>()
            )
        })
}

fn fact<'a>(s: &'a katalogr_model::SymbolNode, k: &str) -> Option<&'a Fact> {
    s.facts.get(k)
}

fn st(v: &str) -> Fact {
    Fact::Str(v.to_string())
}

#[allow(dead_code)]
fn artifact_id(path: &str) -> Id {
    ArtifactName::new("fx", path).unwrap().id()
}

mod sql {
    use super::*;
    use katalogr_ext_sql::*;
    use katalogr_model::RelPath;

    fn files(list: &[(&str, &str)]) -> Vec<(RelPath, Vec<u8>)> {
        let mut v: Vec<(RelPath, Vec<u8>)> = list
            .iter()
            .map(|(p, t)| (RelPath::new(p).unwrap(), t.as_bytes().to_vec()))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    fn ctx(list: &[(&str, &str)]) -> SqlContext {
        SqlContext::build(&files(list))
    }

    #[test]
    fn contract_keys_normalised() {
        for raw in [
            "Orders",
            "\"orders\"",
            "public.orders",
            "main.orders",
            "db.public.orders",
            "[Orders]",
            "`ORDERS`",
        ] {
            let parts: Vec<&str> = raw.split('.').collect();
            assert_eq!(table_key(&parts, "public"), "public.orders", "{raw}");
        }
        assert_eq!(table_key(&["`App`", "`T`"], "public"), "app.t");
        assert_eq!(table_key(&["orders"], "app"), "app.orders");
        assert_eq!(table_key(&["public", "orders"], "app"), "public.orders");
        assert_eq!(table_key(&["main", "orders"], "app"), "public.orders");
        assert_eq!(normalise_ident("\"a\"\"B\""), "a\"b");
        assert_eq!(
            column_key("public.orders", "\"Total\""),
            "public.orders.total"
        );
        assert_eq!(routine_key(&["Touch"], "app"), "app.touch");
        assert!(is_system_relation(&["information_schema", "tables"]));
        assert!(is_system_relation(&["pg_class"]));
        assert!(is_system_relation(&["DUAL"]));
        assert!(!is_system_relation(&["app", "orders"]));
    }

    #[test]
    fn default_schema_from_flyway_config() {
        let one = |p: &str, t: &str| ctx(&[(p, t)]);
        let s = |c: &SqlContext| c.default_schema().to_string();
        assert_eq!(s(&one("flyway.conf", "flyway.defaultSchema=App\n")), "app");
        assert_eq!(s(&one("flyway.conf", "flyway.schemas=s1,s2\n")), "s1");
        assert_eq!(
            s(&one(
                "flyway.conf",
                "flyway.schemas=s1,s2\nflyway.defaultSchema=d\n"
            )),
            "d"
        );
        assert_eq!(
            s(&one("conf/flyway.conf", "flyway.defaultSchema=viaconf\n")),
            "viaconf"
        );
        assert_eq!(
            s(&one(
                "flyway.toml",
                "[flyway]\ndefaultSchema = \"tomlschema\"\n"
            )),
            "tomlschema"
        );
        assert_eq!(
            s(&one(
                "flyway.toml",
                "[environments.dev]\nurl = \"jdbc:postgresql://h/db\"\nschemas = [\"envs\", \"b\"]\n"
            )),
            "envs"
        );
        assert_eq!(
            s(&one(
                "src/main/resources/application.properties",
                "spring.flyway.default-schema=app\n"
            )),
            "app"
        );
        assert_eq!(
            s(&one(
                "application.yml",
                "spring:\n  flyway:\n    schemas: [x, y]\n"
            )),
            "x"
        );
        assert_eq!(
            s(&one(
                "application.yaml",
                "spring:\n  flyway:\n    default_schema: snake\n"
            )),
            "snake"
        );
        assert_eq!(
            s(&one("flyway.conf", "flyway.defaultSchema=main\n")),
            "public"
        );
        // a root config beats a nested one; the loser is listed
        let c = ctx(&[
            ("flyway.conf", "flyway.defaultSchema=root\n"),
            ("svc/flyway.conf", "flyway.defaultSchema=svc\n"),
            ("svc2/flyway.conf", "flyway.defaultSchema=root\n"),
        ]);
        assert_eq!(c.default_schema(), "root");
        assert_eq!(c.schema.as_ref().unwrap().source, "flyway.conf");
        assert_eq!(
            c.schema_ignored,
            vec![SchemaChoice {
                name: "svc".into(),
                source: "svc/flyway.conf".into()
            }]
        );
        // test resources are refused
        assert!(is_test_resource("src/test/resources/application.yml"));
        assert!(is_test_resource("svc/src/test/resources/application.yml"));
        assert!(!is_test_resource("src/main/resources/application.yml"));
        let c = ctx(&[
            (
                "src/main/resources/application.yml",
                "spring: {flyway: {default-schema: app}}\n",
            ),
            (
                "src/test/resources/application.yml",
                "spring: {flyway: {default-schema: other}}\n",
            ),
        ]);
        assert_eq!(c.default_schema(), "app");
        // no config: public, empty bytes
        let c = ctx(&[]);
        assert_eq!(c.default_schema(), "public");
        assert!(c.to_bytes().is_empty());
        assert!(c.embedded_bytes().is_empty());
        let c = ctx(&[("flyway.conf", "flyway.defaultSchema=app\n")]);
        assert_eq!(c.embedded_bytes(), b"app");
        assert_eq!(SqlContext::from_bytes(&c.to_bytes()), c);
        assert_eq!(schema_from_embedded(b"app"), "app");
        assert_eq!(schema_from_embedded(b""), "public");
        // a dialect-only context has no embedded bytes
        let c = ctx(&[("flyway.conf", "flyway.url=jdbc:mysql://h/db\n")]);
        assert!(!c.to_bytes().is_empty());
        assert!(c.embedded_bytes().is_empty());
        // secrets never reach the bytes
        let c = ctx(&[(
            "flyway.conf",
            "flyway.password=secret\nflyway.url=jdbc:postgresql://u:secret@h/db\nflyway.defaultSchema=app\n",
        )]);
        let bytes = String::from_utf8(c.to_bytes()).unwrap();
        assert!(!bytes.contains("secret"), "{bytes}");
        assert!(bytes.contains("postgresql") || bytes.contains("PostgreSql"));
    }

    #[test]
    fn flyway_migration_names() {
        let n = |p: &str| parse_migration_name(p);
        let v = n("db/migration/V1__init.sql").unwrap();
        assert_eq!(v.scheme, MigrationScheme::Versioned);
        assert_eq!(v.version.as_deref(), Some("1"));
        assert_eq!(v.description, "init");
        assert_eq!(
            n("V1_2__add_x.sql").unwrap().version.as_deref(),
            Some("1.2")
        );
        assert_eq!(n("V1_2__add_x.sql").unwrap().description, "add x");
        assert_eq!(n("V010__x.sql").unwrap().version.as_deref(), Some("10"));
        let u = n("U3__undo.sql").unwrap();
        assert_eq!(
            (u.scheme, u.version.as_deref()),
            (MigrationScheme::Undo, Some("3"))
        );
        let r = n("R__views.sql").unwrap();
        assert_eq!(
            (r.scheme, r.version.clone(), r.order.clone()),
            (MigrationScheme::Repeatable, None, None)
        );
        assert_eq!(r.description, "views");
        let up = n("0002_add.up.sql").unwrap();
        assert_eq!(
            (up.scheme, up.version.as_deref()),
            (MigrationScheme::Up, Some("2"))
        );
        let down = n("2024-01-01-000000_init/down.sql").unwrap();
        assert_eq!(down.scheme, MigrationScheme::Down);
        assert_eq!(down.version.as_deref(), Some("20240101000000"));
        assert_eq!(down.description, "init");
        assert!(n("notes.sql").is_none());
        assert!(n("Users.sql").is_none());
        // order strings sort numerically
        let o = |p: &str| n(p).unwrap().order.unwrap();
        assert!(o("V2__a.sql") < o("V10__a.sql"));
        assert!(o("V1_2__a.sql") < o("V1_10__a.sql"));
        assert!(o("V1__a.sql") < o("V1_1__a.sql"));
    }

    #[test]
    fn dialect_from_context() {
        // flyway.conf with url and filesystem locations
        let c = ctx(&[(
            "db/flyway.conf",
            "flyway.url=jdbc:postgresql://u:p@h/db\nflyway.locations=filesystem:sql/pg,filesystem:../shared,filesystem:../../out,classpath:x\n",
        )]);
        assert_eq!(c.dialect_for("db/a.sql").unwrap().0, Dialect::PostgreSql);
        assert_eq!(
            c.dialect_for("db/sql/pg/a.sql").unwrap().1,
            "db/flyway.conf"
        );
        assert_eq!(
            c.dialect_for("shared/a.sql").unwrap().0,
            Dialect::PostgreSql
        );
        assert!(c.dialect_for("out/a.sql").is_none(), "left the repo");
        assert!(c.dialect_for("other/a.sql").is_none());
        assert!(
            c.dialect_for("dbx/a.sql").is_none(),
            "prefix at a / boundary only"
        );
        // conf/flyway.conf resolves from the grandparent
        let c = ctx(&[("conf/flyway.conf", "flyway.url=jdbc:mariadb://h/db\n")]);
        assert_eq!(c.dialect_for("a.sql").unwrap().0, Dialect::MySql);
        // flyway.toml, two environments, environment named
        let toml = "[flyway]\nenvironment = \"prod\"\nlocations = [\"filesystem:migrations\"]\n\
                    [environments.dev]\nurl = \"jdbc:sqlite:dev.db\"\n\
                    [environments.prod]\nurl = \"jdbc:postgresql://h/db\"\n";
        let c = ctx(&[("flyway.toml", toml)]);
        assert_eq!(
            c.dialect_for("migrations/a.sql").unwrap().0,
            Dialect::PostgreSql
        );
        let c = ctx(&[("flyway.toml", &toml.replace("environment = \"prod\"\n", ""))]);
        assert_eq!(
            c.dialect_for("a.sql").unwrap().0,
            Dialect::Sqlite,
            "first sorted"
        );
        // sqlc with a list of schema paths
        let c = ctx(&[(
            "sqlc.yaml",
            "version: '2'\nsql:\n  - engine: mysql\n    schema: [db/schema, db/more/s.sql]\n    queries: db/q\n",
        )]);
        assert_eq!(c.dialect_for("db/schema/a.sql").unwrap().0, Dialect::MySql);
        assert_eq!(c.dialect_for("db/more/b.sql").unwrap().0, Dialect::MySql);
        assert_eq!(c.dialect_for("db/q/c.sql").unwrap().0, Dialect::MySql);
        // .sqlfluff
        let c = ctx(&[("svc/.sqlfluff", "[sqlfluff]\ndialect = postgres\n")]);
        assert_eq!(c.dialect_for("svc/x.sql").unwrap().0, Dialect::PostgreSql);
        // spring defaults
        let c = ctx(&[(
            "src/main/resources/application.yml",
            "spring:\n  flyway:\n    url: jdbc:sqlite:x.db\n",
        )]);
        assert_eq!(
            c.dialect_for("src/main/resources/db/migration/V1__a.sql")
                .unwrap()
                .0,
            Dialect::Sqlite
        );
        // nested dirs: the longest prefix wins
        let c = ctx(&[
            ("flyway.conf", "flyway.url=jdbc:postgresql://h/db\n"),
            ("legacy/flyway.conf", "flyway.url=jdbc:mysql://h/db\n"),
        ]);
        assert_eq!(c.dialect_for("legacy/a.sql").unwrap().0, Dialect::MySql);
        assert_eq!(c.dialect_for("new/a.sql").unwrap().0, Dialect::PostgreSql);
        // equal dirs: the lower source rank first (flyway.toml before flyway.conf)
        let c = ctx(&[
            ("flyway.conf", "flyway.url=jdbc:mysql://h/db\n"),
            ("flyway.toml", "[environments.e]\nurl = \"jdbc:sqlite:x\"\n"),
        ]);
        assert_eq!(
            c.dialect_for("a.sql").unwrap(),
            (Dialect::Sqlite, "flyway.toml")
        );
        // a bad config is skipped
        let c = ctx(&[
            ("flyway.toml", "this is = = not toml ["),
            ("sqlc.yaml", "{{{{"),
            ("flyway.conf", "flyway.url=jdbc:sqlite:z\n"),
        ]);
        assert_eq!(c.dialect_for("a.sql").unwrap().0, Dialect::Sqlite);
        // suffix beats context
        let c = ctx(&[("flyway.conf", "flyway.url=jdbc:mysql://h/db\n")]);
        assert_eq!(
            resolve("a.pgsql", &c),
            (Some(Dialect::PostgreSql), DialectSource::Suffix)
        );
        assert_eq!(
            resolve("a.sql", &c),
            (
                Some(Dialect::MySql),
                DialectSource::Context("flyway.conf".into())
            )
        );
        assert_eq!(
            resolve("a.sql", &SqlContext::default()),
            (None, DialectSource::Auto)
        );
        // chains
        assert_eq!(chain(None).len(), 4);
        assert_eq!(
            chain(Some(Dialect::MySql)),
            &[Dialect::MySql, Dialect::Generic]
        );
        assert_eq!(Dialect::from_name("PG"), Some(Dialect::PostgreSql));
        assert_eq!(Dialect::from_name("oracle"), None);
    }

    fn stmts(text: &str) -> Vec<String> {
        split_statements(text)
            .iter()
            .map(|s| text[s.start..s.end].to_string())
            .collect()
    }

    #[test]
    fn splitter_handles_quotes_and_bodies() {
        // quotes, comments and dollar quotes hide semicolons
        let t = "SELECT 'a;b'; SELECT \"x;y\"; SELECT `p;q`; -- c;d\nSELECT 1; # e;f\nSELECT 2; /* g; /* h; */ i; */ SELECT 3;\nSELECT $$a;b$$; SELECT $fn$ x; $$ y; $fn$; SELECT 'it''s;';";
        assert_eq!(
            stmts(t),
            vec![
                "SELECT 'a;b'",
                "SELECT \"x;y\"",
                "SELECT `p;q`",
                "SELECT 1",
                "SELECT 2",
                "SELECT 3",
                "SELECT $$a;b$$",
                "SELECT $fn$ x; $$ y; $fn$",
                "SELECT 'it''s;'",
            ]
        );
        // `$1` is not a dollar quote; a backslash escape in a string
        assert_eq!(stmts("SELECT $1; SELECT 2"), vec!["SELECT $1", "SELECT 2"]);
        assert_eq!(
            stmts("SELECT 'a\\'b;c'; SELECT 2"),
            vec!["SELECT 'a\\'b;c'", "SELECT 2"]
        );
        // a bracket identifier, and an array subscript that is not one
        assert_eq!(
            stmts("SELECT [a;b] FROM t; SELECT x[1]; SELECT 2"),
            vec!["SELECT [a;b] FROM t", "SELECT x[1]", "SELECT 2"]
        );
        // DELIMITER with a MySQL procedure
        let m = "DELIMITER //\nCREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\n  SELECT 2;\nEND//\nDELIMITER ;\nSELECT 3;\n";
        let got = stmts(m);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].starts_with("CREATE PROCEDURE p()") && got[0].ends_with("END"));
        assert_eq!(got[1], "SELECT 3");
        assert_eq!(split_statements(m)[0].delimiter, "//");
        assert_eq!(split_statements(m)[1].delimiter, ";");
        // a SQLite trigger BEGIN ... END without DELIMITER
        let tr = "CREATE TRIGGER t AFTER INSERT ON o BEGIN\n  INSERT INTO a VALUES (1);\n  UPDATE b SET x = CASE WHEN 1 THEN 2 END;\nEND;\nSELECT 1;";
        let got = stmts(tr);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].ends_with("END"));
        // a MySQL procedure with IF ... END IF and CASE ... END CASE without DELIMITER
        let p = "CREATE PROCEDURE q()\nBEGIN\n  IF x THEN SELECT 1; END IF;\n  CASE x WHEN 1 THEN SELECT 2; END CASE;\n  WHILE a DO SELECT 3; END WHILE;\nEND;\nSELECT 9;";
        let got = stmts(p);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].ends_with("END") && got[1] == "SELECT 9");
        // psql meta commands and comment-only tails are dropped
        assert_eq!(
            stmts("\\connect shop\nSELECT 1;\n-- bye\n/* x */\n"),
            vec!["SELECT 1"]
        );
        // trailing statements without delimiter; ranges exclude delimiter and trailing space
        let t = "SELECT 1  ;  SELECT 2  \n";
        let r = split_statements(t);
        assert_eq!((r[0].start, r[0].end), (0, 8));
        assert_eq!(&t[r[1].start..r[1].end], "SELECT 2");
        // a pg # operator is not a comment
        assert_eq!(
            stmts("SELECT a #> '{x}' FROM t; SELECT 2"),
            vec!["SELECT a #> '{x}' FROM t", "SELECT 2"]
        );
        // an unterminated string consumes the rest without panicking
        assert_eq!(stmts("SELECT 'abc; SELECT 2").len(), 1);
        assert!(split_statements("").is_empty());
    }

    fn tables(text: &str) -> Vec<String> {
        scan_body(text, "public").tables.into_iter().collect()
    }

    #[test]
    fn scanner_unit_cases() {
        // a plpgsql body: the INTO variable is not a table
        let body = "BEGIN SELECT x INTO v FROM orders WHERE id = 1; INSERT INTO audit(a) VALUES (1); \
                    UPDATE customers SET n = 1; INSERT INTO t2 VALUES (1) ON CONFLICT (id) DO UPDATE SET n = 2; \
                    SELECT 1 FROM t3 FOR UPDATE; END";
        assert_eq!(
            tables(body),
            vec![
                "public.audit",
                "public.customers",
                "public.orders",
                "public.t2",
                "public.t3"
            ]
        );
        // a CTE name is excluded, joins and comma lists are followed, NEW and OLD are not tables
        let q = "WITH x AS (SELECT * FROM a), y AS (SELECT 1) INSERT INTO b SELECT * FROM x, c AS cc, d JOIN y ON 1=1 JOIN app.z ON 1=1";
        assert_eq!(
            tables(q),
            vec!["app.z", "public.a", "public.b", "public.c", "public.d"]
        );
        assert_eq!(
            tables("SELECT NEW.id FROM old, pg_class, information_schema.tables, dual"),
            Vec::<String>::new()
        );
        // routines
        let r = scan_body(
            "PERFORM app.recalc(1); CALL purge(); EXECUTE FUNCTION touch();",
            "public",
        );
        assert_eq!(
            r.routines.into_iter().collect::<Vec<_>>(),
            vec!["app.recalc", "public.purge", "public.touch"]
        );
        // strings and comments are skipped; dollar-quoted bodies are scanned once
        assert_eq!(
            tables("SELECT 'FROM nope' FROM real -- FROM no\n"),
            vec!["public.real"]
        );
        assert_eq!(
            tables("DO $$ BEGIN DELETE FROM gone; END $$"),
            vec!["public.gone"]
        );
        // a table in the table position that is a function call or subquery gives nothing
        assert_eq!(
            tables("SELECT * FROM generate_series(1,2), (SELECT 1) s"),
            Vec::<String>::new()
        );
        assert_eq!(
            tables("TRUNCATE TABLE t9; LOCK TABLE t8"),
            vec!["public.t8", "public.t9"]
        );
        assert_eq!(
            tables("MERGE INTO tgt USING src ON 1=1"),
            vec!["public.src", "public.tgt"]
        );
        // configured schema keys unqualified names
        let r = scan_body("SELECT * FROM orders", "app");
        assert!(r.tables.contains("app.orders"));

        // headers
        let h =
            recognise_header("CREATE DEFINER=`x`@`%` PROCEDURE p()\nBEGIN SELECT 1 FROM t; END")
                .unwrap();
        assert_eq!(
            (h.kind, h.ddl, h.name.clone()),
            (ObjectKind::Procedure, Ddl::Create, vec!["p".to_string()])
        );
        let text = "CREATE DEFINER=`x`@`%` PROCEDURE p()\nBEGIN SELECT 1 FROM t; END";
        assert!(text[h.body_start..].starts_with("BEGIN"));
        let h = recognise_header("CREATE OR REPLACE FUNCTION app.touch() RETURNS trigger AS $$ BEGIN RETURN NEW; END $$ LANGUAGE plpgsql").unwrap();
        assert_eq!(
            (h.kind, h.name),
            (
                ObjectKind::Function,
                vec!["app".to_string(), "touch".to_string()]
            )
        );
        let h = recognise_header("CREATE TRIGGER IF NOT EXISTS tg BEFORE INSERT OR UPDATE ON public.orders FOR EACH ROW EXECUTE FUNCTION touch()").unwrap();
        assert_eq!(h.kind, ObjectKind::Trigger);
        assert_eq!(
            h.on_table,
            Some(vec!["public".to_string(), "orders".to_string()])
        );
        let h = recognise_header("CREATE POLICY own ON orders USING (true)").unwrap();
        assert_eq!(
            (h.kind, h.on_table),
            (ObjectKind::Policy, Some(vec!["orders".to_string()]))
        );
        let h = recognise_header("ALTER TABLE ONLY IF EXISTS s.t ADD COLUMN c int").unwrap();
        assert_eq!(
            (h.kind, h.ddl, h.name),
            (
                ObjectKind::Table,
                Ddl::Alter,
                vec!["s".to_string(), "t".to_string()]
            )
        );
        let h = recognise_header("DROP VIEW IF EXISTS v").unwrap();
        assert_eq!((h.kind, h.ddl), (ObjectKind::View, Ddl::Drop));
        let h = recognise_header("CREATE MATERIALIZED VIEW mv AS SELECT 1").unwrap();
        assert_eq!(h.kind, ObjectKind::MaterializedView);
        let h = recognise_header("DO $$ BEGIN PERFORM f(); END $$").unwrap();
        assert_eq!(h.kind, ObjectKind::Anonymous);
        assert!(recognise_header("SELECT 1").is_none());
        assert!(recognise_header("CREATE INDEX i ON t(a)").is_none());
        assert!(recognise_header("").is_none());
    }

    #[test]
    fn tables_and_columns_per_dialect() {
        // PostgreSQL
        let c = case("tables-pg");
        let (f, errs) = &c["schema.sql"];
        assert!(errs.is_empty(), "{errs:?}");
        let b = binding_lines(f);
        assert!(
            has(
                &b,
                "table:public.orders provides contract:sql_table:public.orders Extracted"
            ),
            "{b:#?}"
        );
        assert!(has(
            &b,
            "table:public.orders/column:total provides contract:sql_column:public.orders.total Extracted"
        ));
        assert!(has(
            &b,
            "table:public.customers provides contract:sql_table:public.customers Extracted"
        ));
        let id = sym(f, "table:public.customers/column:id");
        assert_eq!(fact(id, "primary_key"), Some(&Fact::Bool(true)));
        assert_eq!(fact(id, "nullable"), Some(&Fact::Bool(false)));
        assert_eq!(fact(id, "type"), Some(&st("bigserial")));
        let email = sym(f, "table:public.customers/column:email");
        assert_eq!(fact(email, "nullable"), Some(&Fact::Bool(false)));
        let name = sym(f, "table:public.customers/column:name");
        assert_eq!(fact(name, "nullable"), Some(&Fact::Bool(true)));
        let total = sym(f, "table:public.orders/column:total");
        assert_eq!(fact(total, "default"), Some(&st("0")));
        assert_eq!(fact(total, "comment"), Some(&st("Order total")));
        let orders = sym(f, "table:public.orders");
        assert_eq!(fact(orders, "comment"), Some(&st("Customer orders")));
        assert_eq!(
            fact(orders, "ddl"),
            Some(&Fact::List(vec![
                st("create"),
                st("index"),
                st("comment"),
                st("comment")
            ]))
        );
        assert_eq!(fact(orders, "dialect"), Some(&st("postgresql")));
        assert_eq!(fact(orders, "parsed"), Some(&Fact::Bool(true)));
        assert!(f.artifact.facts.contains_key("statements"));
        assert_eq!(f.artifact.facts["default_schema"], st("public"));
        assert_eq!(f.artifact.facts["default_schema_source"], st("default"));
        // spans: a column span is inside the statement and not the whole table's
        assert!(
            total.span.start_line == 10 && total.span.start_col == 3,
            "{:?}",
            total.span
        );

        // MySQL
        let c = case("tables-mysql");
        let (f, errs) = &c["schema.sql"];
        assert!(errs.is_empty(), "{errs:?}");
        let b = binding_lines(f);
        assert!(
            has(
                &b,
                "table:public.orders provides contract:sql_table:public.orders Extracted"
            ),
            "{b:#?}"
        );
        assert!(has(
            &b,
            "table:public.orders/column:note provides contract:sql_column:public.orders.note Extracted"
        ));
        assert_eq!(
            fact(sym(f, "table:public.orders"), "dialect"),
            Some(&st("mysql"))
        );
        assert_eq!(
            fact(sym(f, "table:public.orders/column:note"), "nullable"),
            Some(&Fact::Bool(true))
        );
        assert_eq!(
            fact(sym(f, "table:public.customers/column:id"), "primary_key"),
            Some(&Fact::Bool(true))
        );

        // SQLite: `main` folds into `public`
        let c = case("tables-sqlite");
        let (f, errs) = &c["schema.sql"];
        assert!(errs.is_empty(), "{errs:?}");
        let b = binding_lines(f);
        assert!(
            has(
                &b,
                "table:public.orders provides contract:sql_table:public.orders Extracted"
            ),
            "{b:#?}"
        );
        assert!(has(
            &b,
            "table:public.docs provides contract:sql_table:public.docs Extracted"
        ));
        assert_eq!(
            fact(sym(f, "table:public.docs"), "virtual"),
            Some(&Fact::Bool(true))
        );
        assert_eq!(
            fact(sym(f, "table:public.orders/column:customer_id"), "type"),
            Some(&st("INTEGER"))
        );
    }

    #[test]
    fn foreign_keys_consume_tables() {
        let c = case("foreign-keys");
        let (f, errs) = &c["schema.sql"];
        assert!(errs.is_empty(), "{errs:?}");
        let b = binding_lines(f);
        assert!(
            has(
                &b,
                "table:public.orders consumes contract:sql_table:public.customers Extracted"
            ),
            "{b:#?}"
        );
        assert!(has(
            &b,
            "table:public.orders consumes contract:sql_table:public.regions Extracted"
        ));
        assert!(has(
            &b,
            "table:public.orders consumes contract:sql_table:audit.customers Extracted"
        ));
        assert!(has(
            &b,
            "table:public.orders provides contract:sql_table:public.orders Extracted"
        ));
        let cid = sym(f, "table:public.orders/column:customer_id");
        // the later ALTER ... FOREIGN KEY wins as the latest value
        assert!(matches!(fact(cid, "references"), Some(Fact::Str(_))));
        let region = sym(f, "table:public.orders/column:region_id");
        assert_eq!(fact(region, "references"), Some(&st("public.regions.id")));
    }

    #[test]
    fn broken_statement_reported() {
        let c = case("broken");
        let (f, errs) = &c["schema.sql"];
        assert_eq!(errs.len(), 1, "{errs:?}");
        match &errs[0] {
            ExtractError::Parse { span: Some(sp), .. } => {
                assert_eq!((sp.start_line, sp.start_col), (3, 1), "{sp:?}");
                assert_eq!(sp.end_line, 3);
            }
            e => panic!("{e:?}"),
        }
        // both tables are kept
        sym(f, "table:public.customers");
        sym(f, "table:public.orders");
    }

    #[test]
    fn deterministic_output() {
        let names: Vec<String> = std::fs::read_dir(fixtures())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.len() >= 10);
        for n in names {
            let files = case_files(&n);
            let a = extract_all(&files);
            let b = extract_all(&files);
            let mut rev = files.clone();
            rev.reverse();
            let c = extract_all(&rev);
            assert_eq!(a, b, "{n}");
            assert_eq!(a, c, "{n}: reversed order");
        }
    }

    #[test]
    fn views_consume_relations() {
        let c = case("views");
        let (f, errs) = &c["views.sql"];
        assert!(errs.is_empty(), "{errs:?}");
        let b = binding_lines(f);
        assert!(
            has(
                &b,
                "view:public.open_orders provides contract:sql_table:public.open_orders Extracted"
            ),
            "{b:#?}"
        );
        assert!(has(
            &b,
            "view:public.open_orders consumes contract:sql_table:public.orders Extracted"
        ));
        assert!(has(
            &b,
            "view:public.open_orders consumes contract:sql_table:public.customers Extracted"
        ));
        // CTE names and system catalogs are excluded
        assert!(
            !b.iter()
                .any(|l| l.contains("recent") || l.contains("information_schema")),
            "{b:#?}"
        );
        assert!(has(
            &b,
            "view:public.order_totals consumes contract:sql_table:public.orders Extracted"
        ));
        assert_eq!(
            fact(sym(f, "view:public.order_totals"), "materialized"),
            Some(&Fact::Bool(true))
        );
    }

    #[test]
    fn routines_per_dialect() {
        let c = case("routines");
        let (f, errs) = &c["pg.sql"];
        assert!(errs.is_empty(), "{errs:?}");
        let b = binding_lines(f);
        assert!(
            has(
                &b,
                "function:app.order_count provides contract:sql_routine:app.order_count Extracted"
            ),
            "{b:#?}"
        );
        assert!(has(
            &b,
            "function:app.order_count consumes contract:sql_table:public.orders Extracted"
        ));
        // the overload is a second symbol
        let second = sym(f, "function:app.order_count~2");
        assert_eq!(fact(second, "language"), Some(&st("sql")));
        assert_eq!(fact(second, "body"), Some(&st("parsed")));
        // plpgsql: scanned, so inferred
        let touch = sym(f, "function:app.touch");
        assert_eq!(fact(touch, "language"), Some(&st("plpgsql")));
        assert_eq!(fact(touch, "body"), Some(&st("scanned")));
        assert_eq!(fact(touch, "returns"), Some(&st("TRIGGER")));
        assert!(
            has(
                &b,
                "function:app.touch consumes contract:sql_table:public.audit Inferred"
            ),
            "{b:#?}"
        );
        assert!(has(
            &b,
            "function:app.touch consumes contract:sql_routine:app.recalc Inferred"
        ));
        assert!(has(
            &b,
            "function:app.touch provides contract:sql_routine:app.touch Extracted"
        ));
        // a PG procedure
        let purge = sym(f, "procedure:public.purge_old");
        assert!(matches!(fact(purge, "parsed"), Some(Fact::Bool(_))));
        assert!(b.iter().any(|l| l.starts_with(
            "procedure:public.purge_old provides contract:sql_routine:public.purge_old"
        )));
        assert!(b.iter().any(|l| {
            l.starts_with("procedure:public.purge_old consumes contract:sql_table:public.orders")
        }));
        // MySQL function
        let (m, errs) = &c["mysql.sql"];
        assert!(errs.is_empty(), "{errs:?}");
        let mb = binding_lines(m);
        assert!(
            mb.iter()
                .any(|l| l
                    .starts_with("function:public.tax provides contract:sql_routine:public.tax")),
            "{mb:#?}"
        );
    }

    #[test]
    fn triggers_bind_table_and_routine() {
        let c = case("triggers");
        let (pg, errs) = &c["pg.sql"];
        assert!(errs.is_empty(), "{errs:?}");
        let b = binding_lines(pg);
        assert!(
            has(
                &b,
                "trigger:public.orders_touch consumes contract:sql_table:public.orders Extracted"
            ),
            "{b:#?}"
        );
        assert!(has(
            &b,
            "trigger:public.orders_touch consumes contract:sql_routine:app.touch Extracted"
        ));
        let t = sym(pg, "trigger:public.orders_touch");
        assert_eq!(fact(t, "timing"), Some(&st("before")));
        assert_eq!(fact(t, "events"), Some(&Fact::List(vec![st("update")])));
        assert_eq!(fact(t, "for_each"), Some(&st("row")));
        assert_eq!(fact(t, "executes"), Some(&st("app.touch")));
        assert_eq!(fact(t, "table"), Some(&st("public.orders")));
        for file in ["mysql.sql", "sqlite.sql"] {
            let (f, errs) = &c[file];
            assert!(errs.is_empty(), "{file}: {errs:?}");
            let b = binding_lines(f);
            assert!(
                b.iter()
                    .any(|l| l.contains(" consumes contract:sql_table:public.orders ")),
                "{file}: {b:#?}"
            );
            assert!(
                b.iter()
                    .any(|l| l.contains(" consumes contract:sql_table:public.audit ")),
                "{file}: {b:#?}"
            );
        }
        let (lite, _) = &c["sqlite.sql"];
        assert_eq!(
            fact(sym(lite, "trigger:public.orders_log"), "timing"),
            Some(&st("after"))
        );
    }

    #[test]
    fn policies_bind_table() {
        let c = case("policies");
        let (f, errs) = &c["policies.sql"];
        assert!(errs.is_empty(), "{errs:?}");
        let b = binding_lines(f);
        assert!(
            has(
                &b,
                "policy:public.orders.own_rows consumes contract:sql_table:public.orders Extracted"
            ),
            "{b:#?}"
        );
        assert!(has(
            &b,
            "policy:public.orders.own_rows consumes contract:sql_table:public.customers Extracted"
        ));
        let p = sym(f, "policy:public.orders.own_rows");
        assert_eq!(fact(p, "command"), Some(&st("select")));
        assert_eq!(fact(p, "roles"), Some(&Fact::List(vec![st("app_user")])));
        assert_eq!(fact(p, "permissive"), Some(&Fact::Bool(true)));
        assert!(matches!(fact(p, "using"), Some(Fact::Str(_))));
        assert_eq!(
            fact(sym(f, "table:public.orders"), "row_level_security"),
            Some(&Fact::Bool(true))
        );
    }

    #[test]
    fn procedural_bodies_scanned() {
        let c = case("procedural-body");
        for (file, (_, errs)) in &c {
            assert!(errs.is_empty(), "{file}: {errs:?}");
        }
        let (pg, _) = &c["pg.sql"];
        let proc = sym(pg, "procedure:public.archive_orders");
        assert_eq!(fact(proc, "parsed"), Some(&Fact::Bool(false)));
        assert!(fact(proc, "dialect").is_none());
        let b = binding_lines(pg);
        assert!(
            has(
                &b,
                "procedure:public.archive_orders provides contract:sql_routine:public.archive_orders Inferred"
            ),
            "{b:#?}"
        );
        for t in ["audit", "orders"] {
            assert!(
                has(
                    &b,
                    &format!(
                        "procedure:public.archive_orders consumes contract:sql_table:public.{t} Inferred"
                    )
                ),
                "{b:#?}"
            );
        }
        assert!(has(
            &b,
            "procedure:public.archive_orders consumes contract:sql_routine:app.vacuum_orders Inferred"
        ));
        // the DO block belongs to the artifact
        assert!(
            has(&b, " consumes contract:sql_table:public.customers Inferred"),
            "{b:#?}"
        );
        let (my, _) = &c["mysql.sql"];
        let mb = binding_lines(my);
        assert!(
            mb.iter()
                .any(|l| l.starts_with("procedure:public.recount provides")
                    && l.ends_with("Inferred")),
            "{mb:#?}"
        );
        assert!(
            mb.iter().any(|l| l
                .starts_with("procedure:public.recount consumes contract:sql_table:public.orders")),
            "{mb:#?}"
        );
        assert!(mb.iter().any(|l| {
            l.starts_with("procedure:public.recount consumes contract:sql_table:public.customers")
        }));
        let (lite, _) = &c["sqlite.sql"];
        let lb = binding_lines(lite);
        assert!(
            lb.iter().any(|l| l.starts_with(
                "trigger:public.customers_ad consumes contract:sql_table:public.customers"
            )),
            "{lb:#?}"
        );
        assert!(
            lb.iter().any(|l| l.starts_with(
                "trigger:public.customers_ad consumes contract:sql_table:public.orders"
            )),
            "{lb:#?}"
        );
        // DELIMITER and block-depth splitting
        let d = case("delimiter");
        let (f, errs) = &d["mysql.sql"];
        assert!(errs.is_empty(), "{errs:?}");
        let db = binding_lines(f);
        assert!(
            db.iter().any(|l| l.starts_with(
                "procedure:public.settle provides contract:sql_routine:public.settle"
            )),
            "{db:#?}"
        );
        assert!(
            db.iter().any(|l| l
                .starts_with("procedure:public.settle consumes contract:sql_table:public.orders")),
            "{db:#?}"
        );
        assert!(
            db.iter().any(|l| l
                .starts_with("procedure:public.settle consumes contract:sql_table:public.audit")),
            "{db:#?}"
        );
        assert!(
            has(
                &db,
                " consumes contract:sql_table:public.customers Extracted"
            ),
            "{db:#?}"
        );
        assert_eq!(f.artifact.facts["statements"], Fact::Int(2));
        // top-level DML
        let q = case("dml");
        let (f, errs) = &q["queries.sql"];
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(f.artifact.facts["dml_statements"], Fact::Int(5));
        let qb = binding_lines(f);
        for t in ["orders", "customers", "audit"] {
            assert!(
                has(
                    &qb,
                    &format!(" consumes contract:sql_table:public.{t} Extracted")
                ),
                "{qb:#?}"
            );
        }
        assert!(
            has(&qb, " consumes contract:sql_routine:app.purge Extracted"),
            "{qb:#?}"
        );
    }

    #[test]
    fn configured_schema_keys_unqualified_names() {
        let c = case("default-schema");
        let (f, errs) = &c["V1__init.sql"];
        assert!(errs.is_empty(), "{errs:?}");
        let b = binding_lines(f);
        assert!(
            has(
                &b,
                "table:app.orders provides contract:sql_table:app.orders Extracted"
            ),
            "{b:#?}"
        );
        assert!(has(
            &b,
            "table:app.orders/column:id provides contract:sql_column:app.orders.id Extracted"
        ));
        assert!(has(
            &b,
            "table:public.audit provides contract:sql_table:public.audit Extracted"
        ));
        assert!(has(
            &b,
            "view:app.order_ids consumes contract:sql_table:app.orders Extracted"
        ));
        assert_eq!(f.artifact.facts["default_schema"], st("app"));
        assert_eq!(f.artifact.facts["default_schema_source"], st("flyway.conf"));
        assert_eq!(f.artifact.facts["dialect"], st("postgresql"));
        match &f.artifact.facts["migration"] {
            Fact::Map(m) => {
                assert_eq!(m["scheme"], st("versioned"));
                assert_eq!(m["version"], st("1"));
                assert_eq!(m["description"], st("init"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn no_config_defaults_to_public() {
        let c = case("alter-across-files");
        assert_eq!(c.len(), 3);
        for (path, (f, errs)) in &c {
            assert!(errs.is_empty(), "{errs:?}");
            assert_eq!(f.artifact.facts["default_schema_source"], st("default"));
            assert_eq!(f.artifact.facts["default_schema"], st("public"));
            for l in binding_lines(f) {
                assert!(
                    l.contains(".public.") || l.contains(":public."),
                    "{path}: {l}"
                );
            }
        }
    }

    #[test]
    fn migrations_facts() {
        let c = case("migrations");
        let m = |p: &str| match &c[p].0.artifact.facts["migration"] {
            Fact::Map(m) => m.clone(),
            o => panic!("{o:?}"),
        };
        assert_eq!(m("0001_init.up.sql")["scheme"], st("up"));
        assert_eq!(m("0001_init.down.sql")["scheme"], st("down"));
        assert_eq!(m("R__refresh.sql")["scheme"], st("repeatable"));
        let b = binding_lines(&c["0001_init.down.sql"].0);
        assert!(
            has(
                &b,
                "table:public.customers declares contract:sql_table:public.customers Extracted"
            ),
            "{b:#?}"
        );
    }

    // ---- engine-level tests ----------------------------------------------------------------

    use katalogr_engine::registry::ExtensionRegistry;
    use katalogr_engine::{Consistency, Engine, EventKind, NewEvent, QueryOptions, Scope};
    use katalogr_model::{ContractName, Segment, SymbolName};
    use katalogr_store::{Kindex, ProjectConfig, ProjectName, Reader, RepoConfig};
    use std::time::{Duration, Instant};

    struct Env {
        _tmp: tempfile::TempDir,
        work: Utf8PathBuf,
        engine: Engine,
    }

    fn env(files: &Files) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let work = root.join("work");
        for (p, b) in files {
            let f = work.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, b).unwrap();
        }
        std::fs::create_dir_all(&work).unwrap();
        let kx = root.join("kx");
        let dir = Kindex::init(&kx)
            .unwrap()
            .create_project(
                &ProjectName::new("p").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let mut reg = ExtensionRegistry::new();
        reg.register(Box::new(SqlExtractor));
        let roots = BTreeMap::from([("r".to_string(), work.clone())]);
        let engine =
            Engine::open(dir, reg, roots, Instant::now() + Duration::from_secs(10)).unwrap();
        Env {
            _tmp: tmp,
            work,
            engine,
        }
    }

    impl Env {
        fn drain<T>(&self, f: impl FnOnce(&katalogr_store::ReadTx) -> T) -> T {
            let a = self
                .engine
                .with_consistency(
                    &QueryOptions {
                        consistency: Consistency::WaitAll,
                        timeout: Duration::from_secs(30),
                        scope: Scope::Project,
                    },
                    f,
                )
                .unwrap();
            assert!(!a.stale);
            a.value
        }

        fn index(&self) {
            self.engine.inbox().append(&NewEvent::rescan("r")).unwrap();
            self.drain(|_| ());
        }

        fn changed(&self, path: &str) {
            self.engine
                .inbox()
                .append(&NewEvent::file(
                    "r",
                    &RelPath::new(path).unwrap(),
                    EventKind::Changed,
                ))
                .unwrap();
            self.drain(|_| ());
        }

        fn fragment(&self, path: &str) -> Fragment {
            self.engine
                .dir()
                .read_shard("r", &RelPath::new(path).unwrap())
                .unwrap()
                .unwrap()
        }
    }

    fn symbol_id(path: &str, segs: &[(&str, &str)]) -> Id {
        SymbolName {
            artifact: ArtifactName::new("r", path).unwrap(),
            segments: segs
                .iter()
                .map(|(k, n)| Segment {
                    kind: (*k).into(),
                    name: (*n).into(),
                })
                .collect(),
            ordinal: 1,
        }
        .id()
    }

    fn contract_id(kind: &str, key: &str) -> Id {
        ContractName {
            kind: kind.into(),
            key: key.into(),
        }
        .id()
    }

    fn incoming(e: &Env, kind: &str, key: &str) -> Vec<(Id, String)> {
        let c = contract_id(kind, key);
        e.drain(|tx| {
            tx.links_in(c)
                .unwrap()
                .into_iter()
                .map(|l| (l.from, l.kind.0))
                .collect()
        })
    }

    #[test]
    fn migration_alters_share_table_contract() {
        let e = env(&case_files("alter-across-files"));
        e.index();
        let v = |n: &str| format!("db/migration/{n}");
        let table = |f: &str| symbol_id(&v(f), &[("table", "public.orders")]);
        let inc = incoming(&e, "sql_table", "public.orders");
        for f in [
            "V1__create_orders.sql",
            "V2__add_total.sql",
            "V3__drop_legacy.sql",
        ] {
            assert!(
                inc.contains(&(table(f), "provides".to_string())),
                "{f}: {inc:?}"
            );
        }
        assert_eq!(inc.len(), 3, "{inc:?}");
        let total = incoming(&e, "sql_column", "public.orders.total");
        assert_eq!(
            total,
            vec![(
                symbol_id(
                    &v("V2__add_total.sql"),
                    &[("table", "public.orders"), ("column", "total")]
                ),
                "provides".to_string()
            )]
        );
        let legacy = incoming(&e, "sql_column", "public.orders.legacy");
        assert!(legacy.contains(&(
            symbol_id(
                &v("V3__drop_legacy.sql"),
                &[("table", "public.orders"), ("column", "legacy")]
            ),
            "declares".to_string()
        )));
        assert!(legacy.contains(&(
            symbol_id(
                &v("V1__create_orders.sql"),
                &[("table", "public.orders"), ("column", "legacy")]
            ),
            "provides".to_string()
        )));
    }

    #[test]
    fn dialect_context_change_reextracts() {
        let e = env(&case_files("dialect-context"));
        e.index();
        let path = "db/migration/V1__init.sql";
        assert_eq!(e.fragment(path).artifact.facts["dialect"], st("mysql"));
        assert_eq!(
            e.fragment(path).artifact.facts["dialect_source"],
            st("db/flyway.conf")
        );
        // the sibling directory has no configured dialect
        assert_eq!(
            e.fragment("other/V1__init.sql").artifact.facts["dialect"],
            st("auto")
        );
        std::fs::write(
            e.work.join("db/flyway.conf"),
            "flyway.url=jdbc:postgresql://localhost/shop\nflyway.locations=filesystem:migration\n",
        )
        .unwrap();
        e.changed("db/flyway.conf");
        assert_eq!(e.fragment(path).artifact.facts["dialect"], st("postgresql"));
        // the PostgreSQL parser rejects the backticks: the generic fallback is tried, then the scanner
        assert_eq!(
            e.fragment("other/V1__init.sql").artifact.facts["dialect"],
            st("auto")
        );
    }

    fn embedded_tables(text: &str, holes: &[u32], ctx: &[u8]) -> Vec<String> {
        let ext = SqlExtractor;
        assert!(ext.accepts_embedded(text), "accepts {text:?}");
        let path = RelPath::new("src/a.rs").unwrap();
        let span = katalogr_model::Span {
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 5,
        };
        let input = katalogr_model::EmbeddedInput {
            repo: "r",
            host_path: &path,
            host_extension: "rs",
            owner: Id::of("owner"),
            span,
            text,
            holes,
            call: None,
            context: ctx,
        };
        let out = ext.extract_embedded(&input);
        for b in &out {
            assert_eq!(b.symbol, Id::of("owner"));
            assert_eq!(b.role, katalogr_model::ContractRole::Consumes);
            assert_eq!(b.confidence, katalogr_model::Confidence::Inferred);
            assert_eq!(b.span, Some(span));
        }
        out.iter().map(|b| b.contract.canonical()).collect()
    }

    #[test]
    fn embedded_query_refs() {
        let t = |text: &str| embedded_tables(text, &[], &[]);
        assert_eq!(
            t("SELECT o.id FROM orders o JOIN app.customers c ON c.id = o.customer_id"),
            vec![
                "contract:sql_table:app.customers",
                "contract:sql_table:public.orders"
            ]
        );
        assert_eq!(
            t("WITH x AS (SELECT * FROM a) INSERT INTO b SELECT * FROM x"),
            vec!["contract:sql_table:public.a", "contract:sql_table:public.b"]
        );
        assert!(t("SELECT * FROM {table} WHERE id = {}").is_empty());
        assert_eq!(
            t("SELECT * FROM orders WHERE id = {id}"),
            vec!["contract:sql_table:public.orders"]
        );
        assert_eq!(
            t("CALL app.purge()"),
            vec!["contract:sql_routine:app.purge"]
        );
        // holes inside `WHERE id = ` parse
        let text = "SELECT * FROM orders WHERE id = ";
        assert_eq!(
            embedded_tables(text, &[text.len() as u32], &[]),
            vec!["contract:sql_table:public.orders"]
        );
        // a table named by a hole gives nothing for that part
        let text = "SELECT * FROM  WHERE id = 1";
        assert!(embedded_tables(&format!("{text} /* SELECT FROM */"), &[14], &[]).is_empty());
        // positional parameters
        assert_eq!(
            t("UPDATE orders SET total = $1 WHERE id = $2"),
            vec!["contract:sql_table:public.orders"]
        );
        assert_eq!(
            t("INSERT INTO audit (note) VALUES (?)"),
            vec!["contract:sql_table:public.audit"]
        );
        // the configured schema (R-20, R-23)
        assert_eq!(
            t("SELECT * FROM orders"),
            vec!["contract:sql_table:public.orders"]
        );
        assert_eq!(
            embedded_tables("SELECT * FROM orders", &[], b"app"),
            vec!["contract:sql_table:app.orders"]
        );
        assert_eq!(
            embedded_tables("SELECT * FROM public.orders", &[], b"app"),
            vec!["contract:sql_table:public.orders"]
        );
        // the embedded context is the schema only
        let cfg = files(&[("flyway.conf", "flyway.defaultSchema=app\n")]);
        let bytes = SqlExtractor.build_context(&cfg).unwrap();
        assert_eq!(SqlExtractor.embedded_context(&bytes), b"app");
        let dialect_only = files(&[("flyway.conf", "flyway.url=jdbc:mysql://h/db\n")]);
        let bytes = SqlExtractor.build_context(&dialect_only).unwrap();
        assert!(SqlExtractor.embedded_context(&bytes).is_empty());
        assert!(SqlExtractor.embedded_context(&[]).is_empty());
        assert_eq!(SqlExtractor.embedded_version(), Some("1"));
    }

    #[test]
    fn non_sql_literals_ignored() {
        let ext = SqlExtractor;
        for prose in [
            "select an option from the list",
            "Please select a file",
            "update your settings",
            "delete from cart",
            "Select one (or more) from the list.",
            "/orders",
            "",
            "   ",
            "hello world",
        ] {
            assert!(!ext.accepts_embedded(prose), "{prose:?}");
        }
        for q in [
            "select * from cart",
            "SELECT id FROM orders",
            "  -- c\n (SELECT 1 FROM t)",
        ] {
            assert!(ext.accepts_embedded(q), "{q:?}");
        }
        assert_eq!(
            embedded_tables("select * from cart", &[], &[]),
            vec!["contract:sql_table:public.cart"]
        );
        // an accepted text with no relation gives no binding
        assert!(embedded_tables("SELECT 1 + 1 FROM dual", &[], &[]).is_empty());
    }
}
