//! FEAT-009 schema tests. Fixture cases are indexed through the engine with only the schema
//! (and, for routed files, no other) extractors registered.

use camino::{Utf8Path, Utf8PathBuf};
use katalogr_acceptance::{Case, run_case};
use katalogr_engine::registry::ExtensionRegistry;
use katalogr_ext_schema::{JsonSchemaExtractor, XsdExtractor};
use katalogr_model::{ContractName, Name};
use katalogr_store::Reader;
use serde_json::Value;
use std::collections::BTreeMap;

fn registry() -> ExtensionRegistry {
    let mut r = ExtensionRegistry::new();
    r.register(Box::new(XsdExtractor));
    r.register(Box::new(JsonSchemaExtractor));
    r
}

fn case_dir(kind: &str, name: &str) -> Utf8PathBuf {
    Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(kind)
        .join(name)
}

/// A fixture case compared with its `expected.yaml`, then opened for inspection.
fn open(kind: &str, name: &str) -> Indexed {
    let dir = case_dir(kind, name);
    let report = run_case(&registry, &dir).unwrap();
    assert!(report.passed(), "{}", report.render());
    Indexed::open(&dir)
}

struct Indexed {
    case: Case,
    graph: String,
    nodes: BTreeMap<String, Value>,
}

impl Indexed {
    fn open(dir: &Utf8Path) -> Indexed {
        let case = Case::open(&registry, dir).unwrap();
        let graph = case.graph().unwrap();
        let nodes = graph
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .filter(|v| v["node"] == "symbol" || v["node"] == "artifact")
            .map(|v| (v["name"].as_str().unwrap().to_string(), v))
            .collect();
        Indexed { case, graph, nodes }
    }

    fn node(&self, name: &str) -> &Value {
        self.nodes.get(name).unwrap_or_else(|| {
            panic!(
                "no symbol {name}; have:\n{}",
                self.nodes.keys().cloned().collect::<Vec<_>>().join("\n")
            )
        })
    }

    fn has(&self, name: &str) -> bool {
        self.nodes.contains_key(name)
    }

    fn str_fact(&self, name: &str, key: &str) -> String {
        self.node(name)["facts"][key]["Str"]
            .as_str()
            .unwrap_or("<none>")
            .to_string()
    }

    fn bool_fact(&self, name: &str, key: &str) -> Option<bool> {
        self.node(name)["facts"][key]["Bool"].as_bool()
    }

