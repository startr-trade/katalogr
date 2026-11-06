//! The stateless HTTP/JSON API server (FEAT-049, ADR-0010): a thin wrapper over katalogr-dist,
//! built like `mcp/`. Request parsing and answers are in [`http`], authorisation in [`guard`],
//! the routes in [`ops`] and the OpenAPI document in [`openapi`].
//!
//! No request state and no open handle survive a request: every request resolves the project,
//! makes one facade call and closes. `wait` reads queue behind a per-context FIFO gate.

pub mod guard;
pub mod http;
pub mod openapi;
pub mod ops;
#[cfg(test)]
pub mod test_support;
#[cfg(feature = "web")]
pub mod web;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TrySendError};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use katalogr_dist::{Consistency, DistError, Katalogr, Options, Overrides};

use self::guard::{Authorizer, check_host, check_listen, cors_allow_origin};
use self::http::{HttpError, RequestHead, header, parse_target, redact, status_of};
use self::ops::{Args, Call, OPS, Op, find_op};
use crate::exit::ExitCode;
use crate::render::json::{self as render_json, JsonAnswer};

/// How the server runs.
pub struct ApiConfig {
    /// Default `127.0.0.1:7878`.
    pub listen: SocketAddr,
    pub workers: usize,
    /// Requests that may wait for a worker; a full queue answers 503 at once.
    pub queue: usize,
    /// False: a read never processes the queue (the hosted form).
    pub process: bool,
    pub cors_origin: Option<String>,
    /// Kindex and project, as the CLI.
    pub overrides: Overrides,
    pub start_dir: PathBuf,
    /// Also answer `GET /` and `GET /assets/<name>` (`katalogr serve`, FEAT-033).
    pub web: bool,
    /// The command name, for the start-up and request log lines.
    pub name: &'static str,
}

/// Provisional default (FEAT-049 T9 replaces it from the benchmark).
pub const DEFAULT_QUEUE: usize = 64;

/// `min(cores, 8)`.
pub fn default_workers() -> usize {
    thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(8)
}

/// A ticket lock: waiters enter in arrival order.
#[derive(Default)]
struct Gate {
    state: Mutex<(u64, u64)>,
    turn: Condvar,
}

struct Ticket<'a>(&'a Gate);

impl Gate {
    fn enter(&self) -> Ticket<'_> {
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mine = s.0;
        s.0 += 1;
        while s.1 != mine {
            s = self.turn.wait(s).unwrap_or_else(PoisonError::into_inner);
        }
        Ticket(self)
    }
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        let mut s = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        s.1 += 1;
        self.0.turn.notify_all();
    }
}

/// What every worker shares.
struct Shared {
    cfg: ApiConfig,
    auth: Arc<dyn Authorizer>,
    ops: &'static [Op],
    /// The bound address, for the Host check.
    bound: SocketAddr,
    gates: Mutex<HashMap<String, Arc<Gate>>>,
}

/// One answer, before it is written to the socket.
#[derive(Debug)]
pub struct Out {
    pub status: u16,
    pub body: String,
    pub headers: Vec<(&'static str, String)>,
}

impl Out {
    fn json(status: u16, body: String) -> Out {
        Out {
            status,
            body,
            headers: vec![("Content-Type", "application/json".to_string())],
        }
    }

