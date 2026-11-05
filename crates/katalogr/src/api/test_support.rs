//! Unit-test support: a running server over test ops and a small HTTP client on `std::net`.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use katalogr_dist::{Options, Overrides};

use super::guard::Authorizer;
use super::ops::{Args, Call, Op};
use super::{ApiConfig, run_with};
use crate::exit::ExitCode;
use crate::render::json::JsonAnswer;

fn answer(exit: ExitCode, stdout: &str) -> JsonAnswer {
    JsonAnswer {
        exit,
        stdout: Some(stdout.to_string()),
        message: None,
        warnings: Vec::new(),
        diagnostic: Vec::new(),
    }
}

fn ping(_: &Path, _: &Overrides, _: &Args, _: &Options) -> JsonAnswer {
    answer(ExitCode::Ok, r#"{"schema":1,"result":"pong"}"#)
}

fn boom(_: &Path, _: &Overrides, _: &Args, _: &Options) -> JsonAnswer {
    panic!("test op panics");
}

fn slow(_: &Path, _: &Overrides, _: &Args, _: &Options) -> JsonAnswer {
    thread::sleep(Duration::from_millis(600));
    answer(ExitCode::Ok, r#"{"schema":1,"result":"slow"}"#)
}

const fn op(name: &'static str, f: fn(&Path, &Overrides, &Args, &Options) -> JsonAnswer) -> Op {
    Op {
        name,
        summary: "test op",
        params: &[],
        schema: None,
        call: Call::Free(f),
    }
}

/// Routes that need no project: `ping`, `boom` (panics) and `slow` (0.6 s).
pub static TEST_OPS: &[Op] = &[op("ping", ping), op("boom", boom), op("slow", slow)];

/// Loopback on port 0, with `workers` and `queue`.
pub fn quick_config(workers: usize, queue: usize) -> ApiConfig {
    ApiConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        workers,
        queue,
        process: true,
        cors_origin: None,
        overrides: Overrides::default(),
        start_dir: std::env::temp_dir(),
    }
}

/// A server on its own thread; dropping it stops it.
pub struct Running {
    pub addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

pub fn start(cfg: ApiConfig, auth: Arc<dyn Authorizer>, ops: &'static [Op]) -> Running {
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let flag = stop.clone();
    let handle = thread::spawn(move || {
        run_with(cfg, auth, &flag, ops, Some(tx)).expect("serve");
    });
    let addr = rx.recv_timeout(Duration::from_secs(10)).expect("listening");
    Running {
        addr,
        stop,
        handle: Some(handle),
    }
}

#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<&str> {
        super::http::header(&self.headers, name)
    }
}

/// `GET path` with `extra` headers; `Host` is the server's own unless `extra` sets one.
pub fn get(addr: SocketAddr, path: &str, extra: &[(&str, &str)]) -> Reply {
    let mut req = format!("GET {path} HTTP/1.0\r\n");
    if !extra.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        req.push_str(&format!("Host: {addr}\r\n"));
    }
    for (k, v) in extra {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("Connection: close\r\n\r\n");
    raw(addr, &req)
}

/// Send `request` as it is and read the answer to the end.
pub fn raw(addr: SocketAddr, request: &str) -> Reply {
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    s.write_all(request.as_bytes()).expect("write");
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
