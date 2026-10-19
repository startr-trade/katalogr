//! FEAT-017 SQL extension tests.

mod sql {
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
}
