//! A harness that runs the web view's own scripts under a pure-Rust JavaScript engine, with a
//! fake DOM and stubbed Cytoscape and Pele. Test-only: nothing here is shipped.
//!
//! Requests are queued in JavaScript; Rust drains the queue, asks a [`Backend`] and delivers each
//! answer, so no closure crosses into the engine.

use boa_engine::{Context, JsValue, Source};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::Mutex;

/// Answers the page's requests.
pub trait Backend {
    /// The status and body for a GET of `path_and_query`.
    fn get(&self, path_and_query: &str) -> (u16, String);
}

/// Canned answers by exact path and query. An unlisted request answers 404 and is recorded.
#[derive(Default)]
pub struct MapBackend {
    answers: Vec<(String, u16, String)>,
    missed: Mutex<Vec<String>>,
}

impl MapBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds (or, for the same path, replaces) an answer.
    pub fn with(mut self, path: &str, status: u16, body: &str) -> Self {
        self.answers.retain(|(p, _, _)| p != path);
        self.answers
            .push((path.to_string(), status, body.to_string()));
        self
    }

    /// The requests that had no canned answer, in order.
    pub fn missed(&self) -> Vec<String> {
        self.missed.lock().expect("missed lock").clone()
    }
}

impl Backend for MapBackend {
    fn get(&self, path_and_query: &str) -> (u16, String) {
        match self.answers.iter().find(|(p, _, _)| p == path_and_query) {
            Some((_, status, body)) => (*status, body.clone()),
            None => {
                self.missed
                    .lock()
                    .expect("missed lock")
                    .push(path_and_query.to_string());
                (
                    404,
                    serde_json::json!({"error": {"message": format!("no canned answer for {path_and_query}")}})
                        .to_string(),
                )
            }
        }
    }
}

/// A plain HTTP/1.1 GET over `std::net` to a running server.
pub struct HttpBackend {
    pub addr: SocketAddr,
}

impl Backend for HttpBackend {
    fn get(&self, path_and_query: &str) -> (u16, String) {
        let mut s = std::net::TcpStream::connect(self.addr).expect("connect to the server");
        s.set_read_timeout(Some(std::time::Duration::from_secs(30)))
            .expect("read timeout");
        write!(
            s,
            "GET {path_and_query} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: */*\r\n\r\n",
            self.addr
        )
        .expect("send the request");
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).expect("read the answer");
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("a complete header");
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let body = &raw[split + 4..];
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .expect("a status code");
        let chunked = head.lines().any(|l| {
            l.to_ascii_lowercase().starts_with("transfer-encoding:") && l.contains("chunked")
        });
        let body = if chunked {
            dechunk(body)
        } else {
            body.to_vec()
        };
        (status, String::from_utf8_lossy(&body).into_owned())
    }
}

fn dechunk(mut rest: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let Some(eol) = rest.windows(2).position(|w| w == b"\r\n") else {
            return out;
        };
        let size = std::str::from_utf8(&rest[..eol])
            .ok()
            .and_then(|t| usize::from_str_radix(t.split(';').next()?.trim(), 16).ok())
            .unwrap_or(0);
        rest = &rest[eol + 2..];
        if size == 0 || rest.len() < size {
            return out;
        }
        out.extend_from_slice(&rest[..size]);
        rest = rest.get(size + 2..).unwrap_or(&[]);
    }
}

const FAKE_DOM: &str = include_str!("../js/fake_dom.js");
const STUBS: &str = include_str!("../js/stubs.js");
/// The most delivery rounds before the harness gives up: a page that keeps asking is a bug.
const MAX_ROUNDS: usize = 1000;

/// One page: the bundle's own scripts loaded in a fresh engine.
pub struct Page {
    ctx: Context,
}

