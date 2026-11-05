//! Unit tests of `api::http`; compiled as the crate-level module `api_http` so they print as
//! `api_http::<test>`.

use crate::api::http::*;
use crate::exit::ExitCode;

fn get(url: &str) -> Result<Target, HttpError> {
    parse_target("GET", url, 100)
}

/// R-4.
#[test]
fn status_table() {
    assert_eq!(status_of(ExitCode::Ok), 200);
    assert_eq!(status_of(ExitCode::Stale), 200);
    assert_eq!(status_of(ExitCode::Ambiguous), 409);
    assert_eq!(status_of(ExitCode::PositionUnavailable), 409);
    assert_eq!(status_of(ExitCode::NoMatch), 404);
    assert_eq!(status_of(ExitCode::Usage), 400);
    assert_eq!(status_of(ExitCode::Error), 500);
    // The refusals made before any katalogr call.
    assert_eq!(
        parse_target("POST", "/api/v1/find", 10).unwrap_err().status,
        405
    );
    assert_eq!(get("/nope").unwrap_err().status, 404);
    assert_eq!(get("/api/v1/").unwrap_err().status, 404);
    assert_eq!(get("/api/v1/a/b").unwrap_err().status, 404);
    assert_eq!(
        HttpError::new(503, "busy", "queue full").retry_after(),
        Some("1")
    );
    assert_eq!(HttpError::usage("x").retry_after(), None);
    let v: serde_json::Value =
        serde_json::from_str(&HttpError::new(404, "no-match", "nothing").body()).unwrap();
    assert_eq!(v["schema"], 1);
    assert_eq!(v["error"], "no-match");
    assert_eq!(v["message"], "nothing");
}

/// R-8.
#[test]
fn long_headers_431() {
    assert!(parse_target("GET", "/api/v1/find", MAX_HEAD_BYTES).is_ok());
    let e = parse_target("GET", "/api/v1/find", MAX_HEAD_BYTES + 1).unwrap_err();
    assert_eq!(e.status, 431);
}

/// R-3.
#[test]
fn unknown_param_400() {
    let t = get("/api/v1/find?name=a%20b&context=main&at=x+y").unwrap();
    assert_eq!(t.op, "find");
    assert_eq!(t.params["name"], "a b");
    assert_eq!(t.params["at"], "x y");
    assert!(check_params(&t.params, &["name"]).is_ok());
    let e = check_params(&t.params, &[]).unwrap_err();
    assert_eq!(e.status, 400);
    assert!(e.message.contains("name"), "{}", e.message);
    // A typo of a common parameter is refused too.
    let t = get("/api/v1/find?contxt=main").unwrap();
    assert_eq!(check_params(&t.params, &["name"]).unwrap_err().status, 400);
    // Repeated and malformed parameters.
    assert_eq!(get("/api/v1/find?name=a&name=b").unwrap_err().status, 400);
    assert_eq!(get("/api/v1/find?name=%zz").unwrap_err().status, 400);
    assert_eq!(get("/api/v1/find?name=%ff").unwrap_err().status, 400);
}

/// R-9.
#[test]
fn redact_removes_absolute_paths() {
    let r = redact("cannot open /home/dev/proj/.cache/index.redb: permission denied");
    assert_eq!(r, "cannot open <path>: permission denied");
    assert_eq!(
        redact("bad 'path=/var/lib/x' here"),
        "bad 'path=<path>' here"
    );
    assert_eq!(
        redact(r"read C:\Users\a\b.txt failed"),
        "read <path> failed"
    );
    assert_eq!(
        redact("at (/tmp/a/b) and /etc/x."),
        "at (<path>) and <path>."
    );
    // Relative paths, a lone slash and prose stay.
    assert_eq!(redact("src/lib.rs line 3 / 4"), "src/lib.rs line 3 / 4");
    for m in [
        "cannot open /home/dev/proj/x",
        "root=/srv/data/kx and \"/opt/z\"",
    ] {
        let r = redact(m);
        assert!(
            !r.contains("/home") && !r.contains("/srv") && !r.contains("/opt"),
            "{r}"
        );
    }
}

/// R-8.
#[test]
fn timeout_capped() {
    assert_eq!(capped_timeout("5").unwrap(), 5.0);
    assert_eq!(capped_timeout("0.25").unwrap(), 0.25);
    assert_eq!(capped_timeout("120").unwrap(), 120.0);
    assert_eq!(capped_timeout("100000").unwrap(), MAX_TIMEOUT_SECS);
    for bad in ["-1", "abc", "NaN", "inf", ""] {
        assert_eq!(capped_timeout(bad).unwrap_err().status, 400, "{bad}");
    }
}
