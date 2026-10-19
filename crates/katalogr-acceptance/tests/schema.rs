//! FEAT-009 across extensions: the codec chain (a project profile over data documents, the
//! schema extensions and a channel) and mirrored-schema drift.

use camino::{Utf8Path, Utf8PathBuf};
use katalogr_acceptance::{Case, full_registry, run_case};
use katalogr_model::{ContractName, Name, Span};
use katalogr_query::{FieldDiff, MirrorDrift, SymbolHit, dangling, mirror_drift, path};
use katalogr_store::Reader;

fn fixtures() -> Utf8PathBuf {
    Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn hit(name: &str) -> SymbolHit {
    SymbolHit {
        name: name.to_string(),
        kind: String::new(),
        path: String::new(),
        span: Span {
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 1,
        },
    }
}

/// Write `files` under `root/<dir>`.
fn write(root: &Utf8Path, files: &[(&str, &str)]) {
    for (name, text) in files {
        let p = root.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
}

mod codec {
    use super::*;

    const CODEC: &str = "fx:codecs/orders.yaml#doc:orders";
    const ORDER: &str = "fx:schemas/orders.xsd#type:Order";

    fn open() -> Case {
        let dir = fixtures().join("data/codec-chain");
        let report = run_case(&full_registry, &dir).unwrap();
        assert!(report.passed(), "{}", report.render());
        Case::open(&full_registry, &dir).unwrap()
    }

    /// Resolved links as `(from, kind, to)`.
    fn links(c: &Case) -> Vec<(String, String, String)> {
        c.graph()
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .filter(|v| v["link"] == true)
            .map(|v| {
                let s = |k: &str| v[k].as_str().unwrap().to_string();
                (s("from"), s("kind"), s("to"))
            })
            .collect()
    }

    fn has(c: &Case, from: &str, kind: &str, to: &str) -> bool {
        links(c).contains(&(from.to_string(), kind.to_string(), to.to_string()))
    }

    #[test]
    fn manifest_binds_codec_and_type() {
        let c = open();
        // The manifest provides the codec contract; the channel consumes it.
        let id = ContractName {
            kind: "codec".into(),
            key: "urn:example:orders".into(),
        }
        .id();
        let row = c
            .read(|tx| tx.contract(id).unwrap())
            .unwrap()
            .expect("the codec contract");
        assert_eq!((row.providers, row.consumers), (1, 1));
        // The manifest's schema file and structural type are linked.
        assert!(has(
            &c,
            &format!("{CODEC}/key:schema/key:file"),
            "uses-schema",
            "fx:schemas/orders.xsd"
        ));
        assert!(has(
            &c,
            &format!("{CODEC}/key:schema/key:type"),
            "uses-type",
            ORDER
        ));
        // The rules' facts sit on the document symbol.
        let g = c.graph().unwrap();
        let doc = g
            .lines()
            .find(|l| l.contains(&format!("\"name\":\"{CODEC}\"")))
            .unwrap();
        assert!(doc.contains("\"schema_kind\":{\"Str\":\"xsd\"}"), "{doc}");
        // Everything the rules emitted is declared, with the rule as its origin.
        let origins = c
            .read(|tx| {
                tx.links_all()
                    .unwrap()
                    .into_iter()
                    .flat_map(|(_, r)| r.entries)
                    .filter_map(|e| e.produced_by)
                    .collect::<std::collections::BTreeSet<_>>()
            })
            .unwrap();
        for rule in [
            "codec-provides",
            "codec-schema-file",
            "codec-type",
            "codec-field-mapping",
        ] {
            assert!(
                origins.contains(&format!("profile:example-codecs#{rule}")),
                "{rule}: {origins:?}"
            );
        }
    }

    #[test]
    fn field_mappings_link_members() {
        let c = open();
        let f = |n: &str| format!("{ORDER}/{n}");
        for (fmt, wire, member) in [
            ("json", "orderId", f("field:id")),
            ("json", "amount", f("field:total")),
            ("json", "sku", f("field:lines/field:sku")),
            ("csv", "col_a", f("field:id")),
            ("csv", "col_b", f("field:total")),
        ] {
            let key = format!("{CODEC}/key:formats/key:{fmt}/key:fields/key:{wire}");
            assert!(
                has(&c, &key, "maps-to", &member),
                "{fmt}.{wire} -> {member}"
            );
        }
        // The wire format and field are facts of the mapping symbol.
        let g = c.graph().unwrap();
        let line = g
            .lines()
            .find(|l| {
                l.contains("key:formats/key:json/key:fields/key:orderId\",\"node\":\"symbol\"")
            })
            .unwrap_or_else(|| panic!("{g}"));
        assert!(
            line.contains("\"wire_format\":{\"Str\":\"json\"}"),
            "{line}"
        );
        assert!(
            line.contains("\"wire_field\":{\"Str\":\"orderId\"}"),
            "{line}"
        );
    }

    #[test]
    fn chain_traversable_from_channel() {
        let c = open();
        let channel = hit("fx:channels/orders.tx#channel:orders");
        for field in ["field:id", "field:total", "field:lines/field:sku"] {
            let steps = c
                .read(|tx| path(tx, &channel, &hit(&format!("{ORDER}/{field}")), 8).unwrap())
                .unwrap()
                .unwrap_or_else(|| panic!("no path to {field}"));
            let kinds: Vec<&str> = steps.iter().map(|s| s.kind.as_str()).collect();
            assert!(steps.len() <= 8, "{kinds:?}");
            // channel -> codec contract -> manifest -> type -> field.
            assert_eq!(kinds[0], "consumes", "{kinds:?}");
            assert_eq!(kinds[1], "provides", "{kinds:?}");
            assert!(
                kinds.contains(&"uses-type") || kinds.contains(&"maps-to"),
                "{kinds:?}"
            );
            assert_eq!(steps[0].from, "fx:channels/orders.tx#channel:orders");
            assert!(steps.last().unwrap().to.ends_with(field), "{steps:?}");
        }
        // The plain chain to the type is the one in the design: five links.
        let steps = c
            .read(|tx| path(tx, &channel, &hit(ORDER), 8).unwrap())
            .unwrap()
            .unwrap();
        let kinds: Vec<&str> = steps.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(
            kinds,
            ["consumes", "provides", "contains", "contains", "uses-type"]
        );
    }

    #[test]
    fn unmapped_member_reported_dangling() {
        let c = open();
        let d = c.read(|tx| dangling(tx, &["maps-to"]).unwrap()).unwrap();
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(
            d[0].from.name,
            format!("{CODEC}/key:formats/key:json/key:fields/key:ghost")
        );
        assert_eq!(
            d[0].wanted,
            ["schema-member:{urn:example:orders}Order/missing"]
        );
        // The type and file references of the manifest resolve.
        let d = c
            .read(|tx| dangling(tx, &["uses-type", "uses-schema"]).unwrap())
            .unwrap();
        assert!(d.is_empty(), "{d:?}");
    }
}

mod mirror {
    use super::*;

    const ORDER_XSD: &str = "<?xml version=\"1.0\"?>\n<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" targetNamespace=\"urn:example:orders\">\n  <xs:complexType name=\"Order\">\n    <xs:sequence>\n      <xs:element name=\"id\" type=\"xs:string\"/>\n      <xs:element name=\"total\" type=\"xs:decimal\"/>\n      <xs:element name=\"tags\" type=\"xs:string\" minOccurs=\"0\" maxOccurs=\"unbounded\"/>\n    </xs:sequence>\n  </xs:complexType>\n</xs:schema>\n";

    fn drift(c: &Case, kinds: &[&str]) -> Vec<MirrorDrift> {
        c.read(|tx| mirror_drift(tx, kinds).unwrap()).unwrap()
    }

    fn summary(d: &MirrorDrift) -> Vec<(String, String)> {
        d.differences
            .iter()
            .map(|f: &FieldDiff| (f.what.clone(), f.field.clone()))
            .collect()
    }

    #[test]
    fn copies_across_repos_compared() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let changed = ORDER_XSD.replace(" maxOccurs=\"unbounded\"", "").replace(
            "<xs:element name=\"total\"",
            "<xs:element name=\"note\" type=\"xs:string\"/>\n      <xs:element name=\"total\"",
        );
        write(
            root,
            &[("alpha/order.xsd", ORDER_XSD), ("beta/order.xsd", &changed)],
        );
        let c = Case::open_repos(
            &full_registry,
            &[("alpha", &root.join("alpha")), ("beta", &root.join("beta"))],
        )
        .unwrap();

        // Two repositories declare `{urn:example:orders}Order`: an extra field and a changed
        // cardinality are reported, the rest is equal.
        let d = drift(&c, &[]);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(
            d[0].contract,
            "contract:schema_type:{urn%3Aexample%3Aorders}Order"
        );
        assert_eq!(d[0].left.name, "alpha:order.xsd#type:Order");
        assert_eq!(d[0].right.name, "beta:order.xsd#type:Order");
        assert!(d[0].comparable);
        assert_eq!(
            summary(&d[0]),
            [
                ("missing-left".to_string(), "note".to_string()),
                ("many".to_string(), "tags".to_string()),
            ]
        );
        let many = &d[0].differences[1];
        assert_eq!(many.left.as_ref().unwrap().many, Some(true));
        assert_eq!(many.right.as_ref().unwrap().many, Some(false));
        // A kind filter.
        assert_eq!(drift(&c, &["schema_type"]).len(), 1);
        assert!(drift(&c, &["schema_element"]).is_empty());

        // Identical copies are omitted.
        let tmp2 = tempfile::tempdir().unwrap();
        let root2 = Utf8Path::from_path(tmp2.path()).unwrap();
        write(
            root2,
            &[
                ("alpha/order.xsd", ORDER_XSD),
                ("beta/order.xsd", ORDER_XSD),
            ],
        );
        let c = Case::open_repos(
            &full_registry,
            &[
                ("alpha", &root2.join("alpha")),
                ("beta", &root2.join("beta")),
            ],
        )
        .unwrap();
        assert!(drift(&c, &[]).is_empty());
    }

    #[test]
    fn cross_format_mirror_via_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        write(
            root,
            &[
                (
                    "order.xsd",
                    "<?xml version=\"1.0\"?>\n<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:o=\"urn:example:orders\" targetNamespace=\"urn:example:orders\">\n  <xs:simpleType name=\"Status\"><xs:restriction base=\"xs:string\"/></xs:simpleType>\n  <xs:complexType name=\"Order\">\n    <xs:sequence>\n      <xs:element name=\"orderId\" type=\"xs:string\"/>\n      <xs:element name=\"Total\" type=\"xs:decimal\"/>\n      <xs:element name=\"status\" type=\"o:Status\"/>\n      <xs:element name=\"lines\" type=\"xs:string\" maxOccurs=\"unbounded\"/>\n    </xs:sequence>\n  </xs:complexType>\n</xs:schema>\n",
                ),
                (
                    "order.schema.json",
                    "{\n  \"title\": \"Order\",\n  \"type\": \"object\",\n  \"required\": [\"order_id\", \"total\", \"status\"],\n  \"properties\": {\n    \"order_id\": { \"type\": \"string\" },\n    \"total\": { \"type\": \"number\" },\n    \"status\": { \"type\": \"string\" }\n  }\n}\n",
                ),
                (
                    ".katalogr-profiles/mirror.yaml",
                    "profile: mirror-orders\nversion: 2\nrules:\n  - id: xsd-order\n    when: { extension: xsd, kind: type, name: '^Order$' }\n    emit: { bind: { contract: { kind: schema_mirror, key: orders }, role: declares } }\n  - id: json-order\n    when: { extension: json-schema, kind: type, name: '^Order$' }\n    emit: { bind: { contract: { kind: schema_mirror, key: orders }, role: declares } }\n",
                ),
            ],
        );
        let c = Case::open(&full_registry, root).unwrap();
        // A profile binds a type of each format to one contract; the names are compared with
        // case, `_`, `-` and a leading `@` ignored, and the primitive words are shared.
        let d = drift(&c, &["schema_mirror"]);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].contract, "contract:schema_mirror:orders");
        assert_eq!(d[0].left.name, "fx:order.schema.json#type:Order");
        assert_eq!(d[0].right.name, "fx:order.xsd#type:Order");
        assert_eq!(
            summary(&d[0]),
            [
                ("missing-left".to_string(), "lines".to_string()),
                ("type".to_string(), "status".to_string()),
            ]
        );
        // `orderId`/`order_id` and `Total`/`total` match, with equal shapes.
        let status = &d[0].differences[1];
        assert_eq!(
            status.left.as_ref().unwrap().field_type.as_deref(),
            Some("string")
        );
        assert_eq!(
            status.right.as_ref().unwrap().field_type.as_deref(),
            Some("status")
        );
        // The two types also declare their own `schema_type`: different namespaces, so no
        // mirror set there.
        assert!(drift(&c, &["schema_type"]).is_empty());
    }
}