impl Page {
    /// Loads the fake DOM, stubbed `pele` and `cytoscape`, and the bundle's own scripts in load
    /// order, sets the hash, boots, and answers requests until none is pending.
    pub fn open<B: Backend>(hash: &str, backend: &B) -> Page {
        let mut page = Page {
            ctx: Context::default(),
        };
        page.run(FAKE_DOM);
        page.run(STUBS);
        let views = serde_json::to_string(katalogr_web::VIEWS_JSON).expect("manifest as a string");
        page.run(&format!(
            "document.getElementById('kw-views').textContent = {views};"
        ));
        for a in katalogr_web::assets() {
            if a.name.starts_with("kw-") && a.name.ends_with(".js") {
                page.run(a.text);
            }
        }
        page.run(&format!(
            "__setHash({}); if (window.KW && KW.boot) KW.boot();",
            json_str(hash)
        ));
        page.drain(backend);
        page
    }

    /// Sets the hash, fires `hashchange`, and answers requests until none is pending.
    pub fn navigate<B: Backend>(&mut self, hash: &str, backend: &B) {
        self.run(&format!(
            "__setHash({}); __fireHashChange();",
            json_str(hash)
        ));
        self.drain(backend);
    }

    /// Answers requests the page queued by itself (for example after a script ran), until none
    /// is pending.
    pub fn settle<B: Backend>(&mut self, backend: &B) {
        self.drain(backend);
    }

    /// All text under `<main>`, in document order, one text node per line.
    pub fn text(&mut self) -> String {
        self.eval_string("__mainText()")
    }

    /// The element with this `data-kw` value exists.
    pub fn has(&mut self, data_kw: &str) -> bool {
        self.eval_string(&format!("String(!!__byKw({}))", json_str(data_kw))) == "true"
    }

    /// Fires `click` or `change` on the element with this `data-kw`.
    pub fn fire<B: Backend>(
        &mut self,
        data_kw: &str,
        event: &str,
        value: Option<&str>,
        backend: &B,
    ) {
        let value = value.map_or("null".to_string(), json_str);
        self.run(&format!(
            "__fire({}, {}, {value});",
            json_str(data_kw),
            json_str(event)
        ));
        self.drain(backend);
    }

    /// Every source given to `pele.render`, in order.
    pub fn pele_sources(&mut self) -> Vec<String> {
        self.eval_list("__pele")
    }

    /// Every string assigned to `innerHTML`, in order.
    pub fn markup_writes(&mut self) -> Vec<String> {
        self.eval_list("__markup")
    }

    /// Paths requested so far, in order.
    pub fn requests(&mut self) -> Vec<String> {
        self.eval_list("__requests")
    }

    /// Evaluates an expression and returns it as JSON text, for `KW.core` tests.
    pub fn eval_json(&mut self, expr: &str) -> String {
        self.eval_string(&format!(
            "(function () {{ var v = ({expr}); return v === undefined ? 'null' : JSON.stringify(v); }})()"
        ))
    }

    fn eval_list(&mut self, expr: &str) -> Vec<String> {
        serde_json::from_str(&self.eval_string(&format!("JSON.stringify({expr})")))
            .expect("a list of strings")
    }

    fn run(&mut self, code: &str) -> JsValue {
        match self.ctx.eval(Source::from_bytes(code)) {
            Ok(v) => v,
            Err(e) => panic!("script failed: {e}"),
        }
    }

    fn eval_string(&mut self, code: &str) -> String {
        let v = self.run(code);
        match v.to_string(&mut self.ctx) {
            Ok(s) => s.to_std_string_escaped(),
            Err(e) => panic!("result is not text: {e}"),
        }
    }

    /// Delivers queued requests and runs timers and promise jobs until nothing is pending.
    fn drain<B: Backend>(&mut self, backend: &B) {
        for _ in 0..MAX_ROUNDS {
            if let Err(e) = self.ctx.run_jobs() {
                panic!("job failed: {e}");
            }
            let queued: Vec<serde_json::Value> =
                serde_json::from_str(&self.eval_string("__takeQueue()")).expect("a queue");
            let timers: usize = self
                .eval_string("String(__runTimers())")
                .parse()
                .expect("a count");
            if queued.is_empty() && timers == 0 {
                return;
            }
            for r in queued {
                let id = r["id"].as_u64().expect("a request id");
                let path = r["path"].as_str().expect("a request path");
                let (status, body) = backend.get(path);
                self.run(&format!("__deliver({id}, {status}, {});", json_str(&body)));
            }
        }
        panic!("the page kept requesting after {MAX_ROUNDS} rounds");
    }
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).expect("a string")
}
