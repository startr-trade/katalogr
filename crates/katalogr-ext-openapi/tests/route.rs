//! The `http_route` normaliser (FEAT-014 R-3), over the test vectors of the design.

use katalogr_ext_openapi::route as http;
use katalogr_ext_openapi::route::{normalise, normalise_raw};

mod route {
    use super::*;

    fn raw(cases: &[(&str, Option<&str>)]) {
        for (input, want) in cases {
            assert_eq!(normalise_raw(input).as_deref(), *want, "input {input:?}");
        }
    }

    #[test]
    fn params_normalised_across_styles() {
        raw(&[
            ("get /users/{id}", Some("GET /users/{}")),
            ("GET /users/:userId", Some("GET /users/{}")),
            ("GET /users/:userId?", Some("GET /users/{}")),
            ("GET /users/:id(\\d+)", Some("GET /users/{}")),
            ("post /files/*rest", Some("POST /files/{*}")),
            ("POST /files/{*path}", Some("POST /files/{*}")),
            ("POST /files/:p*", Some("POST /files/{*}")),
            ("POST /files/:p+", Some("POST /files/{*}")),
            ("GET /x/{id}.json", Some("GET /x/{}.json")),
            ("GET /x/{a}-{b}", Some("GET /x/{}-{}")),
            ("GET /x/{}", Some("GET /x/{}")),
        ]);
    }

    #[test]
    fn urls_reduced_to_paths() {
        raw(&[
            (" {}/users/{}", Some("GET /users/{}")),
            (
                "\"get\" https://api.example.test/users/{id}?x=1#f",
                Some("GET /users/{}"),
            ),
            ("GET https://api.example.test", Some("GET /")),
            ("GET https://api.example.test/", Some("GET /")),
            ("GET //api.example.test/a", Some("GET /a")),
            ("DELETE /a//b/", Some("DELETE /a/b")),
            ("GET /v1/items?limit=3", Some("GET /v1/items")),
            ("GET {}/v1/items", Some("GET /v1/items")),
            ("GET /", Some("GET /")),
        ]);
        assert_eq!(http::path_of("/a/b/").as_deref(), Some("/a/b"));
        assert_eq!(http::base_of("https://api.example.test/v1/"), "/v1");
        assert_eq!(http::base_of("https://api.example.test"), "");
        assert_eq!(http::base_of("/v1"), "/v1");
        assert_eq!(http::base_of("v1"), "/v1");
        assert_eq!(http::base_of("{scheme}://h.example.test/v2"), "/v2");
    }

    #[test]
    fn methods_normalised() {
        raw(&[
            ("any /h", Some("* /h")),
            ("all /h", Some("* /h")),
            ("'post' /h", Some("POST /h")),
            ("/h", Some("GET /h")),
            ("patch /h", Some("PATCH /h")),
        ]);
        assert_eq!(normalise("", "/x").as_deref(), Some("GET /x"));
        assert_eq!(normalise("Trace", "/x").as_deref(), Some("TRACE /x"));
    }

    #[test]
    fn invalid_targets_rejected() {
        raw(&[
            ("GET users", None),
            ("FOO /x", None),
            ("m /x", None),
            ("GET", None),
            ("", None),
            ("GET https://", Some("GET /")),
            ("GET ?x=1", None),
        ]);
        let n: &dyn katalogr_model::KeyNormaliser = &http::HTTP_ROUTE;
        assert_eq!(n.id(), "http_route");
        assert_eq!(n.version(), 1);
        assert_eq!(n.normalise("get /a/:b").as_deref(), Some("GET /a/{}"));
    }
}