    fn error(e: &HttpError) -> Out {
        let mut o = Out::json(e.status, e.body());
        if let Some(r) = e.retry_after() {
            o.headers.push(("Retry-After", r.to_string()));
        }
        o
    }
}

/// Serve until `stop`; the library entry point for embedders and tests (`katalogr api` uses
/// [`run_announcing`]). Refuses a non-loopback `listen` while the only authorizer is `Localhost`.
#[allow(dead_code)]
pub fn run(cfg: ApiConfig, auth: Arc<dyn Authorizer>, stop: &AtomicBool) -> Result<(), DistError> {
    run_with(cfg, auth, stop, OPS, None)
}

/// [`run`], printing the bound address on stderr once it listens.
pub fn run_announcing(
    cfg: ApiConfig,
    auth: Arc<dyn Authorizer>,
    stop: &AtomicBool,
) -> Result<(), DistError> {
    let name = cfg.name;
    let (tx, rx) = mpsc::channel();
    thread::scope(|s| {
        s.spawn(move || {
            if let Ok(addr) = rx.recv() {
                eprintln!("katalogr {name}: listening on http://{addr}");
            }
        });
        run_with(cfg, auth, stop, OPS, Some(tx))
    })
}

/// [`run`] over the routes `ops`; `ready` receives the bound address once the socket listens
/// (so a caller can ask for port 0).
pub fn run_with(
    cfg: ApiConfig,
    auth: Arc<dyn Authorizer>,
    stop: &AtomicBool,
    ops: &'static [Op],
    ready: Option<mpsc::Sender<SocketAddr>>,
) -> Result<(), DistError> {
    check_listen(cfg.listen, auth.as_ref())?;
    let server = tiny_http::Server::http(cfg.listen)
        .map_err(|e| DistError::Usage(format!("cannot listen on {}: {e}", cfg.listen)))?;
    let bound = server
        .server_addr()
        .to_ip()
        .ok_or_else(|| DistError::Usage("the listener has no IP address".to_string()))?;
    let (workers, queue) = (cfg.workers.max(1), cfg.queue.max(1));
    let shared = Arc::new(Shared {
        cfg,
        auth,
        ops,
        bound,
        gates: Mutex::new(HashMap::new()),
    });
    let (tx, rx) = mpsc::sync_channel::<tiny_http::Request>(queue);
    let rx = Arc::new(Mutex::new(rx));
    let handles: Vec<_> = (0..workers)
        .map(|_| {
            let (rx, shared) = (rx.clone(), shared.clone());
            thread::spawn(move || {
                loop {
                    let next = rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
                    match next {
                        Ok(req) => serve(&shared, req),
                        Err(_) => break,
                    }
                }
            })
        })
        .collect();
    if let Some(r) = ready {
        let _ = r.send(bound);
    }
    while !stop.load(Ordering::Relaxed) {
        match server.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(req)) => match tx.try_send(req) {
                Ok(()) => {}
                Err(TrySendError::Full(req)) => {
                    let e = HttpError::new(503, "busy", "the request queue is full");
                    respond(req, Out::error(&e));
                }
                Err(TrySendError::Disconnected(_)) => break,
            },
            Ok(None) => {}
            Err(_) => break,
        }
    }
    drop(tx);
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

fn respond(req: tiny_http::Request, out: Out) {
    let mut resp = tiny_http::Response::from_string(out.body).with_status_code(out.status);
    for (k, v) in out.headers {
        if let Ok(h) = tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()) {
            resp.add_header(h);
        }
    }
    let _ = req.respond(resp);
}

/// One request: read its head, dispatch, write the answer, log one line.
fn serve(shared: &Shared, req: tiny_http::Request) {
    let started = Instant::now();
    let method = req.method().as_str().to_string();
    let url = req.url().to_string();
    let headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .map(|h| {
            (
                h.field.as_str().as_str().to_string(),
                h.value.as_str().to_string(),
            )
        })
        .collect();
    let head_len = method.len()
        + url.len()
        + headers
            .iter()
            .map(|(k, v)| k.len() + v.len() + 4)
            .sum::<usize>();
    let out = match catch_unwind(AssertUnwindSafe(|| {
        dispatch(shared, &method, &url, &headers, head_len)
    })) {
        Ok(o) => o,
        Err(_) => Out::error(&HttpError::new(500, "internal", "internal error")),
    };
    // The op and status only: never the query text, since a search term may be sensitive.
    let op = url
        .split('?')
        .next()
        .and_then(|p| p.strip_prefix(http::PREFIX))
        .unwrap_or("-");
    eprintln!(
        "katalogr {}: {method} {op} {} {}ms",
        shared.cfg.name,
        out.status,
        started.elapsed().as_millis()
    );
    respond(req, out);
}

/// The request path: method, limits, authorizer, Host check, route, parameters, one facade call.
fn dispatch(
    shared: &Shared,
    method: &str,
    url: &str,
    headers: &[(String, String)],
    head_len: usize,
) -> Out {
    let mut out = match route(shared, method, url, headers, head_len) {
        Ok(o) => o,
        Err(e) => Out::error(&e),
    };
    let origin = header(headers, "origin");
    if let Some(allow) = cors_allow_origin(shared.cfg.cors_origin.as_deref(), origin) {
        out.headers
            .push(("Access-Control-Allow-Origin", allow.to_string()));
        out.headers.push(("Vary", "Origin".to_string()));
    }
    out
}

