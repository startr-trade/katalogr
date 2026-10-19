//! FEAT-009 data-document tests. Fixture cases are indexed through the engine with a profile
//! that routes their files to the data extension.

use camino::{Utf8Path, Utf8PathBuf};
use katalogr_acceptance::{Case, run_case};
use katalogr_engine::registry::ExtensionRegistry;
use katalogr_ext_data::{DataExtractor, Format, parse};
use katalogr_model::{ExtractInput, Extractor, Fact, Name, RelPath};
use serde_json::Value;

fn registry() -> ExtensionRegistry {
    let mut r = ExtensionRegistry::new();
    r.register(Box::new(DataExtractor));
    r
}

fn case(name: &str) -> Utf8PathBuf {
    Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/data")
        .join(name)
}

/// The facts of every symbol of the canonical graph, by canonical name.
fn symbols(graph: &str) -> std::collections::BTreeMap<String, Value> {
    graph
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| v["node"] == "symbol")
        .map(|v| (v["name"].as_str().unwrap().to_string(), v))
        .collect()
}

fn fact(v: &Value, k: &str) -> String {
    v["facts"][k]["Str"]
        .as_str()
        .unwrap_or("<none>")
        .to_string()
}

/// Write `files` under a fresh case directory with a profile routing every file to `data`.
fn generated(files: &[(&str, Vec<u8>)]) -> (tempfile::TempDir, Utf8PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    std::fs::create_dir_all(dir.join(".katalogr-profiles")).unwrap();
    std::fs::write(
        dir.join(".katalogr-profiles/select.yaml"),
        "profile: all\nversion: 2\nselect:\n  - { extension: data, path: '*.json' }\n  - { extension: data, path: '*.yaml' }\n  - { extension: data, path: '*.xml' }\n",
    )
    .unwrap();
    for (name, bytes) in files {
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    (tmp, dir)
}

/// The index-error rows of each path.
fn errors_of(
    tx: &katalogr_store::ReadTx,
    files: &[&str],
) -> std::collections::BTreeMap<String, Vec<katalogr_store::ErrorRow>> {
    use katalogr_store::Reader;
    files
        .iter()
        .map(|f| {
            let rows = tx.index_errors("fx", &RelPath::new(f).unwrap()).unwrap();
            ((*f).to_string(), rows)
        })
        .collect()
}

fn extract(
    path: &str,
    text: &str,
) -> (katalogr_model::Fragment, Vec<katalogr_model::ExtractError>) {
    let p = RelPath::new(path).unwrap();
    DataExtractor
        .extract_with_errors(&ExtractInput {
            repo: "fx",
            path: &p,
            revision: "rev",
            content: text.as_bytes(),
            context: &[],
        })
        .unwrap()
}

mod data {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn key_tree_json_yaml_xml() {
        let dir = case("key-tree");
        let report = run_case(&registry, &dir).unwrap();
        assert!(report.passed(), "{}", report.render());

        let c = Case::open(&registry, &dir).unwrap();
        let s = symbols(&c.graph().unwrap());
        let at = |n: &str| &s[&format!("fx:docs/{n}")];

        // JSON: pointers (escaped), value types and values.
        assert_eq!(fact(at("orders.json#doc:orders"), "value_type"), "map");
        let count = at("orders.json#doc:orders/key:count");
        assert_eq!(
            (
                fact(count, "pointer"),
                fact(count, "value_type"),
                fact(count, "value")
            ),
            ("/count".into(), "num".into(), "2".into())
        );
        let slash = at("orders.json#doc:orders/key:nested/key:a%2Fb");
        assert_eq!(fact(slash, "pointer"), "/nested/a~1b");
        assert_eq!(
            fact(at("orders.json#doc:orders/key:tags/item:1"), "pointer"),
            "/tags/1"
        );
        assert_eq!(
            fact(at("orders.json#doc:orders/key:nested/key:x"), "value_type"),
            "null"
        );

        // YAML: two documents, the second one with an ordinal; an alias stays unexpanded.
        assert!(s.contains_key("fx:docs/shapes.yaml#doc:shapes"));
        assert!(s.contains_key("fx:docs/shapes.yaml#doc:shapes~2"));
        let alias = at("shapes.yaml#doc:shapes/key:copy/key:<<");
        assert_eq!(
            (fact(alias, "value_type"), fact(alias, "value")),
            ("alias".into(), "*base".into())
        );
        assert_eq!(
            fact(
                at("shapes.yaml#doc:shapes/key:items/item:0/key:id"),
                "value"
            ),
            "1"
        );

        // XML: local names, `@attr`, and a repeated local name gets an ordinal and `[2]`.
        assert_eq!(
            fact(at("order.xml#doc:order/key:order/key:@id"), "pointer"),
            "/order/@id"
        );
        assert_eq!(
            fact(at("order.xml#doc:order/key:order/key:line"), "pointer"),
            "/order/line"
        );
        assert_eq!(
            fact(at("order.xml#doc:order/key:order/key:line~2"), "pointer"),
            "/order/line[2]"
        );
        assert_eq!(
            fact(
                at("order.xml#doc:order/key:order/key:line/key:%23text"),
                "value"
            ),
            "first"
        );
        assert_eq!(
            fact(
                at("order.xml#doc:order/key:order/key:line/key:@sku~2"),
                "value"
            ),
            "s2"
        );
        // The routed files carry their format.
        let g = c.graph().unwrap();
        assert!(
            g.contains("\"data_format\":{\"Str\":\"xml\"}")
                && g.contains("\"data_format\":{\"Str\":\"yaml\"}")
        );
    }

    #[test]
    fn aliases_not_expanded() {
        // An exponential alias chain stays linear: aliases are names, not copies.
        let mut text = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x]\n");
        for i in 1..30 {
            let prev = format!("*a{}", i - 1);
            text.push_str(&format!(
                "a{i}: &a{i} [{p}, {p}, {p}, {p}, {p}, {p}, {p}, {p}, {p}]\n",
                p = prev
            ));
        }
        let t = std::time::Instant::now();
        let parsed = parse(Format::Yaml, &text);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert!(t.elapsed().as_millis() < 500);
        let (f, errors) = extract("big.yaml", &text);
        assert!(errors.is_empty());
        let item = f
            .symbols
            .iter()
            .find(|s| s.name.canonical() == "fx:big.yaml#doc:big/key:a29/item:0")
            .unwrap();
        assert_eq!(item.facts.get("value"), Some(&Fact::Str("*a28".into())));
        assert_eq!(
            item.facts.get("value_type"),
            Some(&Fact::Str("alias".into()))
        );
        assert!(f.symbols.len() < 400, "{}", f.symbols.len());
    }

    #[test]
    fn malformed_reports_span() {
        let report = run_case(&registry, &case("broken")).unwrap();
        assert!(report.passed(), "{}", report.render());
        const FILES: &[&str] = &["docs/bad.json", "docs/bad.yaml", "docs/bad.xml"];
        let c = Case::open(&registry, &case("broken")).unwrap();
        let (errors, graph) = c
            .read(|tx| (errors_of(tx, FILES), katalogr_engine::canonical_graph(tx)))
            .unwrap();
        for (file, line) in [
            ("docs/bad.json", 3),
            ("docs/bad.yaml", 3),
            ("docs/bad.xml", 3),
        ] {
            let rows = &errors[file];
            assert_eq!(rows.len(), 1, "{file}: {rows:?}");
            let span = rows[0].span.unwrap_or_else(|| panic!("{file}: no span"));
            assert_eq!(span.start_line, line, "{file}: {rows:?}");
            assert!(rows[0].message.starts_with("parse error:"), "{rows:?}");
        }
        // What was read before the error is kept.
        assert!(graph.contains("fx:docs/bad.json#doc:bad/key:a"), "{graph}");
        assert!(
            graph.contains("fx:docs/bad.json#doc:bad/key:b/item:0"),
            "{graph}"
        );
        assert!(graph.contains("fx:docs/bad.yaml#doc:bad/key:a"), "{graph}");
        assert!(
            graph.contains("fx:docs/bad.xml#doc:bad/key:a/key:b"),
            "{graph}"
        );
    }

    #[test]
    fn depth_and_size_caps() {
        let deep = format!("{}1{}", "[".repeat(65), "]".repeat(65));
        let ok_depth = format!("{}1{}", "[".repeat(64), "]".repeat(64));
        let mut many = String::from("{");
        for i in 0..20_001 {
            many.push_str(&format!("{}\"k{i}\":{i}", if i == 0 { "" } else { "," }));
        }
        many.push('}');
        let mut exact = String::from("{");
        for i in 0..20_000 {
            exact.push_str(&format!("{}\"k{i}\":{i}", if i == 0 { "" } else { "," }));
        }
        exact.push('}');
        let mut huge = vec![b' '; 8 * 1024 * 1024];
        huge.push(b'1');
        let (_tmp, dir) = generated(&[
            ("deep.json", deep.into_bytes()),
            ("ok_depth.json", ok_depth.into_bytes()),
            ("many.json", many.into_bytes()),
            ("huge.json", huge),
        ]);
        const FILES: &[&str] = &["deep.json", "ok_depth.json", "many.json", "huge.json"];
        let c = Case::open(&registry, &dir).unwrap();
        let (errors, graph) = c
            .read(|tx| (errors_of(tx, FILES), katalogr_engine::canonical_graph(tx)))
            .unwrap();
        let msg = |f: &str| {
            errors
                .get(f)
                .map(|r| r[0].message.clone())
                .unwrap_or_default()
        };
        assert!(
            msg("deep.json").contains("nesting deeper than 64"),
            "{:?}",
            errors["deep.json"]
        );
        assert!(errors.get("ok_depth.json").is_none_or(|r| r.is_empty()));
        assert!(
            msg("many.json").contains("more than 20000 keys"),
            "{}",
            msg("many.json")
        );
        // Exactly the cap is not truncated (checked without the engine).
        let (f, errs) = extract("exact.json", &exact);
        assert!(errs.is_empty(), "{errs:?}");
        assert!(!f.artifact.facts.contains_key("truncated"));
        assert_eq!(f.symbols.len(), 20_001, "one doc and 20,000 keys");
        // The file over the size bound is refused: no symbols, an error row.
        assert!(
            msg("huge.json").contains("larger than 8 MiB"),
            "{:?}",
            errors.get("huge.json")
        );
        assert!(
            !graph.contains("fx:huge.json#"),
            "no symbols from the refused file"
        );
        // Truncation is flagged on the artifact; the first keys are kept.
        let many_line = graph
            .lines()
            .find(|l| l.contains("\"name\":\"fx:many.json\""))
            .unwrap();
        assert!(
            many_line.contains("\"truncated\":{\"Bool\":true}"),
            "{many_line}"
        );
        assert!(graph.contains("fx:many.json#doc:many/key:k0\""));
        assert!(!graph.contains("fx:many.json#doc:many/key:k20000\""));
    }

    fn arb_text() -> impl Strategy<Value = String> {
        prop_oneof![
            any::<String>(),
            "[{}\\[\\]:,\"'<>/&#\\-!?*@ \\n\\t\\\\a-z0-9=|]{0,200}",
            "(\\[|\\{|<a>){0,90}[a-z0-9]{0,4}(\\]|\\}|</a>){0,90}",
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn parsers_never_panic(text in arb_text()) {
            for f in [Format::Json, Format::Yaml, Format::Xml] {
                let p = parse(f, &text);
                // Whatever the input, the answer has a tree or an error.
                prop_assert!(!p.docs.is_empty() || !p.errors.is_empty() || text.trim().is_empty() || f != Format::Json);
            }
            for path in ["x.json", "x.yaml", "x.xml"] {
                let p = RelPath::new(path).unwrap();
                let _ = DataExtractor.extract_with_errors(&ExtractInput {
                    repo: "fx", path: &p, revision: "r", content: text.as_bytes(), context: &[],
                });
            }
        }
    }
}
