//! The REST linker through the CLI (FEAT-014 R-2, R-4, R-9 to R-12, R-16, R-18, R-20): OpenAPI
//! operations, the handlers that serve them and the clients that call them meet on `http_route`
//! contracts. Every file is synthetic and written into a temporary repository.

mod rest {
    use serde_json::Value;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn katalogr(dir: &Path, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(args)
            .output()
            .expect("run katalogr");
        (
            out.status.code().expect("exit code"),
            String::from_utf8(out.stdout).expect("utf8"),
            String::from_utf8(out.stderr).expect("utf8"),
        )
    }

    fn ok(dir: &Path, args: &[&str]) -> String {
        let (code, out, err) = katalogr(dir, args);
        assert_eq!(code, 0, "{args:?}: {out}{err}");
        out
    }

    fn write(repo: &Path, files: &[(&str, &str)]) {
        for (name, text) in files {
            let p = repo.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
    }

    /// A project `demo` with one marked repo `app` under `tmp/<name>`, holding the files, with
    /// its kindex at `tmp/<name>-kx`. Returns the repo; not yet indexed.
    fn setup(tmp: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let kx = tmp.join(format!("{name}-kx"));
        let repo = tmp.join(name);
        std::fs::create_dir_all(&repo).unwrap();
        write(&repo, files);
        ok(tmp, &["kindex", "init", kx.to_str().unwrap()]);
        let spec = format!("app={}", repo.display());
        ok(
            tmp,
            &[
                "--kindex",
                kx.to_str().unwrap(),
                "project",
                "init",
                "demo",
                "--repo",
                &spec,
                "--mark",
            ],
        );
        repo
    }

    /// As [`setup`], indexed.
    fn project(tmp: &Path, files: &[(&str, &str)]) -> PathBuf {
        let repo = setup(tmp, "app", files);
        ok(&repo, &["index"]);
        repo
    }

    fn kindex_of(repo: &Path) -> PathBuf {
        let name = repo.file_name().unwrap().to_str().unwrap();
        repo.parent().unwrap().join(format!("{name}-kx"))
    }

    /// `(role, symbol, confidence)` of everything bound to `http_route:<key>`, sorted.
    fn route(repo: &Path, key: &str) -> Vec<(String, String, String)> {
        let (code, out, err) = katalogr(
            repo,
            &["--json", "refs", &format!("contract:http_route:{key}")],
        );
        if code != 0 {
            return Vec::new();
        }
        let v: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}{err}"));
        let mut rows: Vec<(String, String, String)> = v["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["kind"].as_str().unwrap().to_string(),
                    r["from"]["name"].as_str().unwrap().to_string(),
                    r["confidence"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        rows.sort();
        rows
    }

    /// The lines of `dangling --kind contract` that name `http_route:<key>` (the escaped form).
    fn dangling_route(repo: &Path, escaped_key: &str) -> Vec<String> {
        let (_, out, _) = katalogr(repo, &["dangling", "--kind", "contract"]);
        out.lines()
            .filter(|l| l.contains(&format!("contract:http_route:{escaped_key}")))
            .map(str::to_string)
            .collect()
    }

    fn row(role: &str, symbol: &str, confidence: &str) -> (String, String, String) {
        (role.into(), symbol.into(), confidence.into())
    }

    const CARGO: (&str, &str) = (
        "Cargo.toml",
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace]\n",
    );
    const LIB: (&str, &str) = ("src/lib.rs", "pub mod handlers;\npub mod routes;\n");

    #[test]
    fn axum_handler_provides() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[
                CARGO,
                LIB,
                ("src/handlers.rs", "pub fn show() {}\npub fn remove() {}\n"),
                (
                    "src/routes.rs",
                    "use crate::handlers::{remove, show};\n\n\
                     pub fn router() -> Router {\n    Router::new()\n        \
                     .route(\"/users/{id}\", get(show).delete(remove))\n}\n",
                ),
            ],
        );
        // The handler named in the registration provides the route, not the route function.
        assert_eq!(
            route(&repo, "GET /users/{}"),
            [row("provides", "app:src/handlers.rs#fn:show", "Declared")]
        );
        assert_eq!(
            route(&repo, "DELETE /users/{}"),
            [row("provides", "app:src/handlers.rs#fn:remove", "Declared")]
        );
    }

    const USERS_TS: (&str, &str) = ("src/handlers/users.ts", "export function show() {}\n");

    #[test]
    fn express_handler_provides() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[(
                "src/app.ts",
                "const app = express();\nfunction list() {}\napp.get(\"/items/:id\", list);\n",
            )],
        );
        assert_eq!(
            route(&repo, "GET /items/{}"),
            [row("provides", "app:src/app.ts#fn:list", "Declared")]
        );
    }

    #[test]
    fn imported_handler_provides() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[
                USERS_TS,
                (
                    "src/app.ts",
                    "import { show } from \"./handlers/users\";\nconst app = express();\n\
                     app.get(\"/users/:id\", auth, show);\nfunction auth() {}\n",
                ),
            ],
        );
        // A handler imported from another file provides the route, through a bound reference.
        assert_eq!(
            route(&repo, "GET /users/{}"),
            [row(
                "provides",
                "app:src/handlers/users.ts#fn:show",
                "Declared"
            )]
        );
        // Re-indexing the handler's file with the same names keeps the link; renaming the
        // handler moves it to the registering code, inferred.
        write(
            &repo,
            &[(
                "src/handlers/users.ts",
                "// edited\nexport function show() { return 1; }\n",
            )],
        );
        ok(&repo, &["index"]);
        assert_eq!(
            route(&repo, "GET /users/{}"),
            [row(
                "provides",
                "app:src/handlers/users.ts#fn:show",
                "Declared"
            )]
        );
        write(
            &repo,
            &[("src/handlers/users.ts", "export function display() {}\n")],
        );
        ok(&repo, &["index"]);
        assert!(route(&repo, "GET /users/{}").is_empty());
        // Top-level code is the artifact; `refs` lists symbols, `dangling` shows the holder.
        let held = dangling_route(&repo, "GET%20%2Fusers%2F{}");
        assert_eq!(held.len(), 1, "{held:?}");
        assert!(held[0].starts_with("provides  app:src/app.ts "), "{held:?}");
        write(&repo, &[USERS_TS]);
        ok(&repo, &["index"]);
        assert_eq!(
            route(&repo, "GET /users/{}"),
            [row(
                "provides",
                "app:src/handlers/users.ts#fn:show",
                "Declared"
            )]
        );
    }

    #[test]
    fn closure_handler_falls_back_inferred() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[
                CARGO,
                LIB,
                ("src/handlers.rs", ""),
                (
                    "src/routes.rs",
                    "pub fn router() -> Router {\n    Router::new().route(\"/inline\", get(|| async { \"ok\" }))\n}\n",
                ),
                (
                    "src/app.ts",
                    "const app = express();\nfunction mount() {\n  app.get(\"/arrow\", (req, res) => res.end());\n}\n\
                     app.get(\"/top\", function (req, res) {});\n",
                ),
            ],
        );
        // The registering symbol provides, inferred.
        assert_eq!(
            route(&repo, "GET /inline"),
            [row("provides", "app:src/routes.rs#fn:router", "Inferred")]
        );
        assert_eq!(
            route(&repo, "GET /arrow"),
            [row("provides", "app:src/app.ts#fn:mount", "Inferred")]
        );
        // A function expression at top level falls back to the artifact.
        let held = dangling_route(&repo, "GET%20%2Ftop");
        assert!(
            held.len() == 1 && held[0].starts_with("provides  app:src/app.ts "),
            "{held:?}"
        );
    }

    #[test]
    fn unresolved_handler_falls_back_inferred() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[(
                "src/app.ts",
                "import { gone } from \"some-package\";\nconst app = express();\n\
                 function mount() { app.post(\"/g\", gone); }\n",
            )],
        );
        assert_eq!(
            route(&repo, "POST /g"),
            [row("provides", "app:src/app.ts#fn:mount", "Inferred")]
        );
        // A handler imported from a file of the repository that has no such export is a bound
        // reference that resolves to nothing: the registering symbol holds the route (inferred)
        // and the reference is listed with the other unresolved ones.
        write(
            &repo,
            &[(
                "src/more.ts",
                "import { absent } from \"./missing\";\nconst app = express();\n\
                 function mount2() { app.put(\"/h\", absent); }\n",
            )],
        );
        ok(&repo, &["index"]);
        assert_eq!(
            route(&repo, "PUT /h"),
            [row("provides", "app:src/more.ts#fn:mount2", "Inferred")]
        );
        let (_, out, _) = katalogr(&repo, &["dangling", "--kind", "provides"]);
        assert!(
            out.contains("#fn:mount2") && out.contains("missing"),
            "{out}"
        );
    }

    #[test]
    fn fetch_and_axios_consume() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[(
                "src/client.ts",
                "export async function load(id: number) {\n  \
                 const a = await fetch(`/users/${id}`);\n  \
                 const b = await fetch(\"/orders\", { method: \"POST\" });\n  \
                 const c = await axios.get(\"/ping\");\n  \
                 const d = await axios({ url: \"/things/\" + id, method: \"delete\" });\n  \
                 return [a, b, c, d];\n}\n\n\
                 export class Svc {\n  constructor(private http: HttpClient) {}\n  \
                 list() { return this.http.get(\"/v1/items\"); }\n}\n",
            )],
        );
        let load = "app:src/client.ts#fn:load";
        assert_eq!(
            route(&repo, "GET /users/{}"),
            [row("consumes", load, "Declared")]
        );
        assert_eq!(
            route(&repo, "POST /orders"),
            [row("consumes", load, "Declared")]
        );
        assert_eq!(
            route(&repo, "GET /ping"),
            [row("consumes", load, "Declared")]
        );
        assert_eq!(
            route(&repo, "DELETE /things/{}"),
            [row("consumes", load, "Declared")]
        );
        // A receiver matched by its name is inferred.
        assert_eq!(
            route(&repo, "GET /v1/items"),
            [row(
                "consumes",
                "app:src/client.ts#class:Svc/method:list",
                "Inferred"
            )]
        );
    }

    #[test]
    fn rust_client_consumes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[
                CARGO,
                ("src/lib.rs", "pub mod client;\n"),
                (
                    "src/client.rs",
                    "pub fn run(base: &str) {\n    let client = reqwest::Client::new();\n    \
                     let _ = client.get(\"https://api.example.test/users\");\n    \
                     let _ = client.post(format!(\"{}/orders/{id}\", base));\n    \
                     let _ = reqwest::get(\"https://api.example.test/ping\".to_string());\n}\n",
                ),
            ],
        );
        let run = "app:src/client.rs#fn:run";
        assert_eq!(
            route(&repo, "GET /users"),
            [row("consumes", run, "Inferred")]
        );
        // The format string's holes become parameters; the leading hole is the base.
        assert_eq!(
            route(&repo, "POST /orders/{}"),
            [row("consumes", run, "Inferred")]
        );
        assert_eq!(
            route(&repo, "GET /ping"),
            [row("consumes", run, "Declared")]
        );
    }

    const SPEC: &str = "openapi: 3.0.3\ninfo: { title: Example API, version: '1.0' }\n\
        servers:\n  - url: https://api.example.test/v1\npaths:\n  /users/{id}:\n    get:\n      \
        operationId: getUser\n      responses: {}\n";

    #[test]
    fn openapi_server_client_linked() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[
                CARGO,
                LIB,
                ("openapi.yaml", SPEC),
                ("src/handlers.rs", "pub fn show() {}\n"),
                (
                    "src/routes.rs",
                    "use crate::handlers::show;\n\npub fn router() -> Router {\n    \
                     Router::new().route(\"/v1/users/:id\", get(show))\n}\n",
                ),
                (
                    "web/client.ts",
                    "export async function loadUser(id: number) {\n  \
                     return fetch(`/v1/users/${id}`);\n}\n",
                ),
            ],
        );
        // The operation, the handler and the client meet on one contract.
        assert_eq!(
            route(&repo, "GET /v1/users/{}"),
            [
                row("consumes", "app:web/client.ts#fn:loadUser", "Declared"),
                row(
                    "declares",
                    "app:openapi.yaml#operation:GET%20%2Fusers%2F{id}",
                    "Extracted"
                ),
                row("provides", "app:src/handlers.rs#fn:show", "Declared"),
            ]
        );
        // callers of the handler lists the client through the contract.
        let callers = ok(&repo, &["callers", "app:src/handlers.rs#fn:show"]);
        let line = callers
            .lines()
            .find(|l| l.contains("#fn:loadUser"))
            .unwrap_or_else(|| panic!("{callers}"));
        assert!(
            line.contains("via contract:http_route:GET%20%2Fv1%2Fusers%2F{}"),
            "{line}"
        );
        // path from the client to the operation hops through the contract.
        let path = ok(
            &repo,
            &[
                "path",
                "app:web/client.ts#fn:loadUser",
                "app:openapi.yaml#operation:GET%20%2Fusers%2F{id}",
            ],
        );
        assert!(
            path.contains("contract:http_route:GET%20%2Fv1%2Fusers%2F{}"),
            "{path}"
        );
    }

    #[test]
    fn undocumented_and_unserved_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[
                (
                    "openapi.yaml",
                    "openapi: 3.0.3\npaths:\n  /declared-only:\n    get: { responses: {} }\n",
                ),
                (
                    "src/client.ts",
                    "export function ping() { return fetch(\"/client-only\"); }\n",
                ),
                (
                    "src/app.ts",
                    "function health() {}\nconst app = express();\napp.get(\"/server-only\", health);\n",
                ),
            ],
        );
        let (code, out, _) = katalogr(&repo, &["dangling", "--kind", "contract"]);
        assert_eq!(code, 1, "{out}");
        let lines: Vec<&str> = out.lines().filter(|l| l.contains("contract:")).collect();
        assert_eq!(lines.len(), 2, "{out}");
        assert!(
            lines.iter().any(|l| l.starts_with("consumes")
                && l.contains("#fn:ping")
                && l.contains("GET%20%2Fclient-only")),
            "{out}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("provides")
                && l.contains("#fn:health")
                && l.contains("GET%20%2Fserver-only")),
            "{out}"
        );
        assert!(!out.contains("declared-only"), "{out}");
    }

    #[test]
    fn base_path_applied() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[
                ("openapi.yaml", &SPEC.replace("/users/{id}", "/users")),
                (
                    "client.ts",
                    "export function list() { return fetch(\"https://api.example.test/v1/users\"); }\n",
                ),
            ],
        );
        let rows = route(&repo, "GET /v1/users");
        assert_eq!(
            rows,
            [
                row("consumes", "app:client.ts#fn:list", "Declared"),
                row(
                    "declares",
                    "app:openapi.yaml#operation:GET%20%2Fusers",
                    "Extracted"
                ),
            ]
        );
    }

    #[test]
    fn select_routes_spec_to_openapi() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), &[("specs/users.yaml", SPEC)]);
        // A non-conventional name is not claimed.
        assert!(route(&repo, "GET /v1/users/{}").is_empty());
        let (code, _, _) = katalogr(&repo, &["find", "operation:GET"]);
        assert_ne!(code, 0);
        // A project profile routes it to the OpenAPI extension.
        let dir = kindex_of(&repo).join("projects/demo/profiles");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("specs.yaml"),
            "profile: specs\nversion: 2\nselect:\n  - { extension: openapi, path: 'specs/**/*.yaml' }\nrules: []\n",
        )
        .unwrap();
        ok(&repo, &["index"]);
        assert_eq!(
            route(&repo, "GET /v1/users/{}"),
            [row(
                "declares",
                "app:specs/users.yaml#operation:GET%20%2Fusers%2F{id}",
                "Extracted"
            )]
        );
        // Removing the profile unroutes it again.
        std::fs::remove_file(dir.join("specs.yaml")).unwrap();
        ok(&repo, &["index"]);
        assert!(route(&repo, "GET /v1/users/{}").is_empty());
    }

    #[test]
    fn request_line_literal_consumes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[
                CARGO,
                (
                    "src/lib.rs",
                    "pub fn remove_user() -> &'static str {\n    \"DELETE /users/{id}\"\n}\n",
                ),
                (
                    "src/client.ts",
                    "export function lookup(id: number) {\n  const R = `GET /users/${id}`;\n  return R;\n}\n",
                ),
            ],
        );
        assert_eq!(
            route(&repo, "DELETE /users/{}"),
            [row("consumes", "app:src/lib.rs#fn:remove_user", "Inferred")]
        );
        assert_eq!(
            route(&repo, "GET /users/{}"),
            [row("consumes", "app:src/client.ts#fn:lookup", "Inferred")]
        );
        // The origin of the link is the embedded guest.
        let json = ok(&repo, &["--json", "explain", "app:src/client.ts#fn:lookup"]);
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            v["result"]["outgoing"][0]["origin"], "embedded:openapi",
            "{json}"
        );
    }

    #[test]
    fn nested_prefix_via_project_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = setup(
            tmp.path(),
            "app",
            &[
                CARGO,
                ("src/lib.rs", "pub mod api;\npub mod web;\n"),
                ("src/api.rs", "pub fn list_users() {}\n"),
                (
                    "src/web.rs",
                    "pub fn mounted() -> Router {\n    Router::new().route(\"/users\", get(crate::api::list_users))\n}\n",
                ),
                (
                    "src/client.ts",
                    "export function users() { return fetch(\"/api/users\"); }\n",
                ),
            ],
        );
        // Without a prefix rule the mounted route is keyed as written.
        ok(&repo, &["index"]);
        assert_eq!(
            route(&repo, "GET /users"),
            [row("provides", "app:src/api.rs#fn:list_users", "Declared")]
        );
        assert!(
            route(&repo, "GET /api/users")
                .iter()
                .all(|r| r.0 != "provides")
        );

        // A copy of the built-in with the same name replaces it and adds the prefix to the
        // routes of the files below `src/web.rs`.
        let builtin = ok(&repo, &["profiles", "show", "http-rust"]);
        let copy = builtin.replacen(
            "  - id: axum-get\n    when: { extension: rust,",
            "  - id: axum-get\n    when: { extension: rust, path: 'src/web.rs',",
            1,
        );
        assert_ne!(copy, builtin);
        let copy = copy.replacen(
            "key: 'GET $1' }, role: provides",
            "key: 'GET /api$1' }, role: provides",
            1,
        );
        let dir = kindex_of(&repo).join("projects/demo/profiles");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("rust-routes.yaml"), copy).unwrap();
        let list = ok(&repo, &["profiles", "list"]);
        assert!(
            list.contains("profiles/rust-routes.yaml (replaces builtin:http-rust.yaml)"),
            "{list}"
        );
        ok(&repo, &["index"]);
        // The handler now provides the prefixed route the client calls.
        assert_eq!(
            route(&repo, "GET /api/users"),
            [
                row("consumes", "app:src/client.ts#fn:users", "Declared"),
                row("provides", "app:src/api.rs#fn:list_users", "Declared"),
            ]
        );
        assert!(route(&repo, "GET /users").is_empty());
    }

    /// Every shard of the project's kindex, by path.
    fn shards(repo: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
        fn walk(dir: &Path, base: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, base, out);
                } else {
                    let rel = p.strip_prefix(base).unwrap().to_str().unwrap().to_string();
                    out.insert(rel, std::fs::read(&p).unwrap());
                }
            }
        }
        let base = kindex_of(repo).join("projects/demo/shards");
        let mut out = std::collections::BTreeMap::new();
        walk(&base, &base, &mut out);
        out
    }

    #[test]
    fn shards_deterministic() {
        let files: Vec<(&str, &str)> = vec![
            CARGO,
            LIB,
            ("openapi.yaml", SPEC),
            ("src/handlers.rs", "pub fn show() {}\n"),
            (
                "src/routes.rs",
                "use crate::handlers::show;\n\npub fn router() -> Router {\n    \
                 Router::new().route(\"/v1/users/:id\", get(show))\n}\n",
            ),
            (
                "web/app.ts",
                "import { handle } from \"./handlers\";\nconst app = express();\n\
                 app.get(\"/v1/users/:id\", handle);\n",
            ),
            ("web/handlers.ts", "export function handle() {}\n"),
            (
                "web/client.ts",
                "export function load(id: number) { return fetch(`/v1/users/${id}`); }\n",
            ),
        ];
        let tmp = tempfile::tempdir().unwrap();
        // One project is indexed by a scan, the other from change events in reverse order.
        let a = setup(tmp.path(), "a", &files);
        ok(&a, &["index", "--full"]);
        let b = setup(tmp.path(), "b", &files);
        let mut reversed: Vec<String> = files.iter().map(|f| f.0.to_string()).collect();
        reversed.reverse();
        for f in &reversed {
            ok(&b, &["notify", f]);
        }
        ok(&b, &["index"]);
        let (sa, sb) = (shards(&a), shards(&b));
        assert!(sa.len() >= files.len(), "{:?}", sa.keys());
        assert_eq!(sa.keys().collect::<Vec<_>>(), sb.keys().collect::<Vec<_>>());
        for (k, v) in &sa {
            assert_eq!(v, &sb[k], "shard {k} differs");
        }
        // The same links after stitching.
        let key = "GET /v1/users/{}";
        assert_eq!(route(&a, key), route(&b, key));
        assert_eq!(route(&a, key).len(), 4, "operation, handlers and client");
    }
}
