//! The HTTP/JSON API server through the `katalogr api` binary (FEAT-049).

mod api {
    use fs4::FileExt;
    use serde_json::Value;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    fn cli(dir: &Path, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .env_remove("KATALOGR_CONTEXT")
            .args(args)
            .output()
            .expect("run katalogr")
    }

    fn ok(dir: &Path, args: &[&str]) -> String {
        let out = cli(dir, args);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn git(root: &Path, args: &[&str]) {
        let st = Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("run git");
        assert!(st.success(), "git {args:?} failed");
    }

    /// A clean, committed, indexed repository with `.tx` symbols. Returns (kindex, repo).
    fn project(tmp: &Path) -> (PathBuf, PathBuf) {
        let kx = tmp.join("kx");
        let repo = tmp.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("lib.tx"),
            "sym fn target\nsym use alias\nlink alias reexports target\n\
             export ns:alias alias\nexport ns:target target\nsym fn dup\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("user.tx"),
            "sym fn caller_a\nsym fn caller_b\nsym fn top\nsym item story\n\
             ref caller_a calls ns:target\nref top references ns:alias\n\
             link caller_b calls caller_a\nlink top calls caller_b\n\
             link caller_a refs-item story\nsym fn dup\n",
        )
        .unwrap();
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
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c1"]);
        ok(&repo, &["index"]);
        (kx, repo)
    }

    struct Server {
        child: Child,
        addr: SocketAddr,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn serve(repo: &Path, extra: &[&str]) -> Server {
        let log = repo.join("../api.log");
        let _ = std::fs::remove_file(&log);
        let mut child = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(repo)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(["api", "--listen", "127.0.0.1:0"])
            .args(extra)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .expect("spawn katalogr api");
        let started = Instant::now();
        loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if let Some(i) = text.find("listening on http://") {
                let rest = &text[i + "listening on http://".len()..];
                let addr = rest.lines().next().unwrap().trim().parse().unwrap();
                return Server { child, addr };
            }
            if let Ok(Some(st)) = child.try_wait() {
                panic!("katalogr api exited early ({st}): {text}");
            }
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "no start: {text}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[derive(Debug)]
    struct Reply {
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl Reply {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }

        fn json(&self) -> Value {
            serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {self:?}"))
        }
    }

    fn request(addr: SocketAddr, method: &str, path: &str, extra: &[(&str, &str)]) -> Reply {
        let mut req = format!("{method} {path} HTTP/1.0\r\nHost: {addr}\r\n");
        for (k, v) in extra {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("Connection: close\r\n\r\n");
        let mut s = TcpStream::connect(addr).expect("connect");
        s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut buf = Vec::new();
        let _ = s.read_to_end(&mut buf);
        let text = String::from_utf8_lossy(&buf).to_string();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let mut lines = head.lines();
        let status = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        Reply {
            status,
            headers,
            body: body.to_string(),
        }
    }

    fn get(s: &Server, path: &str) -> Reply {
        request(s.addr, "GET", path, &[])
    }

    fn repo_head(repo: &Path) -> String {
        let out = Command::new("git")
            .current_dir(repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// R-1. An exact answer's `ETag` is its view ID.
    #[test]
    fn etag_is_view_id() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        let r = get(&server, "/api/v1/find?text=target&consistency=wait");
        assert_eq!(r.status, 200, "{r:?}");
        let id = r.json()["view_id"].as_str().expect("view_id").to_string();
        assert_eq!(r.header("etag"), Some(format!("\"{id}\"").as_str()));
        assert_eq!(r.header("content-type"), Some("application/json"));
    }

    /// R-1. The same question at the same position repeats its ETag, and another differs.
    #[test]
    fn same_position_same_etag() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        let a = get(&server, "/api/v1/find?text=target&consistency=wait");
        let b = get(&server, "/api/v1/find?consistency=wait&text=target");
        let c = get(&server, "/api/v1/find?text=dup&consistency=wait");
        assert!(a.header("etag").is_some(), "{a:?}");
        assert_eq!(a.header("etag"), b.header("etag"));
        assert_ne!(a.header("etag"), c.header("etag"));
    }

    /// R-1. An answer that is not exact has no `ETag`.
    #[test]
    fn not_exact_has_no_etag() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        let r = get(&server, "/api/v1/find?text=target");
        assert_eq!(r.status, 200, "{r:?}");
        assert_eq!(r.json()["exact"], false);
        assert_eq!(r.header("etag"), None);
        assert!(r.json().get("view_id").is_none());
    }

    /// R-1. `If-None-Match` is answered 304 only when the read shows the same ID.
    #[test]
    fn not_modified_after_read() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        let path = "/api/v1/find?text=target&consistency=wait";
        let first = get(&server, path);
        let etag = first.header("etag").expect("etag").to_string();
        let same = request(server.addr, "GET", path, &[("If-None-Match", &etag)]);
        assert_eq!(same.status, 304, "{same:?}");
        assert_eq!(same.body, "");
        assert_eq!(same.header("etag"), Some(etag.as_str()));
        let other = request(server.addr, "GET", path, &[("If-None-Match", "\"nope\"")]);
        assert_eq!(other.status, 200);
        // After the position moves, the old ID no longer matches.
        std::fs::write(repo.join("extra.tx"), "sym fn extra\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c2"]);
        ok(&repo, &["index"]);
        let moved = request(server.addr, "GET", path, &[("If-None-Match", &etag)]);
        assert_eq!(moved.status, 200, "{moved:?}");
        assert_ne!(moved.header("etag"), Some(etag.as_str()));
    }