fn route(
    shared: &Shared,
    method: &str,
    url: &str,
    headers: &[(String, String)],
    head_len: usize,
) -> Result<Out, HttpError> {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    #[cfg(feature = "web")]
    if shared.cfg.web && !path.starts_with(http::PREFIX) {
        // A page: the same limits, authorizer and Host check as the API, then the bundle.
        http::check_head(method, head_len)?;
        let head = RequestHead {
            method,
            path,
            query,
            headers,
        };
        shared
            .auth
            .check(&head)
            .map_err(|d| HttpError::new(d.status, "denied", d.message))?;
        check_host(shared.bound, head.header("host"))
            .map_err(|d| HttpError::new(d.status, "denied", d.message))?;
        return web::answer(path, headers);
    }
    let target = parse_target(method, url, head_len)?;
    let head = RequestHead {
        method,
        path,
        query,
        headers,
    };
    shared
        .auth
        .check(&head)
        .map_err(|d| HttpError::new(d.status, "denied", d.message))?;
    check_host(shared.bound, head.header("host"))
        .map_err(|d| HttpError::new(d.status, "denied", d.message))?;
    match target.op.as_str() {
        "meta" => {
            ops::Args::parse(&[], &target.params)?;
            let mut info = katalogr_dist::contract::contract_info();
            if shared.cfg.web {
                // Only a process that serves pages says so.
                info.capabilities
                    .push(katalogr_dist::contract::WEB_CAPABILITY.to_string());
                info.capabilities.sort();
            }
            let body = serde_json::to_string(&info).expect("serialise contract");
            Ok(Out::json(200, body))
        }
        "openapi.json" => {
            ops::Args::parse(&[], &target.params)?;
            Ok(Out::json(200, openapi::pretty(shared.ops)))
        }
        name => {
            let op = find_op(shared.ops, name)
                .ok_or_else(|| HttpError::new(404, "not-found", "no such route"))?;
            let args = Args::parse(op.params, &target.params)?;
            Ok(answer_of(shared, op, &args, headers))
        }
    }
}

/// Run the op and turn its answer into a response (status table, `ETag`).
fn answer_of(shared: &Shared, op: &Op, args: &Args, headers: &[(String, String)]) -> Out {
    // A views route knows its ID before any read (FEAT-088): a client that holds it is told so.
    if let Some(inm) = header(headers, "if-none-match")
        && op.name.starts_with("views/")
    {
        let mut overrides = shared.cfg.overrides.clone();
        overrides.context = args.opt("context").map(str::to_string);
        if let Some(id) = ops::view_id_now(&shared.cfg.start_dir, op, args, &overrides) {
            let etag = format!("\"{id}\"");
            if inm
                .split(',')
                .any(|t| t.trim().trim_start_matches("W/") == etag)
            {
                let mut out = Out::json(304, String::new());
                out.headers.push(("ETag", etag));
                return out;
            }
        }
    }
    let ans = execute(shared, op, args);
    let status = status_of(ans.exit);
    let (body, kind) = match (&ans.stdout, ans.exit) {
        (Some(out), _) => (out.clone(), None),
        (None, ExitCode::Usage) => (String::new(), Some("usage")),
        (None, ExitCode::NoMatch) => (String::new(), Some("no-match")),
        (None, ExitCode::Stale) => (String::new(), Some("stale-file")),
        (None, _) => (String::new(), Some("error")),
    };
    // An answer with findings exits 1 but is still an answer: only an error with nothing on
    // stdout is a 500.
    let status = if ans.stdout.is_some() && ans.exit == ExitCode::Error {
        200
    } else {
        status
    };
    let body = match kind {
        Some(kind) => {
            let msg = ans.message.as_deref().unwrap_or("");
            http::error_body(kind, &redact(msg))
        }
        None => body,
    };
    let mut out = Out::json(status, body);
    out.headers.extend(ans.headers.iter().cloned());
    if status == 200
        && let Some(id) = string_field(&out.body, "view_id")
    {
        let etag = format!("\"{id}\"");
        out.headers.push(("ETag", etag.clone()));
        // 304 only after the read has shown the same ID.
        if let Some(inm) = header(headers, "if-none-match")
            && inm
                .split(',')
                .any(|t| t.trim().trim_start_matches("W/") == etag)
        {
            out.status = 304;
            out.body = String::new();
        }
    }
    out
}

/// The string value of a top-level key in compact JSON, without parsing the document. Quotes
/// inside strings are escaped, so an unescaped `"key":"` is a key.
fn string_field(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let i = json.find(&pat)? + pat.len();
    let end = json[i..].find('"')?;
    Some(json[i..i + end].to_string())
}

