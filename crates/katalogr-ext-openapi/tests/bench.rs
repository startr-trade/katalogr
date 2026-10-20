//! FEAT-014 R-14. Run in release:
//! `cargo test --release -p katalogr-ext-openapi --test bench -- --ignored --nocapture`.

mod bench {
    use katalogr_ext_openapi::OpenApiExtractor;
    use katalogr_model::{ExtractInput, Extractor, RelPath};
    use std::time::{Duration, Instant};

    /// A description of about 100 bytes on every operation.
    const DESC: &str = "Returns or changes the items that belong to the resource, one page at a time, in creation order.";

    /// A synthetic document of 1,000 paths with two operations each (2,000 operations), with
    /// summaries, tags and parameters, as YAML and as JSON.
    fn document() -> (String, String) {
        let mut yaml = String::from(
            "openapi: 3.0.3\ninfo:\n  title: Example API\n  version: '1.0'\nservers:\n  - url: https://api.example.test/v1\npaths:\n",
        );
        let mut json = String::from(
            "{\"openapi\":\"3.0.3\",\"info\":{\"title\":\"Example API\",\"version\":\"1.0\"},\
             \"servers\":[{\"url\":\"https://api.example.test/v1\"}],\"paths\":{",
        );
        for i in 0..1000 {
            yaml.push_str(&format!("  /resource{i}/{{id}}/items:\n"));
            json.push_str(&format!(
                "{}\"/resource{i}/{{id}}/items\":{{",
                if i == 0 { "" } else { "," }
            ));
            for (k, (method, name)) in [("get", "List"), ("post", "Create")].iter().enumerate() {
                yaml.push_str(&format!(
                    "    {method}:\n      operationId: {}{i}\n      summary: {name} the items of resource {i}\n      description: {DESC}\n      \
                     tags: [resource{i}, items]\n      parameters:\n        - name: id\n          in: path\n          \
                     required: true\n          schema: {{type: string}}\n        - name: limit\n          in: query\n          \
                     schema: {{type: integer}}\n      responses:\n        '200':\n          description: ok\n",
                    name.to_lowercase()
                ));
                json.push_str(&format!(
                    "{}\"{method}\":{{\"operationId\":\"{}{i}\",\"summary\":\"{name} the items of resource {i}\",\"description\":\"{DESC}\",\
                     \"tags\":[\"resource{i}\",\"items\"],\"parameters\":[{{\"name\":\"id\",\"in\":\"path\",\
                     \"required\":true,\"schema\":{{\"type\":\"string\"}}}},{{\"name\":\"limit\",\"in\":\"query\",\
                     \"schema\":{{\"type\":\"integer\"}}}}],\"responses\":{{\"200\":{{\"description\":\"ok\"}}}}}}",
                    if k == 0 { "" } else { "," },
                    name.to_lowercase()
                ));
            }
            json.push('}');
        }
        json.push_str("}}");
        (yaml, json)
    }

    fn median(v: &mut [Duration]) -> Duration {
        v.sort();
        v[v.len() / 2]
    }

    fn time(path: &str, text: &str) -> Duration {
        let path = RelPath::new(path).unwrap();
        let mut runs = Vec::new();
        for _ in 0..5 {
            let t = Instant::now();
            let (f, errors) = OpenApiExtractor
                .extract_with_errors(&ExtractInput {
                    repo: "r",
                    path: &path,
                    revision: "ce013625030ba8dba906f756967f9e9ca394464a",
                    content: text.as_bytes(),
                    context: &[],
                })
                .unwrap();
            runs.push(t.elapsed());
            assert!(errors.is_empty(), "{errors:?}");
            // All operations and their `declares` bindings are produced.
            assert_eq!(
                f.symbols.iter().filter(|s| s.kind == "operation").count(),
                2000
            );
            assert_eq!(
                f.bindings
                    .iter()
                    .filter(|b| b.contract.kind == "http_route")
                    .count(),
                2000
            );
        }
        median(&mut runs)
    }

    #[test]
    #[ignore]
    fn openapi_2000_operations() {
        let (yaml, json) = document();
        eprintln!(
            "document sizes: yaml {} bytes, json {} bytes",
            yaml.len(),
            json.len()
        );
        let y = time("openapi.yaml", &yaml);
        let j = time("openapi.json", &json);
        eprintln!("openapi, 2000 operations: yaml {y:?}, json {j:?} (median of 5)");
        assert!(y < Duration::from_millis(200), "yaml {y:?}");
        assert!(j < Duration::from_millis(200), "json {j:?}");
    }
}
