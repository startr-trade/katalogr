//! Unit tests of `api::guard`; compiled as the crate-level module `api_guard` so they print as
//! `api_guard::<test>`.

use crate::api::guard::*;
use std::net::SocketAddr;

fn addr(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

fn head(headers: &[(String, String)]) -> RequestHead<'_> {
    RequestHead {
        method: "GET",
        path: "/api/v1/find",
        query: "",
        headers,
    }
}

/// R-7.
#[test]
fn non_loopback_refused() {
    assert!(check_listen(addr("127.0.0.1:7878"), &Localhost).is_ok());
    assert!(check_listen(addr("[::1]:7878"), &Localhost).is_ok());
    for a in ["0.0.0.0:7878", "192.168.1.5:7878", "[::]:7878"] {
        let e = check_listen(addr(a), &Localhost).unwrap_err();
        assert!(matches!(e, katalogr_dist::DistError::Usage(_)), "{a}: {e}");
    }
    // A provider that decides may serve any address.
    struct Provider;
    impl Authorizer for Provider {
        fn check(&self, _: &RequestHead<'_>) -> Result<(), Denied> {
            Ok(())
        }
    }
    assert!(check_listen(addr("0.0.0.0:7878"), &Provider).is_ok());
}

/// R-7.
#[test]
fn bad_host_403() {
    let l = addr("127.0.0.1:7878");
    for ok in [
        "localhost:7878",
        "127.0.0.1:7878",
        "[::1]:7878",
        "LocalHost:7878",
    ] {
        assert!(check_host(l, Some(ok)).is_ok(), "{ok}");
    }
    for bad in [
        "evil.example:7878",
        "localhost.evil.example:7878",
        "localhost:80",
        "localhost",
        "10.0.0.1:7878",
    ] {
        assert_eq!(check_host(l, Some(bad)).unwrap_err().status, 403, "{bad}");
    }
    assert_eq!(check_host(l, None).unwrap_err().status, 403);
    // Off loopback the authorizer decides, not the Host header.
    assert!(check_host(addr("10.0.0.1:7878"), Some("anything")).is_ok());
}

/// R-7.
#[test]
fn provider_can_deny() {
    struct NeedsToken;
    impl Authorizer for NeedsToken {
        fn check(&self, req: &RequestHead<'_>) -> Result<(), Denied> {
            match req.header("authorization") {
                None => Err(Denied::unauthorized("no credentials")),
                Some("Bearer good") => Ok(()),
                Some(_) => Err(Denied::forbidden("not allowed")),
            }
        }
    }
    let h = |v: Option<&str>| -> Vec<(String, String)> {
        v.map(|v| vec![("Authorization".to_string(), v.to_string())])
            .unwrap_or_default()
    };
    let none = h(None);
    let bad = h(Some("Bearer bad"));
    let good = h(Some("Bearer good"));
    assert_eq!(NeedsToken.check(&head(&none)).unwrap_err().status, 401);
    assert_eq!(NeedsToken.check(&head(&bad)).unwrap_err().status, 403);
    assert!(NeedsToken.check(&head(&good)).is_ok());
    assert!(!NeedsToken.loopback_only());
    assert!(Localhost.check(&head(&none)).is_ok());
}

/// R-7.
#[test]
fn no_cors_by_default() {
    assert_eq!(cors_allow_origin(None, Some("https://a.example")), None);
    assert_eq!(cors_allow_origin(None, None), None);
    // Configured, but the request names no origin.
    assert_eq!(cors_allow_origin(Some("https://a.example"), None), None);
}

/// R-7.
#[test]
fn cors_origin_exact() {
    let c = Some("https://a.example");
    assert_eq!(cors_allow_origin(c, Some("https://a.example")), c);
    for other in [
        "https://a.example:8443",
        "http://a.example",
        "https://A.example",
        "https://a.example/",
        "https://b.example",
        "*",
    ] {
        assert_eq!(cors_allow_origin(c, Some(other)), None, "{other}");
    }
}