fn is_stale(a: &JsonAnswer) -> bool {
    a.exit == ExitCode::Stale
        || a.stdout
            .as_deref()
            .is_some_and(|s| s.contains("\"stale\":true"))
}

/// One facade call, behind the context's gate for a `wait` read, caught if it panics.
fn execute(shared: &Shared, op: &Op, args: &Args) -> JsonAnswer {
    let mode = args.consistency();
    let mut overrides = shared.cfg.overrides.clone();
    overrides.context = args.opt("context").map(str::to_string);
    let timeout = args.float("timeout");
    let base = Options {
        consistency: Some(mode),
        timeout,
        diagnostic: false,
        expand: false,
        at: args.opt("at").map(str::to_string),
        view: None,
        ranking: katalogr_dist::Ranking::Neutral,
    };
    let waits = mode != Consistency::StaleOk;
    let gate;
    let _ticket;
    if waits {
        let key = format!("{:?}", overrides.context);
        gate = shared
            .gates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(key)
            .or_default()
            .clone();
        _ticket = gate.enter();
    }
    let start = shared.cfg.start_dir.as_path();
    let call = |o: &Options| call_op(start, op, args, &overrides, o);
    let run = || {
        if waits && !shared.cfg.process {
            poll_stale(&call, &base, timeout)
        } else {
            call(&base)
        }
    };
    match catch_unwind(AssertUnwindSafe(run)) {
        Ok(a) => a,
        Err(_) => JsonAnswer {
            exit: ExitCode::Error,
            stdout: None,
            message: Some("internal error".to_string()),
            warnings: Vec::new(),
            diagnostic: Vec::new(),
            headers: Vec::new(),
        },
    }
}

fn call_op(
    start: &std::path::Path,
    op: &Op,
    args: &Args,
    overrides: &Overrides,
    o: &Options,
) -> JsonAnswer {
    match op.call {
        Call::Project(f) => match Katalogr::resolve(start, overrides) {
            Ok(k) => f(&k, args, o),
            Err(e) => render_json::error(&e),
        },
        Call::Free(f) => f(start, overrides, args, o),
    }
}

/// With `process: false` a `wait` read never processes: it reads as `stale-ok` until the answer
/// is fresh or `timeout` passes, then answers stale.
fn poll_stale(
    call: &dyn Fn(&Options) -> JsonAnswer,
    base: &Options,
    timeout: Option<f64>,
) -> JsonAnswer {
    let limit = timeout.unwrap_or(katalogr_dist::DEFAULT_TIMEOUT_SECS);
    let deadline = Instant::now() + Duration::from_secs_f64(limit);
    let o = Options {
        consistency: Some(Consistency::StaleOk),
        ..base.clone()
    };
    loop {
        let a = call(&o);
        if !is_stale(&a) || Instant::now() >= deadline {
            return a;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// R-8. A panic in a call is a 500 for that request, and the worker keeps serving.
#[cfg(test)]
#[test]
fn panic_is_500_and_server_lives() {
    use self::test_support::{TEST_OPS, get, quick_config, start};
    let server = start(quick_config(1, 4), Arc::new(guard::Localhost), TEST_OPS);
    let r = get(server.addr, "/api/v1/boom", &[]);
    assert_eq!(r.status, 500, "{}", r.body);
    assert!(r.body.contains("\"error\""), "{}", r.body);
    // The only worker survived.
    let r = get(server.addr, "/api/v1/ping", &[]);
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body.contains("pong"), "{}", r.body);
}

/// R-8. A full queue answers 503 with `Retry-After: 1` at once.
#[cfg(test)]
#[test]
fn full_queue_503() {
    use self::test_support::{TEST_OPS, get, quick_config, start};
    let server = start(quick_config(1, 1), Arc::new(guard::Localhost), TEST_OPS);
    let addr = server.addr;
    let mut clients = Vec::new();
    for _ in 0..4 {
        clients.push(thread::spawn(move || get(addr, "/api/v1/slow", &[])));
        thread::sleep(Duration::from_millis(120));
    }
    let answers: Vec<_> = clients.into_iter().map(|c| c.join().unwrap()).collect();
    let busy: Vec<_> = answers.iter().filter(|r| r.status == 503).collect();
    assert!(!busy.is_empty(), "{answers:?}");
    assert!(
        busy.iter().all(|r| r.header("retry-after") == Some("1")),
        "{busy:?}"
    );
    // The worker and the one queued request still finish.
    assert!(
        answers.iter().filter(|r| r.status == 200).count() >= 2,
        "{answers:?}"
    );
}