    /// R-2. `GET /api/v1/meta` is `katalogr contract --json`.
    #[test]
    fn meta_equals_contract_command() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        let r = get(&server, "/api/v1/meta");
        assert_eq!(r.status, 200);
        let cmd = ok(&repo, &["--json", "contract"]);
        assert_eq!(r.body, cmd.trim_end());
    }

    /// R-2. What a v1 client may rely on: meta is at one path, with these keys.
    #[test]
    fn older_client_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        let v = get(&server, "/api/v1/meta").json();
        assert_eq!(v["api"], 1);
        assert!(v["contract"].as_u64().unwrap() >= 1);
        assert_eq!(v["schema"], 1);
        assert_eq!(v["position_format"], 1);
        assert_eq!(v["view_format"], 1);
        assert!(v["katalogr"].is_string());
        let caps: Vec<&str> = v["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect();
        assert!(
            caps.contains(&"position") && caps.contains(&"view-id"),
            "{caps:?}"
        );
        // Every key of the strict v1 reader is there; new keys may only be added.
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        for k in [
            "api",
            "capabilities",
            "contract",
            "katalogr",
            "position_format",
            "schema",
            "view_format",
        ] {
            assert!(keys.contains(&k), "{k} missing in {keys:?}");
        }
    }

    /// R-3. The body is byte for byte what `--json` prints.
    #[test]
    fn body_equals_cli_json() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        let cases: &[(&str, &[&str])] = &[
            ("find?text=target", &["find", "target"]),
            ("refs?text=target", &["refs", "target"]),
            (
                "callers?text=target&depth=2",
                &["callers", "target", "--depth", "2"],
            ),
            ("explain?symbol=caller_a", &["explain", "caller_a"]),
            ("show?symbol=target", &["show", "target"]),
            ("path?from=top&to=caller_a", &["path", "top", "caller_a"]),
            ("map", &["map"]),
            ("search?terms=caller", &["search", "caller"]),
            ("dangling", &["dangling"]),
            (
                "find?text=target&budget=40",
                &["--budget", "40", "find", "target"],
            ),
        ];
        for (route, args) in cases {
            let sep = if route.contains('?') { '&' } else { '?' };
            let r = get(&server, &format!("/api/v1/{route}{sep}consistency=wait"));
            let mut full = vec!["--json", "--consistency", "wait"];
            full.extend_from_slice(args);
            let out = cli(&repo, &full);
            let want = String::from_utf8(out.stdout).unwrap();
            assert_eq!(r.body, want.trim_end(), "{route}: {r:?}");
        }
    }

    /// FEAT-007 R-8. The `deps` body is byte for byte what `deps --json` prints, with a filter and
    /// a limit.
    #[test]
    fn deps_body_equals_cli_json() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        std::fs::write(repo.join("Cargo.lock"), "version = 3\n\n[[package]]\nname = \"alpha-lib\"\nversion = \"1.2.3\"\nsource = \"registry+https://registry.example/index\"\n\n[[package]]\nname = \"beta-lib\"\nversion = \"0.9.0\"\nsource = \"registry+https://registry.example/index\"\n").unwrap();
        ok(&repo, &["index"]);
        let server = serve(&repo, &[]);
        let cases: &[(&str, &[&str])] = &[
            ("deps", &["deps"]),
            ("deps?package=beta-lib", &["deps", "--package", "beta-lib"]),
            ("deps?limit=1", &["deps", "--limit", "1"]),
            ("deps?used-by-limit=3", &["deps", "--used-by-limit", "3"]),
        ];
        for (route, args) in cases {
            let sep = if route.contains('?') { '&' } else { '?' };
            let r = get(&server, &format!("/api/v1/{route}{sep}consistency=wait"));
            assert_eq!(r.status, 200, "{route}: {r:?}");
            let mut full = vec!["--json", "--consistency", "wait"];
            full.extend_from_slice(args);
            let out = cli(&repo, &full);
            let want = String::from_utf8(out.stdout).unwrap();
            assert!(want.contains("-lib"), "{want}");
            assert_eq!(r.body, want.trim_end(), "{route}: {r:?}");
        }
    }

    /// R-10. The `impact` body is byte for byte what `impact --json` prints, for each way of
    /// giving the seeds and for the depth, limit and test-depth arguments.
    #[test]
    fn impact_body_equals_cli_json() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        let cases: &[(&str, &[&str])] = &[
            ("impact?text=target", &["impact", "target"]),
            ("impact?file=lib.tx", &["impact", "--file", "lib.tx"]),
            ("impact?since=HEAD", &["impact", "--since", "HEAD"]),
            (
                "impact?text=target&depth=1&limit=5&test-depth=2",
                &[
                    "impact",
                    "target",
                    "--depth",
                    "1",
                    "--limit",
                    "5",
                    "--test-depth",
                    "2",
                ],
            ),
        ];
        for (route, args) in cases {
            let sep = if route.contains('?') { '&' } else { '?' };
            let r = get(&server, &format!("/api/v1/{route}{sep}consistency=wait"));
            assert_eq!(r.status, 200, "{route}: {r:?}");
            let mut full = vec!["--json", "--consistency", "wait"];
            full.extend_from_slice(args);
            let out = cli(&repo, &full);
            assert!(out.status.success(), "{args:?}: {out:?}");
            let want = String::from_utf8(out.stdout).unwrap();
            assert_eq!(r.body, want.trim_end(), "{route}: {r:?}");
        }
        // A range and --at together are a usage error, as on the CLI.
        let r = get(
            &server,
            "/api/v1/impact?range=HEAD~1..HEAD&at=HEAD&consistency=wait",
        );
        assert_eq!(r.status, 400, "{r:?}");
    }

    /// R-3. `context` and `at` are parameters; a position nothing holds is 409.
    #[test]
    fn context_and_at_params() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        let head = repo_head(&repo);
        let r = get(
            &server,
            "/api/v1/find?text=target&consistency=wait&context=base",
        );
        let want = ok(
            &repo,
            &[
                "--json",
                "--consistency",
                "wait",
                "--context",
                "base",
                "find",
                "target",
            ],
        );
        assert_eq!(r.status, 200, "{r:?}");
        assert_eq!(r.body, want.trim_end());
        let r = get(
            &server,
            &format!("/api/v1/find?text=target&consistency=wait&at={head}"),
        );
        let want = ok(
            &repo,
            &[
                "--json",
                "--consistency",
                "wait",
                "--at",
                &head,
                "find",
                "target",
            ],
        );
        assert_eq!(r.status, 200, "{r:?}");
        assert_eq!(r.body, want.trim_end());
        let r = get(
            &server,
            "/api/v1/find?text=target&consistency=wait&at=0000000000000000000000000000000000000000",
        );
        assert_eq!(r.status, 409, "{r:?}");
        assert_eq!(r.json()["error"], "position-unavailable");
        assert!(r.json()["available"].is_array());
        let r = get(&server, "/api/v1/find?text=target&context=nonesuch");
        assert!(r.status == 400 || r.status == 500, "{r:?}");
        assert!(r.json()["error"].is_string());
    }

    /// R-3. The OpenAPI file is current.
    #[test]
    fn openapi_current() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        let r = get(&server, "/api/v1/openapi.json");
        assert_eq!(r.status, 200);
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/openapi/v1.json");
        if std::env::var_os("KATALOGR_BLESS").is_some() {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, &r.body).unwrap();
        }
        let have =
            std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        assert_eq!(
            have, r.body,
            "schema/openapi/v1.json is out of date: re-run with KATALOGR_BLESS=1"
        );
        let doc: Value = serde_json::from_str(&have).unwrap();
        assert_eq!(doc["openapi"], "3.1.0");
    }

    /// Check `instance` against the subset of JSON Schema the generated schemas use.
    fn validate(root: &Value, schema: &Value, instance: &Value, at: &str) -> Result<(), String> {
        if let Some(r) = schema.get("$ref") {
            let target = r
                .as_str()
                .and_then(|r| r.strip_prefix('#'))
                .and_then(|p| root.pointer(p))
                .ok_or_else(|| format!("{at}: unresolvable $ref {r}"))?;
            return validate(root, target, instance, at);
        }
        for key in ["anyOf", "oneOf"] {
            if let Some(options) = schema.get(key).and_then(Value::as_array) {
                if options
                    .iter()
                    .any(|o| validate(root, o, instance, at).is_ok())
                {
                    return Ok(());
                }
                return Err(format!("{at}: matches none of {key}"));
            }
        }
        if let Some(t) = schema.get("type") {
            let names: Vec<&str> = match t {
                Value::String(s) => vec![s.as_str()],
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                _ => return Err(format!("{at}: bad type keyword")),
            };
            let ok = names.iter().any(|n| match *n {
                "null" => instance.is_null(),
                "boolean" => instance.is_boolean(),
                "string" => instance.is_string(),
                "array" => instance.is_array(),
                "object" => instance.is_object(),
                "integer" => instance.is_i64() || instance.is_u64(),
                "number" => instance.is_number(),
                _ => false,
            });
            if !ok {
                return Err(format!("{at}: expected {names:?}, got {instance}"));
            }
        }
        if let Some(values) = schema.get("enum").and_then(Value::as_array)
            && !values.contains(instance)
        {
            return Err(format!("{at}: {instance} not in {values:?}"));
        }
        if let Some(obj) = instance.as_object() {
            for k in schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if !obj.contains_key(k) {
                    return Err(format!("{at}: missing required key {k:?}"));
                }
            }
            if let Some(props) = schema.get("properties").and_then(Value::as_object) {
                for (k, v) in obj {
                    if let Some(p) = props.get(k) {
                        validate(root, p, v, &format!("{at}.{k}"))?;
                    }
                }
            }
        }
        if let (Some(items), Some(arr)) = (schema.get("items"), instance.as_array()) {
            for (i, v) in arr.iter().enumerate() {
                validate(root, items, v, &format!("{at}[{i}]"))?;
            }
        }
        Ok(())
    }

    /// One request per route, with the parameters it needs.
    const ROUTES: &[(&str, &str)] = &[
        ("find", "text=target"),
        ("refs", "text=target"),
        ("callers", "text=target"),
        ("tests", "text=target"),
        ("why", "location=lib.tx:1"),
        ("explain", "symbol=caller_a"),
        ("show", "symbol=target"),
        ("path", "from=top&to=caller_a"),
        ("map", ""),
        ("search", "terms=caller"),
        ("dangling", ""),
        ("deps", ""),
        ("mirrors", ""),
        ("status", ""),
        ("docs-affected", ""),
        ("impact", "text=target"),
        ("position", ""),
        ("hubs", ""),
        ("cycles", ""),
        ("communities", ""),
        ("dead-code", ""),
        ("coverage", ""),
        ("rationale", "hubs=3"),
        ("doctor", "skip-git=true"),
        ("context-list", ""),
        ("context-diff", "a=base&b=current"),
        ("context-conflicts", "a=base&b=current"),
        ("views/", ""),
        ("views/structure", ""),
        ("views/contracts", ""),
        ("views/callers", "symbol=target"),
        ("views/impact", "symbol=target"),
        ("views/path", "from=top&to=caller_a"),
        ("views/cycles", ""),
        ("timeline", ""),
    ];

    /// R-3. Every route answers, and a body that has a `schema/v1` file validates against it.
    #[test]
    fn every_op_body_validates() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        let doc = get(&server, "/api/v1/openapi.json").json();
        let paths = doc["paths"].as_object().unwrap();
        for (route, query) in ROUTES {
            assert!(
                paths.contains_key(&format!("/api/v1/{route}")),
                "{route} is not in the OpenAPI document"
            );
            let r = get(
                &server,
                &format!("/api/v1/{route}?consistency=wait&{query}"),
            );
            assert!(matches!(r.status, 200 | 409), "{route}: {r:?}");
            let v = r.json();
            let file = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../schema/v1/{route}.schema.json"));
            if r.status == 200
                && let Ok(text) = std::fs::read_to_string(&file)
            {
                let schema: Value = serde_json::from_str(&text).unwrap();
                validate(&schema, &schema, &v, "$")
                    .unwrap_or_else(|e| panic!("{route}: {e}\n{}", r.body));
            }
        }
        // Every route in the table was exercised.
        let routes: Vec<&str> = ROUTES.iter().map(|(r, _)| *r).collect();
        for p in paths.keys() {
            let name = p.trim_start_matches("/api/v1/");
            // `stored-view` needs a stored ID; `timeline::` tests cover it.
            if name != "meta" && name != "openapi.json" && name != "stored-view" {
                assert!(routes.contains(&name), "{name} has no request in ROUTES");
            }
        }
    }

    /// R-3. Every CLI read command has a route; commands that change state or serve have none.
    #[test]
    fn every_read_command_has_a_row() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let help = ok(&repo, &["--help"]);
        let commands: Vec<String> = help
            .lines()
            .skip_while(|l| !l.starts_with("Commands:"))
            .skip(1)
            .take_while(|l| l.starts_with("  "))
            .filter_map(|l| l.split_whitespace().next())
            .map(str::to_string)
            .collect();
        assert!(commands.len() > 20, "{commands:?}");
        // No route, by design: they write, serve, install, run hooks or print for a person.
        let no_route = [
            "kindex",
            "project",
            "notify",
            "hook",
            "hooks",
            "replace",
            "claude",
            "skill",
            "session-start",
            "index",
            "api",
            // The API plus the web view's pages.
            "serve",
            "mcp",
            "eval",
            "profiles",
            "context",
            "help",
            // Polls and reindexes until interrupted, like `api`.
            "watch",
            // Writes a record into the project (`record`), or compares with one (`verify`); its
            // answers are its own types, not envelopes (FEAT-094 R-14).
            "audit",
            // Inspects and tidies the stored answers of this machine (FEAT-096): a maintenance
            // command with its own report types, not an envelope.
            "answers",
            // Prints one file's extracted text for a person, writing nothing (FEAT-094 R-13).
            "shard",
            // Exports a snapshot of the index to a file, or imports one (FEAT-097): it writes, and its
            // answers are its own types, not envelopes.
            "snapshot",
            // Draws a view for a person; the generated views are `view`, served under
            // `/api/v1/views/`.
            "render",
        ];
        // Served at another path.
        let elsewhere = [("contract", "meta"), ("view", "views/")];
        let doc = {
            let server = serve(&repo, &[]);
            get(&server, "/api/v1/openapi.json").json()
        };
        let paths = doc["paths"].as_object().unwrap();
        for c in &commands {
            if no_route.contains(&c.as_str()) {
                continue;
            }
            let route = elsewhere
                .iter()
                .find(|(n, _)| n == c)
                .map(|(_, r)| *r)
                .unwrap_or(c.as_str());
            assert!(
                paths.contains_key(&format!("/api/v1/{route}")),
                "read command `{c}` has no route"
            );
        }
        // `context` routes: list, diff and conflicts.
        for r in ["context-list", "context-diff", "context-conflicts"] {
            assert!(paths.contains_key(&format!("/api/v1/{r}")), "{r}");
        }
    }

    /// R-4. A timed-out wait answers 200, marked stale.
    #[test]
    fn stale_answer_is_200_marked() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        std::fs::write(repo.join("lib.tx"), "sym fn target\nsym fn changed\n").unwrap();
        ok(&repo, &["notify", "lib.tx"]);
        let lock = kx.join("projects/demo/.cache/processor.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock)
            .unwrap();
        FileExt::try_lock(&file).expect("take the processor lock");
        let r = get(&server, "/api/v1/status?consistency=wait&timeout=0.3");
        assert_eq!(r.status, 200, "{r:?}");
        assert_eq!(r.json()["stale"], true);
        assert_eq!(r.json()["pending"], 1);
    }

    /// R-5. No state survives a request: a change the CLI makes is seen by the next request.
    #[test]
    fn no_state_between_requests() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        let before = get(&server, "/api/v1/find?text=fresh_symbol&consistency=wait");
        assert_eq!(before.status, 404, "{before:?}");
        std::fs::write(repo.join("new.tx"), "sym fn fresh_symbol\n").unwrap();
        ok(&repo, &["notify", "new.tx"]);
        ok(&repo, &["index"]);
        let after = get(&server, "/api/v1/find?text=fresh_symbol&consistency=wait");
        assert_eq!(after.status, 200, "{after:?}");
        assert!(after.body.contains("fresh_symbol"));
    }

    /// R-5. The CLI is never blocked by the server: it indexes and queries beside it.
    #[test]
    fn cli_runs_while_server_up() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        let addr = server.addr;
        let readers: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(move || {
                    for _ in 0..10 {
                        let r = request(addr, "GET", "/api/v1/find?text=target", &[]);
                        assert_eq!(r.status, 200, "{r:?}");
                    }
                })
            })
            .collect();
        for i in 0..3 {
            std::fs::write(repo.join("w.tx"), format!("sym fn written_{i}\n")).unwrap();
            let started = Instant::now();
            ok(&repo, &["notify", "w.tx"]);
            ok(&repo, &["--timeout", "20", "index"]);
            ok(&repo, &["--timeout", "20", "find", &format!("written_{i}")]);
            assert!(started.elapsed() < Duration::from_secs(30));
        }
        for r in readers {
            r.join().unwrap();
        }
    }

    /// R-6. A `wait` read is fresh.
    #[test]
    fn wait_answers_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        std::fs::write(repo.join("n.tx"), "sym fn queued_symbol\n").unwrap();
        ok(&repo, &["notify", "n.tx"]);
        let r = get(&server, "/api/v1/find?text=queued_symbol&consistency=wait");
        assert_eq!(r.status, 200, "{r:?}");
        assert_eq!(r.json()["stale"], false);
        assert!(r.body.contains("queued_symbol"));
    }

    /// R-6. With `--no-process` a `wait` read does not process: it polls, then answers stale.
    #[test]
    fn no_process_polls_then_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &["--no-process"]);
        std::fs::write(repo.join("n.tx"), "sym fn queued_symbol\n").unwrap();
        ok(&repo, &["notify", "n.tx"]);
        let started = Instant::now();
        let r = get(&server, "/api/v1/status?consistency=wait&timeout=0.6");
        assert_eq!(r.status, 200, "{r:?}");
        assert_eq!(r.json()["stale"], true);
        assert_eq!(r.json()["pending"], 1);
        assert!(
            started.elapsed() >= Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
        // Once someone else processes the queue, the same read is fresh.
        ok(&repo, &["index"]);
        let r = get(&server, "/api/v1/status?consistency=wait&timeout=5");
        assert_eq!(r.json()["stale"], false, "{r:?}");
    }

    /// R-9. Only GET exists.
    #[test]
    fn only_get_405() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        for m in ["POST", "PUT", "DELETE", "PATCH"] {
            let r = request(server.addr, m, "/api/v1/find?text=target", &[]);
            assert_eq!(r.status, 405, "{m}: {r:?}");
            assert_eq!(r.json()["error"], "method-not-allowed");
        }
        // The mutating commands have no route.
        for op in ["index", "replace", "notify", "project", "kindex"] {
            assert_eq!(get(&server, &format!("/api/v1/{op}")).status, 404, "{op}");
        }
    }

    /// FEAT-088 R-6. `/api/v1/views/` is served: the catalogue at the prefix, a view per op, and
    /// still 404 for a name that is not an op.
    #[test]
    fn views_prefix_served() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        let r = get(&server, "/api/v1/views/");
        assert_eq!(r.status, 200, "{r:?}");
        assert!(r.json()["views"].is_array());
        let r = get(&server, "/api/v1/views/structure?consistency=wait");
        assert_eq!(r.status, 200, "{r:?}");
        assert!(r.json()["mermaid"].is_string());
        for p in ["/api/v1/views", "/api/v1/views/graph"] {
            let r = get(&server, p);
            assert_eq!(r.status, 404, "{p}: {r:?}");
            assert_eq!(r.json()["error"], "not-found");
        }
    }

    /// R-13. The server needs no board, only a project: nothing here sets one up.
    #[test]
    fn runs_without_board() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = project(tmp.path());
        let server = serve(&repo, &[]);
        assert_eq!(get(&server, "/api/v1/status").status, 200);
        assert_eq!(get(&server, "/api/v1/meta").status, 200);
        // A project that cannot be found is a usage error before anything binds.
        let nowhere = tempfile::tempdir().unwrap();
        let out = cli(nowhere.path(), &["api", "--listen", "127.0.0.1:0"]);
        assert_eq!(out.status.code(), Some(2), "{out:?}");
    }
}
