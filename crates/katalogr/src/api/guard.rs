//! Authorisation and the checks that keep a local server local (FEAT-049 R-7): the pluggable
//! [`Authorizer`], the `Localhost` default, the `Host` check against DNS rebinding, the refusal of
//! a non-loopback listen and the exact-origin CORS rule.

use std::net::SocketAddr;

use katalogr_dist::DistError;

pub use super::http::RequestHead;

/// Why an authorizer refused: 401 when the caller is unknown, 403 when it is not allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denied {
    pub status: u16,
    pub message: String,
}

impl Denied {
    pub fn unauthorized(message: impl Into<String>) -> Denied {
        Denied {
            status: 401,
            message: message.into(),
        }
    }

    pub fn forbidden(message: impl Into<String>) -> Denied {
        Denied {
            status: 403,
            message: message.into(),
        }
    }
}

/// Pluggable authorisation (ADR-0010). A provider sees only the method, path and headers.
pub trait Authorizer: Send + Sync {
    fn check(&self, req: &RequestHead<'_>) -> Result<(), Denied>;

    /// True only for an authorizer that allows everything and so may serve a loopback listener
    /// alone. A provider that really decides returns false (the default).
    fn loopback_only(&self) -> bool {
        false
    }
}

/// Allow everything, on a loopback listener only.
pub struct Localhost;

impl Authorizer for Localhost {
    fn check(&self, _req: &RequestHead<'_>) -> Result<(), Denied> {
        Ok(())
    }

    fn loopback_only(&self) -> bool {
        true
    }
}

/// Refuse (usage error, exit 2) a non-loopback `listen` while `auth` is loopback-only.
pub fn check_listen(listen: SocketAddr, auth: &dyn Authorizer) -> Result<(), DistError> {
    if auth.loopback_only() && !listen.ip().is_loopback() {
        return Err(DistError::Usage(format!(
            "refusing to listen on {listen}: only loopback addresses are served until a permission \
             provider is configured"
        )));
    }
    Ok(())
}

/// On a loopback listener, the `Host` header must be `localhost`, `127.0.0.1` or `[::1]` with the
/// listener's port. Otherwise a web page could reach the API through DNS rebinding (403).
pub fn check_host(listen: SocketAddr, host: Option<&str>) -> Result<(), Denied> {
    if !listen.ip().is_loopback() {
        return Ok(());
    }
    let bad = || Denied::forbidden("unexpected Host header");
    let host = host.ok_or_else(bad)?;
    let (name, port) = host.rsplit_once(':').ok_or_else(bad)?;
    let name = name.to_ascii_lowercase();
    let port_ok = port.parse::<u16>().is_ok_and(|p| p == listen.port());
    if port_ok && matches!(name.as_str(), "localhost" | "127.0.0.1" | "[::1]") {
        Ok(())
    } else {
        Err(bad())
    }
}

/// The `Access-Control-Allow-Origin` value for a request: the configured origin when the request's
/// `Origin` is exactly it, otherwise none. No CORS header is sent unless an origin is configured.
pub fn cors_allow_origin<'a>(
    configured: Option<&'a str>,
    request_origin: Option<&str>,
) -> Option<&'a str> {
    match (configured, request_origin) {
        (Some(c), Some(o)) if c == o => Some(c),
        _ => None,
    }
}
