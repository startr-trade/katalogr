//! OpenAPI extraction and the request-line guest (FEAT-014 R-1, R-2, R-4, R-20, R-21).

use katalogr_ext_openapi::OpenApiExtractor;
use katalogr_model::{
    Confidence, ContractRole, EmbeddedInput, ExtractError, ExtractInput, Extractor, Fact, Fragment,
    Id, Name, RelPath, Span,
};
use std::path::Path;

fn fixture(case: &str, file: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/openapi")
        .join(case)
        .join(file);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn run(path: &str, text: &str) -> (Fragment, Vec<ExtractError>) {
    let path = RelPath::new(path).unwrap();
    OpenApiExtractor
        .extract_with_errors(&ExtractInput {
            repo: "fx",
            path: &path,
            revision: "ce013625030ba8dba906f756967f9e9ca394464a",
            content: text.as_bytes(),
            context: &[],
        })
        .unwrap()
}

fn names(f: &Fragment, kind: &str) -> Vec<String> {
    let mut v: Vec<String> = f
        .symbols
        .iter()
        .filter(|s| s.kind == kind)
        .map(|s| s.name.canonical())
        .collect();
    v.sort();
    v
}

fn declared(f: &Fragment) -> Vec<String> {
    let mut v: Vec<String> = f
        .bindings
        .iter()
        .filter(|b| b.contract.kind == "http_route")
        .inspect(|b| assert_eq!(b.role, ContractRole::Declares))
        .map(|b| b.contract.key.clone())
        .collect();
    v.sort();
    v
}

fn str_fact<'f>(f: &'f Fragment, sym: &str, key: &str) -> Option<&'f str> {
    let s = f
        .symbols
        .iter()
        .find(|s| s.name.canonical().ends_with(sym))
        .unwrap_or_else(|| panic!("no symbol {sym}"));
    match s.facts.get(key) {
        Some(Fact::Str(v)) => Some(v),
        _ => None,
    }
}

mod openapi {
    use super::*;

    #[test]
    fn operations_indexed_yaml_and_json() {
        for (file, text) in [
            ("openapi.yaml", fixture("yaml-v3", "openapi.yaml")),
            ("openapi.json", fixture("json-v3", "openapi.json")),
        ] {
            let (f, errors) = run(file, &text);
            assert!(errors.is_empty(), "{file}: {errors:?}");
            assert_eq!(
                names(&f, "operation"),
                vec![
                    format!("fx:{file}#operation:DELETE%20%2Fusers%2F{{id}}"),
                    format!("fx:{file}#operation:GET%20%2Fusers"),
                    format!("fx:{file}#operation:GET%20%2Fusers%2F{{id}}"),
                ],
                "{file}"
            );
            // Facts of one operation, and its span from the method key on.
            assert_eq!(
                str_fact(&f, "GET%20%2Fusers", "operation_id"),
                Some("listUsers")
            );
            assert_eq!(str_fact(&f, "GET%20%2Fusers", "method"), Some("GET"));
            assert_eq!(str_fact(&f, "GET%20%2Fusers", "path"), Some("/users"));
            assert_eq!(
                str_fact(&f, "GET%20%2Fusers", "route"),
                Some("GET /v1/users")
            );
            assert_eq!(str_fact(&f, "GET%20%2Fusers", "doc"), Some("List users"));
            // The first line of a description is the documentation.
            assert_eq!(
                str_fact(&f, "GET%20%2Fusers%2F{id}", "doc"),
                Some("Fetch one user.")
            );
            let del = f
                .symbols
                .iter()
                .find(|s| s.name.canonical().contains("DELETE"))
                .unwrap();
            assert_eq!(del.facts.get("deprecated"), Some(&Fact::Bool(true)));
            let get = f
                .symbols
                .iter()
                .find(|s| s.name.canonical().ends_with("GET%20%2Fusers"))
                .unwrap();
            assert_eq!(
                get.facts.get("tags"),
                Some(&Fact::List(vec![Fact::Str("users".into())]))
            );
            assert!(get.span.start_line < get.span.end_line);
            assert_eq!(
                f.artifact.facts.get("openapi_version"),
                Some(&Fact::Str("3.0.3".into()))
            );
            assert_eq!(
                f.artifact.facts.get("title"),
                Some(&Fact::Str("Example API".into()))
            );
            assert_eq!(
                f.artifact.facts.get("base_path"),
                Some(&Fact::Str("/v1".into()))
            );
        }
        // The same API in both forms gives the same routes.
        let (y, _) = run("openapi.yaml", &fixture("yaml-v3", "openapi.yaml"));
        let (j, _) = run("openapi.json", &fixture("json-v3", "openapi.json"));
        assert_eq!(declared(&y), declared(&j));
    }

