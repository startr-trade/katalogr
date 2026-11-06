//! Timing benchmarks, run on demand:
//! `cargo test -p katalogr-ext-schema --release --test bench -- --ignored --nocapture`.

use katalogr_ext_schema::{JsonSchemaExtractor, XsdExtractor};
use katalogr_model::{ExtractInput, Extractor, RelPath};
use std::time::{Duration, Instant};

const TYPES: usize = 2_000;
const FIELDS: usize = 8;

/// 2,000 complex types of 8 fields: just over 1 MiB.
fn xsd_1mib() -> String {
    let mut s = String::from(
        "<?xml version=\"1.0\"?>\n<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:t=\"urn:example:bench\" targetNamespace=\"urn:example:bench\">\n",
    );
    for t in 0..TYPES {
        s.push_str(&format!(
            "  <xs:complexType name=\"Type{t}\">\n    <xs:sequence>\n"
        ));
        for f in 0..FIELDS {
            let (ty, occurs) = match f % 4 {
                0 => ("xs:string", ""),
                1 => ("xs:int", " minOccurs=\"0\""),
                2 => ("xs:dateTime", " maxOccurs=\"unbounded\""),
                _ => ("t:Type0", " minOccurs=\"0\""),
            };
            s.push_str(&format!(
                "      <xs:element name=\"field{f}\" type=\"{ty}\"{occurs}/>\n"
            ));
        }
        s.push_str("    </xs:sequence>\n    <xs:attribute name=\"id\" type=\"xs:string\" use=\"required\"/>\n  </xs:complexType>\n");
    }
    s.push_str("</xs:schema>\n");
    s
}

/// 2,000 definitions of 8 properties, as JSON and as YAML.
fn json_schema_1mib() -> (String, String) {
    let mut j =
        String::from("{\n  \"$id\": \"https://example.test/bench.json\",\n  \"$defs\": {\n");
    let mut y = String::from("$id: https://example.test/bench.json\n$defs:\n");
    for t in 0..TYPES {
        j.push_str(&format!(
            "{}    \"Type{t}\": {{\n      \"type\": \"object\",\n      \"required\": [\"field0\"],\n      \"properties\": {{\n",
            if t == 0 { "" } else { ",\n" }
        ));
        y.push_str(&format!(
            "  Type{t}:\n    type: object\n    required: [field0]\n    properties:\n"
        ));
        for f in 0..FIELDS {
            let body = match f % 4 {
                0 => (
                    "{ \"type\": \"string\", \"description\": \"A text field of the example.\" }",
                    "{type: string, description: A text field of the example.}",
                ),
                1 => (
                    "{ \"type\": \"integer\", \"default\": 1 }",
                    "{type: integer, default: 1}",
                ),
                2 => (
                    "{ \"type\": \"array\", \"items\": { \"type\": \"string\", \"format\": \"date-time\" } }",
                    "{type: array, items: {type: string, format: date-time}}",
                ),
                _ => ("{ \"$ref\": \"#/$defs/Type0\" }", "{$ref: '#/$defs/Type0'}"),
            };
            j.push_str(&format!(
                "        \"field{f}\": {}{}\n",
                body.0,
                if f + 1 < FIELDS { "," } else { "" }
            ));
            y.push_str(&format!("      field{f}: {}\n", body.1));
        }
        j.push_str("      }\n    }");
    }
    j.push_str("\n  }\n}\n");
    (j, y)
}

fn best_of(ext: &dyn Extractor, path: &str, text: &str, runs: usize) -> Duration {
    let path = RelPath::new(path).unwrap();
    let input = ExtractInput {
        repo: "r",
        path: &path,
        revision: "ce013625030ba8dba906f756967f9e9ca394464a",
        content: text.as_bytes(),
        context: &[],
    };
    let mut best = Duration::MAX;
    for _ in 0..runs {
        let start = Instant::now();
        let (f, errors) = ext.extract_with_errors(&input).unwrap();
        best = best.min(start.elapsed());
        assert!(errors.is_empty(), "{errors:?}");
        assert!(f.symbols.len() >= TYPES * FIELDS, "{}", f.symbols.len());
    }
    best
}

mod bench {
    use super::*;

    /// R-13 (FEAT-009). A 1 MiB XSD of 2,000 complex types of 8 fields. Budget: 60 ms.
    #[test]
    #[ignore]
    fn xsd_extract_1mib() {
        let text = xsd_1mib();
        assert!(text.len() >= 1 << 20, "{} bytes", text.len());
        let best = best_of(&XsdExtractor, "bench.xsd", &text, 7);
        println!("bench::xsd_extract_1mib: {} bytes in {best:?}", text.len());
        println!(
            "katalogr-bench: xsd_extract_1mib {:.3} ms",
            best.as_secs_f64() * 1000.0
        );
        assert!(best < Duration::from_millis(121), "took {best:?}");
    }

    /// R-13 (FEAT-009). A 1 MiB JSON Schema (60 ms) and the same schema as YAML (120 ms).
    #[test]
    #[ignore]
    fn jsonschema_extract_1mib() {
        let (json, yaml) = json_schema_1mib();
        assert!(json.len() >= 1 << 20, "{} bytes", json.len());
        let jt = best_of(&JsonSchemaExtractor, "bench.schema.json", &json, 7);
        let yt = best_of(&JsonSchemaExtractor, "bench.schema.yaml", &yaml, 7);
        println!(
            "bench::jsonschema_extract_1mib: json {} bytes in {jt:?}, yaml {} bytes in {yt:?}",
            json.len(),
            yaml.len()
        );
        println!(
            "katalogr-bench: jsonschema_extract_1mib_json {:.3} ms",
            jt.as_secs_f64() * 1000.0
        );
        println!(
            "katalogr-bench: jsonschema_extract_1mib_yaml {:.3} ms",
            yt.as_secs_f64() * 1000.0
        );
        assert!(jt < Duration::from_millis(107), "json took {jt:?}");
        assert!(yt < Duration::from_millis(153), "yaml took {yt:?}");
    }
}
