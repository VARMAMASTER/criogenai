//! JSON-RPC pieces shared by the transports (design 0022).

use std::future::Future;
use std::pin::pin;
use std::task::Poll;
use std::time::Duration;

use criogenai_core::CancelToken;
use serde_json::{Map, Value, json};

use super::PROTOCOL_VERSION;

pub(super) const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
pub(super) const META_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
pub(super) const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
pub(super) const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
pub(super) const METHOD_NOT_FOUND: i64 = -32601;
pub(super) const UNSUPPORTED_VERSION: i64 = -32022;

/// Which protocol generation the server speaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Era {
    /// 2026-07-28 and later: `_meta` on every request, no session.
    Modern,
    /// `initialize` first; the negotiated version.
    Legacy(String),
}

/// Why a request did not produce a result.
#[derive(Debug)]
pub(super) enum Failure {
    /// The server answered with a JSON-RPC error.
    Rpc {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    /// An HTTP status without a JSON-RPC error in the body.
    Status(u16),
    /// 401 or 403.
    Auth(u16),
    /// The legacy HTTP session is gone (404 with a session id).
    SessionExpired,
    Timeout,
    Cancelled,
    /// The process exited, or the client was closed.
    Closed(String),
    /// Could not start or reach the server.
    Connect(String),
    /// The answer was not usable.
    Protocol(String),
}

impl Failure {
    /// What happened, for an error message. `detail`: may include the
    /// server's own words (stderr, error text) for the developer; tool errors
    /// shown to the model keep only the error's message.
    pub(super) fn describe(&self, detail: bool) -> String {
        match self {
            Self::Rpc { code, message, .. } => {
                format!("error {code}: {}", super::cut(message, 500))
            }
            Self::Status(status) => format!("HTTP status {status}"),
            Self::Auth(status) => {
                format!("the server refused the credentials (HTTP {status})")
            }
            Self::SessionExpired => "the session expired".into(),
            Self::Timeout => "no answer in time".into(),
            Self::Cancelled => "cancelled".into(),
            Self::Closed(why) if detail => why.clone(),
            Self::Closed(_) => "the server stopped".into(),
            Self::Connect(why) if detail => why.clone(),
            Self::Connect(_) => "could not reach the server".into(),
            Self::Protocol(why) => why.clone(),
        }
    }
}

pub(super) fn client_info() -> Value {
    json!({"name": "criogenai", "version": env!("CARGO_PKG_VERSION")})
}

/// A request message; a modern one carries the protocol `_meta`.
pub(super) fn request(era: &Era, id: i64, method: &str, mut params: Map<String, Value>) -> Value {
    if *era == Era::Modern {
        let meta = params
            .entry("_meta")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(meta) = meta {
            meta.insert(META_VERSION.into(), json!(PROTOCOL_VERSION));
            meta.insert(META_CAPABILITIES.into(), json!({}));
            meta.insert(META_CLIENT_INFO.into(), client_info());
        }
    }
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// The result of a response message, or its error.
pub(super) fn response(message: &Value) -> Result<Value, Failure> {
    if let Some(error) = message.get("error") {
        return Err(Failure::Rpc {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string(),
            data: error.get("data").cloned(),
        });
    }
    match message.get("result") {
        Some(result @ Value::Object(_)) => Ok(result.clone()),
        _ => Err(Failure::Protocol(
            "the server answered without a result object".into(),
        )),
    }
}

/// Whether `message` answers request `id`.
pub(super) fn answers(message: &Value, id: i64) -> bool {
    message.get("method").is_none()
        && message.get("id").and_then(Value::as_i64) == Some(id)
        && (message.get("result").is_some() || message.get("error").is_some())
}

pub(super) enum Stop {
    Timeout,
    Cancelled,
}

/// Runs `work` until it finishes, `limit` passes or `cancel` fires.
pub(super) async fn bounded<T>(
    work: impl Future<Output = T>,
    cancel: Option<&CancelToken>,
    limit: Duration,
) -> Result<T, Stop> {
    let cancelled = async {
        match cancel {
            Some(token) => token.cancelled().await,
            None => std::future::pending().await,
        }
    };
    let mut work = pin!(work);
    let mut cancelled = pin!(cancelled);
    let race = std::future::poll_fn(|cx| {
        if let Poll::Ready(value) = work.as_mut().poll(cx) {
            return Poll::Ready(Ok(value));
        }
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(Stop::Cancelled));
        }
        Poll::Pending
    });
    match criogenai_rt::timeout(limit, race).await {
        Ok(result) => result,
        Err(_) => Err(Stop::Timeout),
    }
}

/// The host of `url`, without user info or port.
pub(super) fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = if let Some(v6) = host.strip_prefix('[') {
        v6.split(']').next()?
    } else {
        host.split(':').next()?
    };
    (!host.is_empty()).then_some(host)
}

/// A header value as MCP sends it: as is when it is printable ASCII without
/// surrounding spaces, else `=?base64?…?=`.
pub(super) fn header_value(value: &str) -> String {
    let plain = !value.is_empty()
        && value.bytes().all(|b| (0x20..0x7f).contains(&b))
        && !value.starts_with(' ')
        && !value.ends_with(' ')
        && !value.starts_with("=?");
    if plain {
        value.to_string()
    } else {
        format!("=?base64?{}?=", base64(value.as_bytes()))
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("héllo".as_bytes()), "aMOpbGxv");
    }

    #[test]
    fn header_values_are_encoded_when_needed() {
        assert_eq!(header_value("us-west"), "us-west");
        assert_eq!(header_value(" x"), "=?base64?IHg=?=");
        assert_eq!(header_value("é"), "=?base64?w6k=?=");
        assert_eq!(header_value("a\nb"), "=?base64?YQpi?=");
    }

    #[test]
    fn hosts_are_found() {
        assert_eq!(host_of("http://127.0.0.1:8080/mcp"), Some("127.0.0.1"));
        assert_eq!(host_of("https://u:p@example.com/x"), Some("example.com"));
        assert_eq!(host_of("http://[::1]:9/"), Some("::1"));
        assert_eq!(host_of("nope"), None);
    }
}