    #[test]
    fn swagger2_indexed() {
        let (f, errors) = run("swagger.yaml", &fixture("swagger2", "swagger.yaml"));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            names(&f, "operation"),
            vec![
                "fx:swagger.yaml#operation:GET%20%2Fusers",
                "fx:swagger.yaml#operation:GET%20%2Fusers%2F{id}",
            ]
        );
        assert_eq!(
            f.artifact.facts.get("openapi_version"),
            Some(&Fact::Str("2.0".into()))
        );
        assert_eq!(
            declared(&f),
            vec!["GET /v1/users".to_string(), "GET /v1/users/{}".to_string()]
        );
    }

    #[test]
    fn operations_declare_routes() {
        let (f, _) = run("openapi.yaml", &fixture("yaml-v3", "openapi.yaml"));
        // `/users/{id}` under server `https://api.example.test/v1`.
        assert_eq!(
            declared(&f),
            vec![
                "DELETE /v1/users/{}".to_string(),
                "GET /v1/users".to_string(),
                "GET /v1/users/{}".to_string(),
            ]
        );
        let b = f
            .bindings
            .iter()
            .find(|b| b.contract.key == "GET /v1/users/{}")
            .unwrap();
        assert_eq!(b.confidence, Confidence::Extracted);
        // The binding sits on the operation, at its method key.
        let sym = f
            .symbols
            .iter()
            .find(|s| s.name.id() == b.symbol)
            .expect("bound symbol is in the fragment");
        assert_eq!(sym.kind, "operation");
        let span = b.span.unwrap();
        assert_eq!(span.start_line, sym.span.start_line);
        assert_eq!(span.start_col, sym.span.start_col);
        assert_eq!(span.end_line, span.start_line);
    }

    #[test]
    fn server_overrides_applied() {
        let text = "\
openapi: 3.1.0
servers:
  - url: https://{env}.example.test:{port}/root/
    variables:
      env: { default: api }
      port: { default: '8443' }
paths:
  /a:
    get: { responses: {} }
  /b:
    servers: [ { url: /item } ]
    get: { responses: {} }
    post:
      servers: [ { url: 'https://other.example.test/op' } ]
      responses: {}
  /c:
    get:
      servers: [ { url: 'https://other.example.test' } ]
      responses: {}
";
        let (f, errors) = run("openapi.yaml", text);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            declared(&f),
            vec![
                "GET /c".to_string(),
                "GET /item/b".to_string(),
                "GET /root/a".to_string(),
                "POST /op/b".to_string(),
            ]
        );
        assert_eq!(
            f.artifact.facts.get("base_path"),
            Some(&Fact::Str("/root".into()))
        );
    }

    #[test]
    fn conventional_names_claimed_only() {
        let x = OpenApiExtractor;
        for yes in [
            "openapi.yaml",
            "openapi-v2.json",
            "swagger.yml",
            "orders.openapi.yaml",
            "Billing.Swagger.JSON",
            "api/v1/openapi.yaml",
        ] {
            assert!(x.handles(&RelPath::new(yes).unwrap()), "{yes}");
        }
        for no in [
            "api.yaml",
            "spec.json",
            "docs/openapi/users.yaml",
            "openapi.txt",
            "notopenapi.yaml",
            "openapi.md",
        ] {
            assert!(!x.handles(&RelPath::new(no).unwrap()), "{no}");
        }
    }

    #[test]
    fn non_openapi_file_is_plain_artifact() {
        let text = fixture("not-openapi", "openapi-generator.yaml");
        let (f, errors) = run("openapi-generator.yaml", &text);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(f.symbols.is_empty());
        assert!(f.bindings.is_empty());
        assert!(f.artifact.facts.is_empty());
        assert_eq!(f.extension, "openapi");
        // A JSON file that names neither marker is cheap and plain too.
        let (f, errors) = run("openapi.json", "{\"a\": 1}");
        assert!(errors.is_empty() && f.symbols.is_empty());
    }

    #[test]
    fn parse_error_located() {
        let text = fixture("broken", "openapi.yaml");
        let (f, errors) = run("openapi.yaml", &text);
        assert!(!errors.is_empty());
        let ExtractError::Parse { span, .. } = &errors[0] else {
            panic!("{errors:?}")
        };
        let line = span.expect("located").start_line;
        assert!((10..=12).contains(&line), "line {line}");
        // The operation parsed before the error is kept.
        assert!(
            names(&f, "operation")
                .iter()
                .any(|n| n.ends_with("GET%20%2Ffirst")),
            "{:?}",
            names(&f, "operation")
        );
    }

    #[test]
    fn component_schemas_indexed() {
        let (f, _) = run("openapi.yaml", &fixture("yaml-v3", "openapi.yaml"));
        assert_eq!(
            names(&f, "type"),
            vec!["fx:openapi.yaml#type:User".to_string()]
        );
        assert_eq!(
            names(&f, "field"),
            vec![
                "fx:openapi.yaml#type:User/field:id",
                "fx:openapi.yaml#type:User/field:name",
            ]
        );
        let schema = f
            .bindings
            .iter()
            .find(|b| b.contract.kind == "schema_type")
            .expect("schema_type binding");
        assert_eq!(schema.contract.key, "{}User");
        // Swagger 2.0 `definitions` as well.
        let (f, _) = run("swagger.yaml", &fixture("swagger2", "swagger.yaml"));
        assert!(
            f.bindings
                .iter()
                .any(|b| b.contract.kind == "schema_type" && b.contract.key == "{}User")
        );
    }

    fn input<'a>(
        path: &'a RelPath,
        text: &'a str,
        holes: &'a [u32],
        owner: Id,
    ) -> EmbeddedInput<'a> {
        EmbeddedInput {
            repo: "fx",
            host_path: path,
            host_extension: "ts",
            owner,
            span: Span {
                start_line: 3,
                start_col: 5,
                end_line: 3,
                end_col: 30,
            },
            text,
            holes,
            call: None,
            context: &[],
        }
    }

    #[test]
    fn request_line_accepted_and_bound() {
        let x = OpenApiExtractor;
        assert_eq!(x.embedded_version(), Some("1"));
        let path = RelPath::new("src/a.ts").unwrap();
        let owner = Id::of("fx:src/a.ts#function:f");
        let bind = |text: &str, holes: &[u32]| {
            assert!(x.accepts_embedded(text) || !holes.is_empty(), "{text}");
            x.extract_embedded(&input(&path, text, holes, owner))
        };
        let b = bind("GET /users/{id}", &[]);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].symbol, owner);
        assert_eq!(b[0].role, ContractRole::Consumes);
        assert_eq!(b[0].contract.kind, "http_route");
        assert_eq!(b[0].contract.key, "GET /users/{}");
        assert_eq!(b[0].confidence, Confidence::Inferred);
        assert_eq!(b[0].span.unwrap().start_line, 3);

        let b = bind("POST https://api.example.test/v1/orders HTTP/1.1", &[]);
        assert_eq!(b[0].contract.key, "POST /v1/orders");
        let b = bind("HEAD /h HTTP/2", &[]);
        assert_eq!(b[0].contract.key, "HEAD /h");

        // A template `DELETE /users/${id}` arrives as text with one hole at the end.
        let b = bind("DELETE /users/", &[14]);
        assert_eq!(b[0].contract.key, "DELETE /users/{}");
    }

    #[test]
    fn prose_with_methods_not_accepted() {
        let x = OpenApiExtractor;
        for text in [
            "Get /help for more",
            "GET the /users list",
            "GET  /x",
            "GET users",
            "PURGE /x",
            "GET /x HTTP/one",
            "GET /x HTTP/1.1 extra",
            "GET /x\tHTTP/1.1",
            "GET",
            "",
            "get /x",
        ] {
            assert!(!x.accepts_embedded(text), "{text:?}");
        }
        for text in [
            "GET /x",
            " GET /x ",
            "GET http://h.example.test/x",
            "GET /x HTTP/1.0",
        ] {
            assert!(x.accepts_embedded(text), "{text:?}");
        }
        let path = RelPath::new("a.ts").unwrap();
        assert!(
            x.extract_embedded(&input(&path, "Get /help for more", &[], Id::of("x")))
                .is_empty()
        );
    }
}
