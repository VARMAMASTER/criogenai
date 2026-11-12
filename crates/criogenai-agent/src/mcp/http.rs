//! Streamable HTTP (design 0022): one POST per message; the answer is JSON
//! or an SSE stream. Uses the engine's `Transport`, so it also runs in the
//! browser (with `fetch`).

use std::sync::Mutex;
use std::time::Duration;

use criogenai_core::CancelToken;
use criogenai_providers::{HttpConfig, ModelErrorKind, SseParser, Transport, WireRequest};
use serde_json::Value;

use super::rpc::{self, Era, Failure, Stop};
use super::{McpError, McpErrorKind, PROTOCOL_VERSION};

pub(super) struct Http {
    url: String,
    headers: Vec<(String, String)>,
    transport: Transport,
    max: usize,
    session: Mutex<Option<String>>,
    closed: std::sync::atomic::AtomicBool,
}

fn loopback(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1" || host == "::1"
}

impl Http {
    pub(super) fn new(
        label: &str,
        url: &str,
        headers: Vec<(String, String)>,
        max: usize,
        timeout: Duration,
    ) -> Result<Self, McpError> {
        let lower = url.to_ascii_lowercase();
        let host = rpc::host_of(url);
        let ok = match host {
            None => false,
            Some(_) if lower.starts_with("https://") => true,
            Some(host) if lower.starts_with("http://") => headers.is_empty() || loopback(host),
            Some(_) => false,
        };
        if !ok {
            return Err(McpError::new(
                McpErrorKind::Connect,
                format!(
                    "MCP server \"{label}\": the URL must be https (plain http only to this \
                     computer when headers are set)"
                ),
            ));
        }
        let config = HttpConfig::default()
            .total_timeout(timeout + Duration::from_secs(5))
            .idle_timeout(timeout + Duration::from_secs(5))
            .max_response_bytes(max);
        let transport = Transport::new(config)
            .map_err(|e| McpError::new(McpErrorKind::Connect, e.to_string()))?
            .redacting(
                headers
                    .iter()
                    .map(|(_, v)| v.clone())
                    .filter(|v| v.len() >= 4),
            );
        Ok(Self {
            url: url.to_string(),
            headers,
            transport,
            max,
            session: Mutex::new(None),
            closed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub(super) fn close(&self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn session(&self) -> Option<String> {
        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn wire(
        &self,
        era: &Era,
        method: &str,
        message: &Value,
        name: Option<&str>,
        extra: &[(String, String)],
    ) -> WireRequest {
        let body = serde_json::to_vec(message).unwrap_or_default();
        let mut request = WireRequest::post(&self.url, body)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json");
        for (k, v) in &self.headers {
            request = request.header(k, v);
        }
        match era {
            Era::Modern => {
                request = request
                    .header("MCP-Protocol-Version", PROTOCOL_VERSION)
                    .header("Mcp-Method", method);
                if let Some(name) = name {
                    request = request.header("Mcp-Name", rpc::header_value(name));
                }
                for (k, v) in extra {
                    request = request.header(k, v);
                }
            }
            Era::Legacy(version) => {
                if method != "initialize" {
                    request = request.header("MCP-Protocol-Version", version);
                }
                if let Some(session) = self.session() {
                    request = request.header("Mcp-Session-Id", session);
                }
            }
        }
        request
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn request(
        &self,
        era: &Era,
        id: i64,
        method: &str,
        message: &Value,
        name: Option<&str>,
        extra: &[(String, String)],
        cancel: Option<&CancelToken>,
        timeout: Duration,
    ) -> Result<Value, Failure> {
        if self.closed.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(Failure::Closed("the client was closed".into()));
        }
        let wire = self.wire(era, method, message, name, extra);
        // Dropping the exchange on a timeout or cancel closes its stream.
        match rpc::bounded(self.exchange(era, id, method, &wire), cancel, timeout).await {
            Ok(result) => result,
            Err(Stop::Timeout) => Err(Failure::Timeout),
            Err(Stop::Cancelled) => Err(Failure::Cancelled),
        }
    }

    pub(super) async fn notify(&self, era: &Era, method: &str, message: &Value, timeout: Duration) {
        let wire = self.wire(era, method, message, None, &[]);
        let send = async {
            if let Ok(mut exchange) = self.transport.exchange(&wire).await {
                while let Ok(Some(_)) = exchange.body.next_chunk().await {}
            }
        };
        let _ = rpc::bounded(send, None, timeout).await;
    }

    async fn exchange(
        &self,
        era: &Era,
        id: i64,
        method: &str,
        wire: &WireRequest,
    ) -> Result<Value, Failure> {
        let mut exchange = self.transport.exchange(wire).await.map_err(|e| {
            if e.kind == ModelErrorKind::Timeout {
                Failure::Timeout
            } else {
                Failure::Connect(e.message.clone())
            }
        })?;
        let status = exchange.status;
        if (200..300).contains(&status) {
            if method == "initialize"
                && let Some(session) = exchange.header("mcp-session-id")
            {
                let session = session.trim();
                if !session.is_empty()
                    && session.len() <= 256
                    && session.bytes().all(|b| (0x21..0x7f).contains(&b))
                {
                    *self
                        .session
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(session.to_string());
                }
            }
            let sse = exchange
                .header("content-type")
                .is_some_and(|t| t.to_ascii_lowercase().starts_with("text/event-stream"));
            if sse {
                let mut parser = SseParser::new();
                while let Some(chunk) = exchange.body.next_chunk().await.map_err(read_error)? {
                    for event in parser.feed(&chunk) {
                        if let Ok(message) = serde_json::from_str::<Value>(&event.data)
                            && rpc::answers(&message, id)
                        {
                            return rpc::response(&message);
                        }
                    }
                }
                return Err(Failure::Protocol(
                    "the server's stream ended without an answer".into(),
                ));
            }
            let body = read_all(&mut exchange.body, self.max).await?;
            let message: Value = serde_json::from_slice(&body)
                .map_err(|_| Failure::Protocol("the server's answer is not JSON".into()))?;
            let found = match &message {
                Value::Array(items) => items.iter().find(|m| rpc::answers(m, id)).cloned(),
                one if rpc::answers(one, id) => Some(one.clone()),
                _ => None,
            };
            return match found {
                Some(message) => rpc::response(&message),
                None => Err(Failure::Protocol(
                    "the server's answer does not match the request".into(),
                )),
            };
        }
        if status == 401 || status == 403 {
            return Err(Failure::Auth(status));
        }
        if status == 404 && matches!(era, Era::Legacy(_)) && self.session().is_some() {
            *self
                .session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            return Err(Failure::SessionExpired);
        }
        let body = read_all(&mut exchange.body, 64 * 1024)
            .await
            .unwrap_or_default();
        if let Ok(message) = serde_json::from_slice::<Value>(&body)
            && message.get("error").is_some()
        {
            return rpc::response(&message);
        }
        Err(Failure::Status(status))
    }
}

fn read_error(e: criogenai_providers::ModelError) -> Failure {
    if e.kind == ModelErrorKind::Timeout {
        Failure::Timeout
    } else {
        Failure::Protocol(e.message)
    }
}

async fn read_all(body: &mut criogenai_providers::Body, max: usize) -> Result<Vec<u8>, Failure> {
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next_chunk().await.map_err(read_error)? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() > max {
            return Err(Failure::Protocol(format!(
                "the server sent a message larger than {max} bytes"
            )));
        }
    }
    Ok(bytes)
}
