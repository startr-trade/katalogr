//! MCP wrapper tests (FEAT-067): the real binary, spoken to over piped stdio.
//!
//! The scripted `Client` sends JSON-RPC lines and reads what comes back with a timeout, so a
//! server that hangs fails a test instead of stalling it. The project is the synthetic `.tx`
//! query project that `tests/cli.rs` builds (duplicated here, as `tests/wasm.rs` does).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_katalogr");

fn katalogr(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .current_dir(dir)
        .env_remove("KATALOGR_KINDEX")
        .env_remove("KATALOGR_PROJECT")
        .args(args)
        .output()
        .expect("run katalogr")
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let out = katalogr(dir, args);
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8 stdout")
}

/// A kindex and a project `demo` with one marked repo `app` holding a file.
fn setup(tmp: &Path) -> (PathBuf, PathBuf) {
    let kx = tmp.join("kx");
    let repo = tmp.join("app");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
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
    (kx, repo)
}

/// A project with the `.tx` fixtures below, indexed. Returns the kindex and repo paths.
fn query_project(tmp: &Path) -> (PathBuf, PathBuf) {
    let (kx, repo) = setup(tmp);
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
    ok(&repo, &["index"]);
    (kx, repo)
}

/// Run a CLI command: `(exit code, stdout, stderr)`.
fn run_raw(repo: &Path, args: &[&str]) -> (i32, String, String) {
    let out = katalogr(repo, args);
    (
        out.status.code().expect("exit code"),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

/// A scripted MCP client over a spawned `katalogr mcp`.
struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
}

impl Client {
    /// Spawn `katalogr <global...> mcp <extra...>` in `dir`.
    fn spawn(dir: &Path, global: &[&str], extra: &[&str]) -> Client {
        let mut child = Command::new(BIN)
            .current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(global)
            .arg("mcp")
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn katalogr mcp");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let stderr_pipe = child.stderr.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let sink = stderr.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr_pipe).lines() {
                let Ok(line) = line else { break };
                let mut s = sink.lock().unwrap();
                s.push_str(&line);
                s.push('\n');
            }
        });
        Client {
            child,
            stdin,
            lines,
            stderr,
        }
    }

    fn send_raw(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    fn send(&mut self, v: Value) {
        self.send_raw(&v.to_string());
    }

    /// The next stdout line, parsed; panics after 5 s.
    fn recv(&mut self) -> Value {
        self.recv_within(Duration::from_secs(5))
            .expect("no message within 5 s")
    }

    fn recv_within(&mut self, d: Duration) -> Option<Value> {
        let line = self.recv_line(d)?;
        Some(serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}")))
    }

    fn recv_line(&mut self, d: Duration) -> Option<String> {
        match self.lines.recv_timeout(d) {
            Ok(l) => Some(l),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => panic!("server closed stdout"),
        }
    }

    /// Nothing arrives on stdout for `d`.
    fn expect_silence(&mut self, d: Duration) {
        if let Some(extra) = self.recv_line(d) {
            panic!("unexpected message: {extra}");
        }
    }

    /// `initialize` with `version`, then `notifications/initialized`; returns the result.
    fn initialize(&mut self, version: &str) -> Value {
        self.send(
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": version, "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"}}}),
        );
        let r = self.recv();
        assert_eq!(r["id"], 0, "{r}");
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        r
    }

    /// A `tools/call`; returns the response with the matching id.
    fn call(&mut self, id: u64, tool: &str, args: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": tool, "arguments": args}}));
        loop {
            let r = self.recv();
            if r["id"] == id {
                return r;
            }
        }
    }

    /// Close stdin and wait (up to 10 s) for the exit status.
    fn finish(&mut self) -> std::process::ExitStatus {
        self.stdin = None;
        let start = Instant::now();
        loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                return s;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "server did not exit"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The text of the last content item of a `tools/call` response.
fn last_text(r: &Value) -> String {
    let content = r["result"]["content"].as_array().expect("content");
    content.last().expect("a content item")["text"]
        .as_str()
        .expect("text")
        .to_string()
}

fn exit_meta(r: &Value) -> i64 {
    r["result"]["_meta"]["katalogr/exit"]
        .as_i64()
        .unwrap_or_else(|| panic!("no katalogr/exit in {r}"))
}

mod mcp {
    use super::*;

    /// R-1. A scripted client: initialize, ping, tools/list and a `find` call; the ids match,
    /// the shapes are right, and the server exits 0 when stdin closes.
    #[test]
    fn round_trip_scripted_client() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let mut c = Client::spawn(&repo, &[], &[]);
        let init = c.initialize("2025-06-18");
        assert_eq!(init["jsonrpc"], "2.0");
        assert_eq!(init["result"]["serverInfo"]["name"], "katalogr");
        assert_eq!(
            init["result"]["serverInfo"]["version"],
            env!("CARGO_PKG_VERSION")
        );
        assert!(
            init["result"]["instructions"].as_str().unwrap().len() <= 300,
            "{init}"
        );
        c.send(json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}));
        let pong = c.recv();
        assert_eq!(pong["id"], 1);
        assert_eq!(pong["result"], json!({}));
        c.send(json!({"jsonrpc": "2.0", "id": "two", "method": "tools/list"}));
        let list = c.recv();
        assert_eq!(list["id"], "two");
        assert!(list["result"]["tools"].as_array().unwrap().len() == 12);
        let found = c.call(3, "find", json!({"text": "target"}));
        assert_eq!(found["result"]["isError"], false, "{found}");
        let out = last_text(&found);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["schema"], 1);
        assert_eq!(v["result"][0]["name"], "app:lib.tx#fn:target");
        assert_eq!(found["result"]["structuredContent"], v);
        assert_eq!(exit_meta(&found), 0);
        assert!(c.finish().success());
    }

    /// R-1. A known version is echoed; an unknown one gets the server's latest.
    #[test]
    fn negotiates_protocol_version() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let mut c = Client::spawn(&repo, &[], &[]);
        let r = c.initialize("2025-06-18");
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        drop(c);
        let mut c = Client::spawn(&repo, &[], &[]);
        let r = c.initialize("1999-01-01");
        assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
    }

    /// R-1. Stdout carries protocol messages only, over a session that includes a stale
    /// answer; the warning rides in `_meta`, not on stdout outside a message.
    #[test]
    fn stdout_carries_only_protocol() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        // A change the index has not seen yet: reads answer stale.
        std::fs::write(repo.join("late.tx"), "sym fn late\n").unwrap();
        ok(&repo, &["notify", "late.tx"]);
        let mut c = Client::spawn(&repo, &["--consistency", "stale-ok"], &[]);
        c.initialize("2025-06-18");
        c.send_raw("this is not json");
        c.send(json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}));
        c.send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "find", "arguments": {"text": "target"}}}));
        c.send(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "status", "arguments": {}}}));
        let mut lines = Vec::new();
        for _ in 0..3 {
            lines.push(c.recv_line(Duration::from_secs(5)).expect("a response"));
        }
        assert!(c.finish().success());
        for l in &lines {
            let v: Value = serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}"));
            assert_eq!(v["jsonrpc"], "2.0", "{l}");
            assert!(v.get("result").is_some() || v.get("error").is_some(), "{l}");
        }
        let found: Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(found["result"]["isError"], false, "{found}");
        let warnings = &found["result"]["_meta"]["katalogr/warnings"];
        assert!(
            warnings.to_string().contains("answer may be stale"),
            "the stale notice rides in _meta: {found}"
        );
        assert!(
            !c.stderr().contains("jsonrpc"),
            "protocol messages on stderr: {}",
            c.stderr()
        );
    }

    /// R-2. Exactly the twelve core tools, in order, and the `tools` capability only.
    #[test]
    fn tools_list_is_the_core_queries() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let mut c = Client::spawn(&repo, &[], &[]);
        let init = c.initialize("2025-06-18");
        let caps = init["result"]["capabilities"].as_object().unwrap();
        assert_eq!(caps.keys().collect::<Vec<_>>(), ["tools"], "{init}");
        c.send(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}));
        let list = c.recv();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "find", "refs", "callers", "explain", "show", "path", "map", "search", "dangling",
                "why", "index", "status"
            ]
        );
        for banned in [
            "replace", "notify", "hooks", "claude", "skill", "project", "kindex",
        ] {
            assert!(!names.contains(&banned), "{banned} must not be a tool");
        }
        // Every tool's schema is a closed object.
        for t in list["result"]["tools"].as_array().unwrap() {
            assert_eq!(t["inputSchema"]["type"], "object");
            assert_eq!(t["inputSchema"]["additionalProperties"], false);
        }
    }

    /// R-9. Descriptions are at most 100 characters, the list at most 6144 bytes; with
    /// `--output-schemas` each query tool carries its `schema/v1` file.
    #[test]
    fn tools_list_within_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let mut c = Client::spawn(&repo, &[], &[]);
        c.initialize("2025-06-18");
        c.send(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}));
        let list = c.recv();
        let result = list["result"].to_string();
        assert!(result.len() <= 6144, "tools/list is {} bytes", result.len());
        for t in list["result"]["tools"].as_array().unwrap() {
            let d = t["description"].as_str().unwrap();
            assert!(d.chars().count() <= 100, "{}: {d}", t["name"]);
            assert!(t.get("outputSchema").is_none(), "{}", t["name"]);
        }
        drop(c);

        let mut c = Client::spawn(&repo, &[], &["--output-schemas"]);
        c.initialize("2025-06-18");
        c.send(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}));
        let list = c.recv();
        let mut with_schema = 0;
        for t in list["result"]["tools"].as_array().unwrap() {
            let name = t["name"].as_str().unwrap();
            if matches!(name, "index" | "status") {
                assert!(t.get("outputSchema").is_none(), "{name}");
                continue;
            }
            let file = schema_dir().join(format!("{name}.schema.json"));
            let want: Value =
                serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
            assert_eq!(t["outputSchema"], want, "{name}");
            with_schema += 1;
        }
        assert_eq!(with_schema, 10);
        // A client on an older protocol gets none.
        drop(c);
        let mut c = Client::spawn(&repo, &[], &["--output-schemas"]);
        c.initialize("2025-03-26");
        c.send(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}));
        let list = c.recv();
        for t in list["result"]["tools"].as_array().unwrap() {
            assert!(t.get("outputSchema").is_none(), "{}", t["name"]);
        }
    }

    /// R-8. Every tool is also a CLI command.
    #[test]
    fn every_tool_has_a_cli_command() {
        let tmp = tempfile::tempdir().unwrap();
        for name in [
            "find", "refs", "callers", "explain", "show", "path", "map", "search", "dangling",
            "why", "index", "status",
        ] {
            let out = katalogr(tmp.path(), &[name, "--help"]);
            assert!(out.status.success(), "{name} --help: {out:?}");
        }
    }

    fn schema_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/v1")
    }

    /// A small JSON Schema validator for the subset the `schema/v1` files use (the one in
    /// `tests/cli.rs`).
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

    /// The tool calls of the CLI cases: `(tool, arguments, schema name, CLI args)`.
    type Case = (&'static str, Value, &'static str, Vec<&'static str>);

    fn cases() -> Vec<Case> {
        vec![
            (
                "find",
                json!({"text": "call"}),
                "find",
                vec!["find", "call"],
            ),
            (
                "refs",
                json!({"symbol": "target"}),
                "refs",
                vec!["refs", "target"],
            ),
            (
                "callers",
                json!({"symbol": "target", "depth": 3}),
                "callers",
                vec!["callers", "target", "--depth", "3"],
            ),
            (
                "why",
                json!({"location": "user.tx:1"}),
                "why",
                vec!["why", "user.tx:1"],
            ),
            (
                "why",
                json!({"location": "user.tx:3"}),
                "why",
                vec!["why", "user.tx:3"],
            ),
            ("map", json!({}), "map", vec!["map"]),
            (
                "map",
                json!({"budget": 30}),
                "map",
                vec!["map", "--budget", "30"],
            ),
            // Ambiguous candidates are an answer of the `find` shape.
            (
                "refs",
                json!({"symbol": "dup"}),
                "find",
                vec!["refs", "dup"],
            ),
            (
                "show",
                json!({"symbol": "target"}),
                "show",
                vec!["show", "target"],
            ),
            (
                "show",
                json!({"symbol": "target", "signature": true}),
                "show",
                vec!["show", "target", "--signature"],
            ),
            (
                "search",
                json!({"terms": "caller"}),
                "search",
                vec!["search", "caller"],
            ),
            (
                "path",
                json!({"from": "top", "to": "target"}),
                "path",
                vec!["path", "top", "target"],
            ),
            (
                "explain",
                json!({"symbol": "target"}),
                "explain",
                vec!["explain", "target"],
            ),
            (
                "explain",
                json!({"symbol": "caller_a", "budget": 20}),
                "explain",
                vec!["explain", "caller_a", "--budget", "20"],
            ),
        ]
    }

    /// R-3. Every answer, text and `structuredContent`, validates against its `schema/v1`
    /// file (ADR-0023 compliance); the candidates of an ambiguous call validate as `find`.
    #[test]
    fn output_validates_against_schema_v1() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let mut c = Client::spawn(&repo, &[], &[]);
        c.initialize("2025-06-18");
        for (i, (tool, args, schema_name, cli)) in cases().into_iter().enumerate() {
            let schema: Value = serde_json::from_str(
                &std::fs::read_to_string(schema_dir().join(format!("{schema_name}.schema.json")))
                    .unwrap(),
            )
            .unwrap();
            let r = c.call(i as u64 + 1, tool, args);
            let text: Value = serde_json::from_str(&last_text(&r))
                .unwrap_or_else(|e| panic!("{cli:?}: {e}: {r}"));
            validate(&schema, &schema, &text, "$").unwrap_or_else(|e| panic!("{cli:?}: {e}\n{r}"));
            assert_eq!(text["schema"], 1);
            if r["result"]["isError"] == false {
                assert_eq!(r["result"]["structuredContent"], text, "{cli:?}");
            }
        }
    }

    /// R-3. For every tool, with and without `budget`, the text is the CLI's `--json` stdout
    /// and the exit meta is the CLI's exit code.
    #[test]
    fn output_matches_cli_json() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let mut tools: Vec<(&str, Value, Vec<String>)> = Vec::new();
        let base: Vec<(&str, Value, Vec<&str>)> = vec![
            ("find", json!({"text": "call"}), vec!["find", "call"]),
            ("refs", json!({"symbol": "target"}), vec!["refs", "target"]),
            (
                "callers",
                json!({"symbol": "target", "depth": 2}),
                vec!["callers", "target", "--depth", "2"],
            ),
            (
                "explain",
                json!({"symbol": "caller_a"}),
                vec!["explain", "caller_a"],
            ),
            (
                "show",
                json!({"symbol": "target", "signature": true}),
                vec!["show", "target", "--signature"],
            ),
            (
                "path",
                json!({"from": "top", "to": "target"}),
                vec!["path", "top", "target"],
            ),
            ("map", json!({}), vec!["map"]),
            (
                "search",
                json!({"terms": "caller", "limit": 5}),
                vec!["search", "caller", "--limit", "5"],
            ),
            ("dangling", json!({}), vec!["dangling"]),
            (
                "why",
                json!({"location": "user.tx:1"}),
                vec!["why", "user.tx:1"],
            ),
        ];
        for (tool, args, cli) in base {
            tools.push((
                tool,
                args.clone(),
                cli.iter().map(|s| s.to_string()).collect(),
            ));
            let mut with = args.clone();
            with["budget"] = json!(25);
            let mut cli_b: Vec<String> = cli.iter().map(|s| s.to_string()).collect();
            cli_b.extend(["--budget".to_string(), "25".to_string()]);
            tools.push((tool, with, cli_b));
        }
        tools.push(("status", json!({}), vec!["status".to_string()]));
        tools.push(("index", json!({}), vec!["index".to_string()]));

        let mut c = Client::spawn(&repo, &[], &[]);
        c.initialize("2025-06-18");
        for (i, (tool, args, cli)) in tools.iter().enumerate() {
            let mut full = vec!["--json"];
            full.extend(cli.iter().map(String::as_str));
            let (code, out, _) = run_raw(&repo, &full);
            let r = c.call(i as u64 + 1, tool, args.clone());
            let (mut got, mut want) = (last_text(&r), out.trim_end_matches('\n').to_string());
            if *tool == "index" {
                // Every `index` moves the watermark on; the rest of the line is the same.
                let mark = |s: &str| {
                    let v: Value = serde_json::from_str(s).unwrap();
                    assert!(v["watermark"].as_u64().is_some(), "{s}");
                    s.replace(
                        &format!("\"watermark\":{}", v["watermark"]),
                        "\"watermark\":0",
                    )
                };
                (got, want) = (mark(&got), mark(&want));
            }
            assert_eq!(got, want, "{tool} {args}: text differs from `{full:?}`");
            assert_eq!(exit_meta(&r), code as i64, "{tool} {args}");
        }
    }

    /// R-4. The exit-code table of the CLI, through MCP.
    #[test]
    fn errors_mirror_cli_exit_codes() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        std::fs::write(repo.join("ghost.tx"), "sym fn g\nref g links-to ns:ghost\n").unwrap();
        ok(&repo, &["index"]);
        let mut c = Client::spawn(&repo, &[], &[]);
        c.initialize("2025-06-18");

        // Ambiguous: a tool error, exit 3, the message first and the candidates second.
        let r = c.call(1, "refs", json!({"symbol": "dup"}));
        assert_eq!(r["result"]["isError"], true, "{r}");
        assert_eq!(exit_meta(&r), 3);
        let content = r["result"]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2, "{r}");
        assert_eq!(content[0]["text"], "ambiguous: 2 candidates");
        let (code, out, err) = run_raw(&repo, &["--json", "refs", "dup"]);
        assert_eq!(code, 3);
        assert_eq!(content[1]["text"], out.trim_end());
        assert_eq!(err.trim(), "katalogr: ambiguous: 2 candidates");

        // No match: exit 4, the CLI's message.
        let r = c.call(2, "find", json!({"text": "zzz"}));
        assert_eq!(r["result"]["isError"], true, "{r}");
        assert_eq!(exit_meta(&r), 4);
        let (code, _, err) = run_raw(&repo, &["--json", "find", "zzz"]);
        assert_eq!(code, 4);
        assert_eq!(
            last_text(&r),
            err.trim().strip_prefix("katalogr: ").unwrap()
        );

        // A bad argument type, a missing argument, an unknown argument and a bad number.
        for (id, tool, args, want) in [
            (3, "find", json!({"text": 5}), "find: text: expected string"),
            (4, "find", json!({}), "find: text: required"),
            (
                5,
                "find",
                json!({"text": "a", "bogus": 1}),
                "find: bogus: unknown argument",
            ),
            (
                6,
                "callers",
                json!({"symbol": "a", "depth": 0}),
                "callers: depth: must be >= 1",
            ),
            (
                7,
                "index",
                json!({"since": "HEAD", "full": true}),
                "index: since conflicts with full",
            ),
        ] {
            let r = c.call(id, tool, args);
            assert_eq!(r["result"]["isError"], true, "{r}");
            assert_eq!(exit_meta(&r), 2, "{r}");
            assert_eq!(last_text(&r), want);
        }

        // A usage error of the facade is exit 2 as well (a location without a line).
        let r = c.call(8, "why", json!({"location": "nolinehere"}));
        assert_eq!(r["result"]["isError"], true, "{r}");
        assert_eq!(exit_meta(&r), 2);
        let (code, _, _) = run_raw(&repo, &["why", "nolinehere"]);
        assert_eq!(code, 2);

        // `dangling` with findings is an answer, not a failure: exit 1, isError false.
        let r = c.call(9, "dangling", json!({}));
        assert_eq!(r["result"]["isError"], false, "{r}");
        assert_eq!(exit_meta(&r), 1);
        let (code, out, _) = run_raw(&repo, &["--json", "dangling"]);
        assert_eq!(code, 1);
        assert_eq!(last_text(&r), out.trim_end());

        // An unknown tool is a JSON-RPC error, and the server keeps serving.
        c.send(json!({"jsonrpc": "2.0", "id": 10, "method": "tools/call",
            "params": {"name": "replace", "arguments": {}}}));
        let r = c.recv();
        assert_eq!(r["id"], 10);
        assert_eq!(r["error"]["code"], -32602, "{r}");
        assert!(r["error"]["message"].as_str().unwrap().contains("replace"));
        let r = c.call(11, "status", json!({}));
        assert_eq!(r["result"]["isError"], false);
    }

    /// R-4. Garbage gets no answer, a non-message gets -32600 with no id, an unknown method
    /// -32601; the server keeps serving after each.
    #[test]
    fn malformed_input_keeps_serving() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let mut c = Client::spawn(&repo, &[], &[]);
        c.initialize("2025-06-18");
        // rmcp 3.5.0 can drop its -32600 reply to well-formed non-message JSON that arrives
        // within a millisecond of the previous response (observed in about a quarter of runs
        // when sent back to back); a short pause makes the cases deterministic.
        let settle = || std::thread::sleep(Duration::from_millis(20));
        let mut ping = 100;
        let mut expect_ping = |c: &mut Client| {
            ping += 1;
            c.send(json!({"jsonrpc": "2.0", "id": ping, "method": "ping"}));
            let r = c.recv();
            assert_eq!(r["id"], ping, "{r}");
        };

        c.send_raw("not json");
        expect_ping(&mut c);

        settle();
        c.send_raw("{}");
        let r = c.recv();
        assert_eq!(r["error"]["code"], -32600, "{r}");
        assert!(r.get("id").is_none_or(Value::is_null), "{r}");
        expect_ping(&mut c);

        settle();
        c.send_raw("[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}]");
        let r = c.recv();
        assert_eq!(r["error"]["code"], -32600, "{r}");
        assert!(r.get("id").is_none_or(Value::is_null), "{r}");
        expect_ping(&mut c);

        c.send(json!({"jsonrpc": "2.0", "id": 7, "method": "nope/x"}));
        let r = c.recv();
        assert_eq!(r["id"], 7);
        assert_eq!(r["error"]["code"], -32601, "{r}");
        expect_ping(&mut c);
    }

    /// R-11. A 9 MiB line gets -32600 with no id, memory stays bounded, and the next ping
    /// answers.
    #[test]
    fn oversized_line_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let mut c = Client::spawn(&repo, &[], &[]);
        c.initialize("2025-06-18");
        let big = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"params\":{{\"pad\":\"{}\"}}}}",
            "x".repeat(9 * 1024 * 1024)
        );
        c.send_raw(&big);
        let r = c.recv();
        assert_eq!(r["error"]["code"], -32600, "{r}");
        assert!(r.get("id").is_none_or(Value::is_null), "{r}");
        c.send(json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}));
        let r = c.recv();
        assert_eq!(r["id"], 2, "{r}");
        #[cfg(target_os = "linux")]
        {
            let status = std::fs::read_to_string(format!("/proc/{}/status", c.child.id())).unwrap();
            let kb: u64 = status
                .lines()
                .find_map(|l| l.strip_prefix("VmHWM:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
                .expect("VmHWM");
            assert!(kb < 64 * 1024, "peak RSS {kb} kB");
        }
    }

    /// A project whose processor lock this process holds, so `index` waits for it.
    struct Locked {
        _tmp: tempfile::TempDir,
        repo: PathBuf,
        _lock: std::fs::File,
    }

    fn locked_project() -> Locked {
        use fs4::FileExt;
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = setup(tmp.path());
        let lock = kx.join("projects/demo/.cache/processor.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock)
            .unwrap();
        FileExt::try_lock(&file).expect("take the processor lock");
        Locked {
            _tmp: tmp,
            repo,
            _lock: file,
        }
    }

    /// R-5. A request cancelled while queued or running gets no response; a ping still does.
    #[test]
    fn cancelled_request_gets_no_response() {
        let p = locked_project();
        let mut c = Client::spawn(&p.repo, &["--timeout", "1"], &[]);
        c.initialize("2025-06-18");
        c.send(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "index", "arguments": {}}}));
        c.send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "find", "arguments": {"text": "x"}}}));
        std::thread::sleep(Duration::from_millis(200));
        for id in [2, 1] {
            c.send(
                json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                "params": {"requestId": id, "reason": "test"}}),
            );
        }
        c.send(json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}));
        let r = c.recv();
        assert_eq!(r["id"], 3, "{r}");
        // The running index call ends within its 1 s timeout; nothing is sent for 1 or 2.
        c.expect_silence(Duration::from_secs(2));
        c.send(json!({"jsonrpc": "2.0", "id": 4, "method": "ping"}));
        assert_eq!(c.recv()["id"], 4);
    }

    /// R-6. With a progress token, notifications with that token and a rising `progress`
    /// arrive until the response; without a token, none do.
    #[test]
    fn progress_while_waiting() {
        let p = locked_project();
        let mut c = Client::spawn(&p.repo, &["--timeout", "0.5"], &["--progress-ms", "50"]);
        c.initialize("2025-06-18");
        c.send(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "index", "arguments": {}, "_meta": {"progressToken": "p"}}}));
        let mut progress = Vec::new();
        let response = loop {
            let m = c.recv();
            if m["method"] == "notifications/progress" {
                assert_eq!(m["params"]["progressToken"], "p", "{m}");
                progress.push(m["params"]["progress"].as_f64().unwrap());
            } else {
                break m;
            }
        };
        assert_eq!(response["id"], 1, "{response}");
        assert_eq!(exit_meta(&response), 5, "{response}");
        assert!(progress.len() >= 3, "progress {progress:?}");
        assert!(
            progress.windows(2).all(|w| w[1] > w[0]),
            "not increasing: {progress:?}"
        );

        c.send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "index", "arguments": {}}}));
        let m = c.recv();
        assert_eq!(m["id"], 2, "no progress without a token: {m}");
    }

    /// R-7. `claude sync`, `skill install` and `hooks install` write nothing about MCP
    /// (ADR-0023 compliance).
    #[test]
    fn claude_wiring_never_registers_mcp() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let (_kx, repo) = setup(&base);
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .current_dir(&repo)
                .args(args)
                .output()
                .expect("run git");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        let wire = |args: &[&str]| {
            let out = Command::new(BIN)
                .current_dir(&repo)
                .env("HOME", &home)
                .env_remove("KATALOGR_KINDEX")
                .env_remove("KATALOGR_PROJECT")
                .args(args)
                .output()
                .expect("run katalogr");
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        wire(&["claude", "sync"]);
        wire(&["skill", "install", "--scope", "repo"]);
        wire(&["hooks", "install"]);

        let mut files = Vec::new();
        walk(&repo, &mut files);
        walk(&home, &mut files);
        assert!(!files.is_empty());
        let tmp_name = base.to_string_lossy().to_string();
        for f in &files {
            let text = String::from_utf8_lossy(&std::fs::read(f).unwrap()).to_lowercase();
            // The random temp directory name appears in absolute paths; leave it out.
            let text = text.replace(&tmp_name.to_lowercase(), "");
            assert!(!text.contains("mcp"), "{} mentions mcp", f.display());
        }
        assert!(!repo.join(".mcp.json").exists());
        assert!(!home.join(".mcp.json").exists());
        assert!(
            !home.join(".claude.json").exists() || {
                !std::fs::read_to_string(home.join(".claude.json"))
                    .unwrap()
                    .contains("mcpServers")
            }
        );
    }

    /// Every file under `dir`, except git's object store and index (binary, not written by
    /// katalogr).
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.ends_with(".git/objects") {
                    continue;
                }
                walk(&p, out);
            } else if !p.ends_with(".git/index") {
                out.push(p);
            }
        }
    }

    /// R-12. The README's setup snippets parse in their hosts' formats and launch
    /// `katalogr mcp`.
    #[test]
    fn setup_snippets_parse() {
        let readme =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md"))
                .unwrap();
        let section = readme
            .split("## Other agent hosts (MCP)")
            .nth(1)
            .expect("README has an `Other agent hosts (MCP)` section");
        let section = section.split("\n## ").next().unwrap();
        let mut blocks: Vec<(String, String)> = Vec::new();
        let mut lang = None::<String>;
        let mut body = String::new();
        for line in section.lines() {
            if let Some(rest) = line.strip_prefix("```") {
                match lang.take() {
                    Some(l) => blocks.push((l, std::mem::take(&mut body))),
                    None => lang = Some(rest.trim().to_string()),
                }
            } else if lang.is_some() {
                body.push_str(line);
                body.push('\n');
            }
        }
        let langs: Vec<&str> = blocks.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(langs, ["toml", "json", "json"], "{section}");

        let codex: toml::Table = blocks[0].1.parse().expect("the Codex block parses as TOML");
        let server = &codex["mcp_servers"]["katalogr"];
        assert_eq!(server["command"].as_str(), Some("katalogr"));
        assert!(
            server["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a.as_str() == Some("mcp"))
        );
        for (_, body) in &blocks[1..] {
            let v: Value = serde_json::from_str(body).expect("an editor block parses as JSON");
            let server = &v["mcpServers"]["katalogr"];
            assert_eq!(server["command"], "katalogr", "{body}");
            assert!(
                server["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|a| a == "mcp"),
                "{body}"
            );
        }
    }
}

mod bench {
    use super::*;

    fn p95(mut v: Vec<Duration>) -> Duration {
        v.sort();
        v[(v.len() * 95).div_ceil(100) - 1]
    }

    /// R-10. `initialize` within 100 ms of spawn; the p95 of 100 `find` calls in one session
    /// does not exceed the p95 of 100 `katalogr --json find` processes. Run in release:
    /// `cargo test --release -p katalogr --test mcp -- --ignored bench::mcp_session`.
    #[test]
    #[ignore = "benchmark: run in release with --ignored"]
    fn mcp_session_vs_cli_processes() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, repo) = query_project(tmp.path());
        let start = Instant::now();
        let mut c = Client::spawn(&repo, &[], &[]);
        c.initialize("2025-06-18");
        let init = start.elapsed();
        let mut mcp = Vec::new();
        for i in 0..100 {
            let t = Instant::now();
            let r = c.call(i + 1, "find", json!({"text": "target"}));
            mcp.push(t.elapsed());
            assert_eq!(r["result"]["isError"], false, "{r}");
        }
        let mut cli = Vec::new();
        for _ in 0..100 {
            let t = Instant::now();
            let (code, _, _) = run_raw(&repo, &["--json", "find", "target"]);
            cli.push(t.elapsed());
            assert_eq!(code, 0);
        }
        let (m, l) = (p95(mcp), p95(cli));
        println!("initialize {init:?}; MCP p95 {m:?}; CLI process p95 {l:?}");
        assert!(
            init <= Duration::from_millis(100),
            "initialize took {init:?}"
        );
        assert!(m <= l, "MCP p95 {m:?} exceeds CLI p95 {l:?}");
    }

    fn musl_installed() -> bool {
        let out = Command::new("rustc").args(["--print", "sysroot"]).output();
        let Ok(out) = out else { return false };
        let sysroot = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Path::new(&sysroot)
            .join("lib/rustlib/x86_64-unknown-linux-musl")
            .is_dir()
    }

    /// R-13. The `mcp` feature adds at most 3.5 MiB to the unstripped x86_64 musl release
    /// binary: default features minus `--no-default-features`.
    #[test]
    #[ignore = "benchmark: builds the release binary twice"]
    fn mcp_binary_size_within_budget() {
        const TARGET: &str = "x86_64-unknown-linux-musl";
        if !musl_installed() {
            eprintln!("skipped: the {TARGET} target is not installed");
            return;
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target_dir = tempfile::tempdir().unwrap();
        let build = |extra: &[&str]| -> u64 {
            let status = Command::new(env!("CARGO"))
                .current_dir(&root)
                .env("CARGO_TARGET_DIR", target_dir.path())
                .env_remove("CARGO_ENCODED_RUSTFLAGS")
                .args([
                    "build",
                    "-p",
                    "katalogr",
                    "--release",
                    "--locked",
                    "--target",
                    TARGET,
                ])
                .args(extra)
                .status()
                .expect("run cargo build");
            assert!(status.success(), "cargo build {extra:?} failed");
            std::fs::metadata(target_dir.path().join(TARGET).join("release/katalogr"))
                .unwrap()
                .len()
        };
        let with = build(&[]);
        let without = build(&["--no-default-features"]);
        let diff = with as i64 - without as i64;
        println!("with mcp {with} B; without {without} B; difference {diff} B");
        assert!(
            diff <= 3_670_016,
            "the mcp feature adds {diff} B (budget 3,670,016)"
        );
    }
}