    fn list_fact(&self, name: &str, key: &str) -> Vec<String> {
        self.node(name)["facts"][key]["List"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|x| x["Str"].as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `(providers, consumers, declarers)` of a contract node.
    fn contract(&self, kind: &str, key: &str) -> Option<(u32, u32, u32)> {
        let id = ContractName {
            kind: kind.into(),
            key: key.into(),
        }
        .id();
        self.case
            .read(|tx| {
                tx.contract(id)
                    .unwrap()
                    .map(|c| (c.providers, c.consumers, c.declarers))
            })
            .unwrap()
    }

    /// The names that declare a contract.
    fn declarers(&self, kind: &str, key: &str) -> Vec<String> {
        let id = ContractName {
            kind: kind.into(),
            key: key.into(),
        }
        .id();
        self.case
            .read(|tx| {
                let mut v: Vec<String> = tx
                    .links_in(id)
                    .unwrap()
                    .into_iter()
                    .filter(|k| k.kind.0 == "declares")
                    .filter_map(|k| tx.symbol(k.from).unwrap().map(|s| s.name.canonical()))
                    .collect();
                v.sort();
                v
            })
            .unwrap()
    }

    /// Resolved links of one kind as `(from, to)` pairs, by canonical name.
    fn links(&self, kind: &str) -> Vec<(String, String)> {
        self.graph
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .filter(|v| v["link"] == true && v["kind"] == kind)
            .map(|v| {
                (
                    v["from"].as_str().unwrap().to_string(),
                    v["to"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    /// Unresolved references of one kind: `(from, wanted keys)`.
    fn unresolved(&self, kind: &str) -> Vec<(String, Vec<String>)> {
        self.case
            .read(|tx| {
                let mut v: Vec<(String, Vec<String>)> = tx
                    .unresolved_refs(None)
                    .unwrap()
                    .into_iter()
                    .filter(|(_, r)| r.kind.0 == kind)
                    .map(|(_, r)| {
                        let from = tx
                            .symbol(r.from)
                            .unwrap()
                            .map(|s| s.name.canonical())
                            .unwrap_or_default();
                        (
                            from,
                            r.candidates
                                .iter()
                                .map(|c| format!("{}:{}", c.namespace, c.key))
                                .collect(),
                        )
                    })
                    .collect();
                v.sort();
                v
            })
            .unwrap()
    }

    /// The index-error rows of a path.
    fn errors(&self, path: &str) -> Vec<katalogr_store::ErrorRow> {
        self.case
            .read(|tx| {
                tx.index_errors("fx", &katalogr_model::RelPath::new(path).unwrap())
                    .unwrap()
            })
            .unwrap()
    }
}

mod xsd {
    use super::*;

    const O: &str = "fx:schemas/order.xsd";

    #[test]
    fn types_elements_and_fields() {
        let ix = open("xsd", "types-fields");
        // Named definitions.
        for n in [
            "schema:order",
            "type:Order",
            "type:RushOrder",
            "type:Status",
            "type:Code",
            "element:order",
            "element:status",
            "group:OrderIds",
            "group:Audit",
        ] {
            assert!(ix.has(&format!("{O}#{n}")), "{n}");
        }
        let t = |n: &str| format!("{O}#type:Order/{n}");
        // Facts of a type.
        let order = format!("{O}#type:Order");
        assert_eq!(ix.str_fact(&order, "qname"), "{urn:example:orders}Order");
        assert_eq!(ix.str_fact(&order, "form"), "complex");
        assert_eq!(ix.str_fact(&order, "doc"), "An order placed by a customer.");
        assert_eq!(ix.bool_fact(&order, "open"), Some(true), "xs:any");
        let status = format!("{O}#type:Status");
        assert_eq!(ix.str_fact(&status, "form"), "enum");
        assert_eq!(ix.list_fact(&status, "enum"), ["new", "paid", "shipped"]);
        assert_eq!(ix.str_fact(&status, "base"), "string");
        assert_eq!(ix.str_fact(&format!("{O}#type:Code"), "form"), "simple");
        assert_eq!(
            ix.str_fact(&format!("{O}#group:Audit"), "form"),
            "attribute-group"
        );
        assert_eq!(ix.str_fact(&format!("{O}#group:OrderIds"), "form"), "group");
        assert_eq!(
            ix.str_fact(&format!("{O}#schema:order"), "namespace"),
            "urn:example:orders"
        );
        assert_eq!(
            ix.str_fact(&format!("{O}#element:order"), "type_ref"),
            "{urn:example:orders}Order"
        );

        // Fields: type word, required, many.
        let shape = |f: &str| {
            let n = t(&format!("field:{f}"));
            (
                ix.str_fact(&n, "field_type"),
                ix.bool_fact(&n, "required"),
                ix.bool_fact(&n, "many"),
            )
        };
        assert_eq!(shape("total"), ("number".into(), Some(false), Some(false)));
        assert_eq!(
            shape("status"),
            ("{urn:example:orders}Status".into(), Some(true), Some(false))
        );
        assert_eq!(shape("tag"), ("string".into(), Some(false), Some(true)));
        assert_eq!(
            shape("placed"),
            ("datetime".into(), Some(true), Some(false))
        );
        assert_eq!(
            shape("coupon"),
            ("string".into(), Some(false), Some(false)),
            "in a choice"
        );
        assert_eq!(shape("gift"), ("boolean".into(), Some(false), Some(false)));
        assert_eq!(
            shape("version"),
            ("integer".into(), Some(true), Some(false))
        );
        assert_eq!(shape("lang"), ("string".into(), Some(false), Some(false)));
        assert_eq!(ix.str_fact(&t("field:version"), "xml_node"), "attribute");
        assert_eq!(ix.str_fact(&t("field:total"), "xml_node"), "element");
        assert_eq!(ix.str_fact(&t("field:tag"), "max_occurs"), "unbounded");
        assert_eq!(ix.str_fact(&t("field:tag"), "min_occurs"), "0");
        // A derived type keeps its own members and links to its base.
        assert!(ix.has(&format!("{O}#type:RushOrder/field:priority")));
        assert_eq!(
            ix.str_fact(&format!("{O}#type:RushOrder"), "base"),
            "{urn:example:orders}Order"
        );
        // The span of a field is its element.
        let span = &ix.node(&t("field:total"))["span"];
        assert_eq!(span["start_line"], 40, "{span}");
        assert_eq!(span["start_col"], 7);
        // Containment.
        assert!(
            ix.links("contains")
                .contains(&(order.clone(), t("field:total")))
        );
        // Group member fields belong to the group.
        assert!(ix.has(&format!("{O}#group:OrderIds/field:id")));
        assert!(ix.has(&format!("{O}#group:Audit/field:createdBy")));
    }

    #[test]
    fn anonymous_and_nested_types() {
        let ix = open("xsd", "anonymous-nested");
        let f = "fx:schemas/shipment.xsd";
        for n in [
            "element:shipment",
            "element:shipment/field:address",
            "element:shipment/field:address/field:street",
            "element:shipment/field:address/field:city",
            "element:shipment/field:address/field:kind",
            "element:shipment/field:parcel/field:weight",
            "element:shipment/field:ref",
            "type:Route/field:stop/field:place",
        ] {
            assert!(ix.has(&format!("{f}#{n}")), "{n}");
        }
        let a = format!("{f}#element:shipment/field:address");
        assert_eq!(ix.str_fact(&a, "field_type"), "object");
        assert_eq!(
            ix.str_fact(&format!("{a}/field:kind"), "field_type"),
            "string",
            "an inline simple type gives its base"
        );
        assert_eq!(
            ix.list_fact(&format!("{a}/field:kind"), "enum"),
            ["home", "work"]
        );
        assert_eq!(
            ix.bool_fact(&format!("{a}/field:kind"), "required"),
            Some(true)
        );
        let p = format!("{f}#element:shipment/field:parcel");
        assert_eq!(ix.bool_fact(&p, "many"), Some(true));
        assert_eq!(
            ix.str_fact(&format!("{p}/field:weight"), "field_type"),
            "number"
        );
        assert_eq!(
            ix.bool_fact(&format!("{f}#type:Route/field:stop"), "many"),
            Some(true)
        );
        // Members are exported under the owner key, so a profile can map to them.
        assert!(ix.graph.contains("shipment"));
    }

    #[test]
    fn qualified_refs_resolve_across_files() {
        let ix = open("xsd", "refs-across-files");
        let money = "fx:common/money.xsd";
        let order = "fx:orders/order.xsd";
        // `cm:` and `c:` are two prefixes for one namespace: both resolve through the URI.
        let uses = ix.links("uses-type");
        assert!(
            uses.contains(&(
                format!("{order}#type:Order/field:total"),
                format!("{money}#type:Money")
            )),
            "{uses:?}"
        );
        assert!(uses.contains(&(
            format!("{order}#type:Order/field:currency"),
            format!("{money}#type:Currency")
        )));
        assert!(uses.contains(&(
            format!("{money}#type:Money/field:currency"),
            format!("{money}#type:Currency")
        )));
        assert!(ix.links("uses-element").contains(&(
            format!("{order}#type:Order/field:note"),
            format!("{money}#element:note")
        )));
        assert!(ix.links("uses-group").contains(&(
            format!("{order}#type:Order"),
            format!("{money}#group:Stamp")
        )));
        assert!(ix.links("extends").contains(&(
            format!("{order}#type:Refund"),
            format!("{money}#type:Money")
        )));
        assert_eq!(
            ix.str_fact(&format!("{order}#type:Refund"), "base"),
            "{urn:example:common}Money"
        );
        // A name no file defines stays unresolved and is reported by `dangling`.
        let dangling = ix.unresolved("uses-type");
        assert_eq!(dangling.len(), 1, "{dangling:?}");
        assert_eq!(dangling[0].0, format!("{order}#type:Order/field:ghost"));
        assert_eq!(dangling[0].1, ["schema-type:{urn:example:common}Missing"]);
        // The members are exported for profiles to map to.
        assert_eq!(
            ix.str_fact(&format!("{money}#type:Money/field:amount"), "field_type"),
            "number"
        );
    }

    #[test]
    fn import_and_include_link_files() {
        let ix = open("xsd", "imports");
        let main = "fx:schemas/main.xsd";
        let imports = ix.links("imports");
        let from = format!("{main}#schema:main");
        for to in [
            "fx:schemas/common/common.xsd",
            "fx:schemas/parts/part.xsd",
            "fx:schemas/parts/other.xsd",
        ] {
            assert!(
                imports.contains(&(from.clone(), to.to_string())),
                "{to}: {imports:?}"
            );
        }
        assert_eq!(
            imports.len(),
            3,
            "no link for a URL or a path outside the repository: {imports:?}"
        );
        // What was not linked is recorded on the file.
        assert_eq!(
            ix.list_fact("fx:schemas/main.xsd", "external_refs"),
            ["../../../outside.xsd", "http://example.test/remote.xsd"]
        );
    }

    #[test]
    fn types_declare_schema_contracts() {
        let ix = open("xsd", "types-fields");
        // Every named type declares `schema_type`, keyed `{ns}Name`; every global element
        // declares `schema_element`.
        for t in ["Order", "RushOrder", "Status", "Code"] {
            let key = format!("{{urn:example:orders}}{t}");
            assert_eq!(ix.contract("schema_type", &key), Some((0, 0, 1)), "{key}");
            assert_eq!(
                ix.declarers("schema_type", &key),
                [format!("fx:schemas/order.xsd#type:{t}")]
            );
        }
        for e in ["order", "status"] {
            let key = format!("{{urn:example:orders}}{e}");
            assert_eq!(
                ix.contract("schema_element", &key),
                Some((0, 0, 1)),
                "{key}"
            );
        }
        // Groups are not contracts.
        assert_eq!(
            ix.contract("schema_type", "{urn:example:orders}OrderIds"),
            None
        );
        // The confidence of the binding is Extracted.
        assert!(ix.graph.contains("\"kind\":\"declares\""));
        assert!(
            ix.graph
                .lines()
                .filter(|l| l.contains("\"kind\":\"declares\""))
                .all(|l| l.contains("Extracted"))
        );
    }

    #[test]
    fn malformed_keeps_partial() {
        let ix = open("xsd", "broken");
        let f = "fx:schemas/broken.xsd";
        // Everything before the error, and the elements open at it, are kept.
        assert!(ix.has(&format!("{f}#type:Good/field:x")));
        assert!(ix.has(&format!("{f}#type:Bad")));
        assert!(ix.has(&format!("{f}#type:Bad/field:y")));
        let rows = ix.errors("schemas/broken.xsd");
        assert_eq!(rows.len(), 1, "{rows:?}");
        let span = rows[0].span.expect("the error has a span");
        assert_eq!(span.start_line, 11, "{rows:?}");
        assert!(rows[0].message.starts_with("parse error:"));
        // Not XML at all, and XML that is not a schema, are reported without a panic.
        let (_t, dir) = generated(&[("a.xsd", "not xml at all"), ("b.xsd", "<root/>")]);
        let ix = Indexed::open(&dir);
        assert!(!ix.errors("a.xsd").is_empty());
        let rows = ix.errors("b.xsd");
        assert!(rows[0].message.contains("not an XML Schema"), "{rows:?}");
    }

    #[test]
    fn no_external_entities() {
        let tmp = tempfile::tempdir().unwrap();
        let canary = Utf8PathBuf::from_path_buf(tmp.path().join("canary.txt")).unwrap();
        std::fs::write(&canary, "CANARY-SECRET-4242").unwrap();
        let xsd = format!(
            "<?xml version=\"1.0\"?>\n<!DOCTYPE xs:schema [\n  <!ENTITY xxe SYSTEM \"file://{canary}\">\n  <!ENTITY % pe SYSTEM \"http://127.0.0.1:9/never.dtd\">\n  <!ENTITY a \"&b;&b;&b;&b;\">\n  <!ENTITY b \"LOL\">\n]>\n<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" targetNamespace=\"urn:example:xxe\">\n  <xs:element name=\"e\" type=\"xs:string\" default=\"&xxe;\"/>\n  <xs:complexType name=\"T\">\n    <xs:annotation><xs:documentation>see &xxe; and &a; and &#65;&lt;</xs:documentation></xs:annotation>\n    <xs:sequence><xs:element name=\"f\" type=\"xs:string\" default=\"&a;\"/></xs:sequence>\n  </xs:complexType>\n</xs:schema>\n"
        );
        let (_t, dir) = generated(&[("x.xsd", &xsd)]);
        let ix = Indexed::open(&dir);
        assert!(ix.errors("x.xsd").is_empty(), "{:?}", ix.errors("x.xsd"));
        assert!(
            !ix.graph.contains("CANARY-SECRET"),
            "the entity was not read"
        );
        // The references stay literal; the character references are resolved.
        assert_eq!(
            ix.str_fact("fx:x.xsd#type:T", "doc"),
            "see &xxe; and &a; and A<"
        );
        assert_eq!(ix.str_fact("fx:x.xsd#type:T/field:f", "default"), "&a;");
        assert!(ix.has("fx:x.xsd#type:T"));
    }
}

mod jsonschema {
    use super::*;

    const F: &str = "fx:schemas/order.schema.json";

    #[test]
    fn defs_and_properties() {
        let ix = open("json-schema", "defs-properties");
        for n in [
            "type:Order",
            "type:Money",
            "type:Code",
            "type:Order/field:id",
            "type:Order/field:lines/field:sku",
            "type:Order/field:lines/field:qty",
            "type:Order/field:address/field:city",
            "type:Money/field:amount",
        ] {
            assert!(ix.has(&format!("{F}#{n}")), "{n}");
        }
        let order = format!("{F}#type:Order");
        assert_eq!(ix.str_fact(&order, "qname"), "{}Order");
        assert_eq!(ix.str_fact(&order, "form"), "object");
        assert_eq!(ix.str_fact(&order, "doc"), "An order placed by a customer.");
        assert_eq!(ix.str_fact(&format!("{F}#type:Code"), "form"), "enum");
        assert_eq!(ix.list_fact(&format!("{F}#type:Code"), "enum"), ["A", "B"]);
        let shape = |n: &str| {
            let n = format!("{F}#type:Order/field:{n}");
            (
                ix.str_fact(&n, "field_type"),
                ix.bool_fact(&n, "required"),
                ix.bool_fact(&n, "many"),
            )
        };
        assert_eq!(shape("id"), ("string".into(), Some(true), Some(false)));
        assert_eq!(shape("total"), ("number".into(), Some(false), Some(false)));
        assert_eq!(
            shape("placed"),
            ("datetime".into(), Some(false), Some(false))
        );
        assert_eq!(shape("tags"), ("string".into(), Some(false), Some(true)));
        assert_eq!(
            shape("note"),
            ("string".into(), Some(false), Some(false)),
            "[t, null] is t"
        );
        assert_eq!(shape("lines"), ("object".into(), Some(true), Some(true)));
        assert_eq!(
            shape("address"),
            ("object".into(), Some(false), Some(false))
        );
        assert_eq!(shape("extra"), ("any".into(), Some(false), Some(false)));
        assert_eq!(
            ix.str_fact(&format!("{F}#type:Order/field:placed"), "format"),
            "date-time"
        );
        assert_eq!(
            ix.str_fact(&format!("{F}#type:Order/field:status"), "default"),
            "new"
        );
        assert_eq!(
            ix.list_fact(&format!("{F}#type:Order/field:status"), "enum"),
            ["new", "paid"]
        );
        assert_eq!(
            ix.str_fact(&format!("{F}#type:Order/field:id"), "doc"),
            "Identifier of the order."
        );
        // Nested through `items`, with its own `required`.
        let sku = format!("{F}#type:Order/field:lines/field:sku");
        assert_eq!(ix.bool_fact(&sku, "required"), Some(true));
        assert_eq!(
            ix.bool_fact(&format!("{F}#type:Order/field:lines/field:qty"), "required"),
            Some(false)
        );
        assert_eq!(
            ix.bool_fact(&format!("{F}#type:Money/field:amount"), "required"),
            Some(true)
        );
        // File-level facts.
        assert!(ix.graph.contains("\"dialect\":{\"Str\":\"draft-07\"}"));
        assert!(
            ix.graph
                .contains("\"schema_kind\":{\"Str\":\"json-schema\"}")
        );
        // The span of a property is its key to the end of its value.
        let span = &ix.node(&format!("{F}#type:Order/field:id"))["span"];
        assert_eq!(
            (span["start_line"].as_u64(), span["start_col"].as_u64()),
            (Some(8), Some(5))
        );
    }

    /// The symbols of a file without its path, with their facts.
    fn shapes(ix: &Indexed, file: &str) -> BTreeMap<String, Value> {
        let prefix = format!("fx:schemas/{file}#");
        ix.nodes
            .iter()
            .filter_map(|(k, v)| {
                k.strip_prefix(&prefix)
                    .map(|n| (n.to_string(), v["facts"].clone()))
            })
            .collect()
    }

    #[test]
    fn yaml_same_as_json() {
        let ix = open("json-schema", "yaml");
        let j = shapes(&ix, "shape.schema.json");
        let y = shapes(&ix, "shape.schema.yaml");
        assert!(j.len() >= 8, "{:?}", j.keys());
        assert_eq!(j.keys().collect::<Vec<_>>(), y.keys().collect::<Vec<_>>());
        for (name, facts) in &j {
            assert_eq!(facts, &y[name], "{name}");
        }
        // Both bind the same contract: two declarers.
        assert_eq!(
            ix.contract(
                "schema_type",
                "{https://example.test/schemas/shape.json}Shape"
            ),
            Some((0, 0, 2))
        );
        assert_eq!(
            ix.str_fact(
                "fx:schemas/shape.schema.yaml#type:Shape/field:size",
                "default"
            ),
            "1"
        );
    }

    #[test]
    fn component_schemas_in_api_documents() {
        let ix = open("json-schema", "api-components");
        // Routed by the case profile: component schemas become types, properties fields.
        for n in [
            "fx:api/openapi.yaml#type:Pet",
            "fx:api/openapi.yaml#type:Pet/field:name",
            "fx:api/openapi.yaml#type:Pet/field:owner",
            "fx:api/openapi.yaml#type:Owner",
            "fx:api/events.yaml#type:Placed/field:orderId",
            "fx:api/swagger.json#type:Tag/field:label",
        ] {
            assert!(ix.has(n), "{n}");
        }
        // The document itself is not a type.
        assert!(
            !ix.nodes
                .keys()
                .any(|k| k.contains("#type:openapi") || k.contains("#type:events"))
        );
        assert_eq!(
            ix.bool_fact("fx:api/openapi.yaml#type:Pet/field:name", "required"),
            Some(true)
        );
        assert_eq!(
            ix.bool_fact("fx:api/openapi.yaml#type:Pet/field:photos", "many"),
            Some(true)
        );
        assert_eq!(
            ix.str_fact("fx:api/openapi.yaml#type:Pet/field:photos", "field_type"),
            "binary"
        );
        assert_eq!(
            ix.str_fact("fx:api/events.yaml#type:Placed/field:at", "field_type"),
            "datetime"
        );
        assert!(ix.graph.contains("\"dialect\":{\"Str\":\"openapi-3\"}"));
        assert!(ix.graph.contains("\"dialect\":{\"Str\":\"asyncapi\"}"));
        assert!(ix.graph.contains("\"dialect\":{\"Str\":\"swagger-2\"}"));
        // `#/components/schemas/Owner` resolves inside the document.
        assert!(ix.links("uses-type").contains(&(
            "fx:api/openapi.yaml#type:Pet/field:owner".to_string(),
            "fx:api/openapi.yaml#type:Owner".to_string()
        )));
        assert_eq!(ix.contract("schema_type", "{}Pet"), Some((0, 0, 1)));
    }

    #[test]
    fn local_and_external_refs_resolve() {
        let ix = open("json-schema", "refs");
        let m = "fx:a/main.schema.json";
        let uses = ix.links("uses-type");
        let has = |from: &str, to: &str| {
            assert!(
                uses.contains(&(format!("{m}#{from}"), to.to_string())),
                "{from} -> {to}: {uses:?}"
            );
        };
        // Local, to another file (by its path, so two files without `$id` do not mix), to a
        // whole file, and to an absolute URI that another file's `$id` names.
        has("type:Main/field:local", &format!("{m}#type:Local"));
        has(
            "type:Main/field:money",
            "fx:b/common.schema.json#type:Money",
        );
        has(
            "type:Main/field:whole",
            "fx:b/common.schema.json#type:Common",
        );
        has(
            "type:Main/field:remote",
            "fx:c/with-id.schema.json#type:Ext",
        );
        has("type:Main/field:list", &format!("{m}#type:Local"));
        has("type:Main/field:pet", &format!("{m}#type:Cat"));
        has("type:Main/field:pet", &format!("{m}#type:Dog"));
        assert_eq!(
            ix.list_fact(&format!("{m}#type:Main/field:pet"), "variants"),
            ["{}Cat", "{}Dog"]
        );
        assert_eq!(
            ix.bool_fact(&format!("{m}#type:Main/field:list"), "many"),
            Some(true)
        );
        // `allOf`: a `$ref` member extends, an inline member merges into the owner.
        assert!(
            ix.links("extends")
                .contains(&(format!("{m}#type:Derived"), format!("{m}#type:Local")))
        );
        assert!(ix.links("extends").contains(&(
            format!("{m}#type:Alias"),
            "fx:b/common.schema.json#type:Money".to_string()
        )));
        assert_eq!(ix.str_fact(&format!("{m}#type:Derived"), "base"), "{}Local");
        assert_eq!(
            ix.bool_fact(&format!("{m}#type:Derived/field:extra"), "required"),
            Some(true)
        );
        // Files that are referred to are imported; an absolute URI and a path outside the
        // repository are not, and are recorded.
        let imports = ix.links("imports");
        assert!(imports.contains(&(
            format!("{m}#type:Main/field:money"),
            "fx:b/common.schema.json".to_string()
        )));
        assert!(imports.contains(&(
            format!("{m}#type:Main/field:whole"),
            "fx:b/common.schema.json".to_string()
        )));
        assert_eq!(imports.len(), 3, "{imports:?}");
        assert_eq!(
            ix.list_fact("fx:a/main.schema.json", "external_refs"),
            [
                "../../outside.schema.json",
                "https://example.test/schemas/ext.json"
            ]
        );
        // A pointer into a schema names no type: no reference, the text is kept.
        let inner = format!("{m}#type:Main/field:inner");
        assert_eq!(ix.str_fact(&inner, "ref"), "#/properties/local");
        assert_eq!(ix.str_fact(&inner, "field_type"), "any");
        assert_eq!(
            ix.str_fact(&format!("{m}#type:Main/field:remote"), "field_type"),
            "{https://example.test/schemas/ext.json}Ext"
        );
        assert_eq!(
            ix.unresolved("uses-type"),
            Vec::<(String, Vec<String>)>::new()
        );
    }

    #[test]
    fn missing_id_binds_inferred() {
        let ix = open("json-schema", "defs-properties");
        assert_eq!(ix.contract("schema_type", "{}Order"), Some((0, 0, 1)));
        assert_eq!(ix.contract("schema_type", "{}Money"), Some((0, 0, 1)));
        let declares: Vec<&str> = ix
            .graph
            .lines()
            .filter(|l| l.contains("\"kind\":\"declares\""))
            .collect();
        assert_eq!(declares.len(), 3);
        assert!(
            declares.iter().all(|l| l.contains("Inferred")),
            "{declares:?}"
        );
        // With an `$id` the key carries it and the binding is Extracted.
        let ix = open("json-schema", "yaml");
        let declares: Vec<&str> = ix
            .graph
            .lines()
            .filter(|l| l.contains("\"kind\":\"declares\""))
            .collect();
        assert!(!declares.is_empty());
        assert!(
            declares.iter().all(|l| l.contains("Extracted")),
            "{declares:?}"
        );
    }

    #[test]
    fn malformed_reports_span() {
        let ix = open("json-schema", "broken");
        // What was read before the error is kept.
        assert!(ix.has("fx:schemas/broken.schema.json#type:Broken/field:a"));
        assert!(ix.has("fx:schemas/broken.schema.json#type:Broken/field:b"));
        let rows = ix.errors("schemas/broken.schema.json");
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].span.expect("span").start_line, 7, "{rows:?}");
        let rows = ix.errors("schemas/broken.schema.yaml");
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].span.is_some());
        assert!(ix.has("fx:schemas/broken.schema.yaml#type:BrokenYaml/field:a"));
        // A second YAML document is reported, not read.
        let (_t, dir) = generated(&[(
            "m.schema.yaml",
            "title: M\ntype: object\nproperties:\n  a: {type: string}\n---\ntitle: N\n",
        )]);
        let ix = Indexed::open(&dir);
        assert!(
            ix.errors("m.schema.yaml")[0]
                .message
                .contains("only the first document")
        );
        assert!(ix.has("fx:m.schema.yaml#type:M/field:a"));
    }

    /// Every shard file of an indexed project: path under `shards/` to bytes.
    fn shard_bytes(c: &Case) -> BTreeMap<String, Vec<u8>> {
        fn walk(
            dir: &std::path::Path,
            base: &std::path::Path,
            out: &mut BTreeMap<String, Vec<u8>>,
        ) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, base, out);
                } else {
                    out.insert(
                        p.strip_prefix(base).unwrap().to_string_lossy().to_string(),
                        std::fs::read(&p).unwrap(),
                    );
                }
            }
        }
        let root = c.engine().dir().root().join("shards");
        let mut out = BTreeMap::new();
        walk(root.as_std_path(), root.as_std_path(), &mut out);
        out
    }

    #[test]
    fn shards_byte_identical_across_runs() {
        for (kind, name) in [
            ("json-schema", "defs-properties"),
            ("xsd", "types-fields"),
            ("json-schema", "refs"),
        ] {
            let dir = case_dir(kind, name);
            let a = Case::open(&registry, &dir).unwrap();
            let b = Case::open(&registry, &dir).unwrap();
            let (sa, sb) = (shard_bytes(&a), shard_bytes(&b));
            assert!(!sa.is_empty(), "{name}: no shards");
            assert_eq!(
                sa.keys().collect::<Vec<_>>(),
                sb.keys().collect::<Vec<_>>(),
                "{name}"
            );
            for (path, bytes) in &sa {
                assert!(
                    bytes == &sb[path],
                    "{name}: shard {path} differs between runs"
                );
            }
        }
    }
}

/// Write `files` under a fresh case directory.
fn generated(files: &[(&str, &str)]) -> (tempfile::TempDir, Utf8PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = Utf8PathBuf::from_path_buf(tmp.path().join("case")).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    for (name, text) in files {
        let p = dir.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    (tmp, dir)
}
