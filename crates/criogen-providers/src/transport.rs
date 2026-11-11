//! HTTP transport: the only I/O in the provider layer (design 0006 §5, §6).
//!
//! It sends a prepared request and returns bytes, applying four timeouts
//! (connect, first byte, idle between chunks, total) and recording how far
//! the request got ([`Dispatch`]). It never retries: errors are classified
//! and returned, and the caller decides.
//!
//! Two backends move the bytes (design 0021): reqwest on servers, the host's
//! `fetch` in the browser (WebAssembly). Everything else here (timeouts,
//! the size limit, refusing redirects, classifying errors, redacting keys)
//! is shared, so both behave the same.

use std::time::Duration;

use criogen_rt::Instant;

use crate::error::{Dispatch, HttpFailure, ModelError, ModelErrorKind, classify_http};

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
#[path = "transport/reqwest_backend.rs"]
mod backend;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
#[path = "transport/fetch_backend.rs"]
mod backend;

/// Timeouts for one attempt (0003 §5.1 defaults).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HttpConfig {
    /// Establishing the connection, including TLS. Default 10 s. (In the
    /// browser, `fetch` has no separate connect step: the first-byte and
    /// total timeouts cover it.)
    pub connect_timeout: Duration,
    /// Until the response headers arrive. Default: the total timeout.
    pub first_byte_timeout: Option<Duration>,
    /// Between two chunks of a streamed body. Default 60 s.
    pub idle_timeout: Duration,
    /// The whole attempt, body included. Default 120 s.
    pub total_timeout: Duration,
    /// The most bytes of one response body (the timeouts bound time, not
    /// memory): a larger one fails with `invalid_response`. Default 64 MiB.
    pub max_response_bytes: usize,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            first_byte_timeout: None,
            idle_timeout: Duration::from_secs(60),
            total_timeout: Duration::from_secs(120),
            max_response_bytes: 64 * 1024 * 1024,
        }
    }
}

impl HttpConfig {
    /// Sets [`connect_timeout`](Self::connect_timeout).
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Sets [`first_byte_timeout`](Self::first_byte_timeout).
    pub fn first_byte_timeout(mut self, timeout: Duration) -> Self {
        self.first_byte_timeout = Some(timeout);
        self
    }

    /// Sets [`idle_timeout`](Self::idle_timeout).
    pub fn idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
    }

    /// Sets [`total_timeout`](Self::total_timeout).
    pub fn total_timeout(mut self, timeout: Duration) -> Self {
        self.total_timeout = timeout;
        self
    }

    /// Sets [`max_response_bytes`](Self::max_response_bytes).
    pub fn max_response_bytes(mut self, bytes: usize) -> Self {
        self.max_response_bytes = bytes;
        self
    }
}

/// A request ready to send: what a protocol's `encode` produces.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WireRequest {
    /// `POST` (every model call) or `GET` (token services such as Azure's
    /// managed identity).
    pub method: &'static str,
    /// Full URL.
    pub url: String,
    /// Headers, including authentication.
    pub headers: Vec<(String, String)>,
    /// JSON body.
    pub body: Vec<u8>,
}

impl std::fmt::Debug for WireRequest {
    /// Header values and the body are not shown: they hold the API key and
    /// the prompt.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WireRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field(
                "headers",
                &self.headers.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            )
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

impl WireRequest {
    /// A POST of `body` to `url`.
    pub fn post(url: impl Into<String>, body: Vec<u8>) -> Self {
        Self {
            method: "POST",
            url: url.into(),
            headers: Vec::new(),
            body,
        }
    }

    /// A GET of `url`, without a body.
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: "GET",
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// Adds a header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// Sends [`WireRequest`]s. Cheap to clone; clones share the connection pool.
#[derive(Clone)]
pub struct Transport {
    client: backend::Client,
    config: HttpConfig,
    secrets: Vec<String>,
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transport")
            .field("config", &self.config)
            .field("secrets", &self.secrets.len())
            .finish_non_exhaustive()
    }
}

impl Transport {
    /// A transport with its own connection pool.
    pub fn new(config: HttpConfig) -> Result<Self, ModelError> {
        // No redirects: an LLM API never needs one, and a redirected POST
        // would carry the prompt and any non-standard auth header (such as
        // `x-api-key`) to another host.
        let client = backend::Client::new(&config).map_err(|message| {
            ModelError::new(
                ModelErrorKind::Unknown,
                format!("could not create the HTTP client: {message}"),
            )
            .with_dispatch(Dispatch::NotSent)
        })?;
        Ok(Self {
            client,
            config,
            secrets: Vec::new(),
        })
    }

    /// Values (API keys) to remove from every error message.
    pub fn redacting(mut self, secrets: impl IntoIterator<Item = String>) -> Self {
        self.secrets.extend(secrets);
        self
    }

    /// The timeouts in use.
    pub fn config(&self) -> &HttpConfig {
        &self.config
    }

    /// Sends `request` and returns the whole body of a 2xx response.
    /// Other statuses are classified with [`classify_http`].
    pub async fn send(&self, request: &WireRequest) -> Result<Vec<u8>, ModelError> {
        let mut body = self.open(request).await?;
        let mut bytes = Vec::new();
        while let Some(chunk) = body.next_chunk().await? {
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    /// Sends `request` and returns the body of a 2xx response as it arrives.
    pub async fn open(&self, request: &WireRequest) -> Result<Body, ModelError> {
        let exchange = self.exchange(request).await?;
        if (200..300).contains(&exchange.status) {
            return Ok(exchange.body);
        }
        let Exchange {
            status,
            headers,
            mut body,
        } = exchange;
        // Read the error body (bounded by the same timeouts); a failure here
        // still leaves a useful status-based classification.
        let mut text = Vec::new();
        while let Ok(Some(chunk)) = body.next_chunk().await {
            text.extend_from_slice(&chunk);
            if text.len() > 64 * 1024 {
                break;
            }
        }
        let text = String::from_utf8_lossy(&text);
        let error = classify_http(
            &HttpFailure::new(status, &headers, &text),
            criogen_rt::system_now(),
        );
        Err(error.redact(&self.secret_refs()))
    }

    /// Sends `request` and returns the response whatever its status, with its
    /// headers, for protocols that read their own error bodies (MCP, design
    /// 0022). The body has the same timeouts and size limit; redirects are
    /// still refused.
    pub async fn exchange(&self, request: &WireRequest) -> Result<Exchange, ModelError> {
        let deadline = Instant::now() + self.config.total_timeout;
        let first_byte_limit = self
            .config
            .first_byte_timeout
            .unwrap_or(self.config.total_timeout)
            .min(self.config.total_timeout);
        let first_byte = Instant::now() + first_byte_limit;

        let response = match criogen_rt::timeout_at(first_byte, self.client.post(request)).await {
            Ok(Ok(response)) => response,
            Ok(Err(err)) => return Err(self.request_error(err, Dispatch::Sent)),
            Err(_) => {
                return Err(timeout(
                    format!("no response within {} ms", first_byte_limit.as_millis()),
                    Dispatch::Sent,
                ));
            }
        };

        let status = response.status();
        let redirect = response.is_redirect();
        let headers = response.headers();
        let mut body = Body {
            response,
            transport: self.clone(),
            deadline,
            streaming: false,
            received: 0,
        };
        // A redirect is an error: it was not followed (see `new`).
        if redirect {
            return Err(ModelError::new(
                ModelErrorKind::InvalidRequest,
                format!(
                    "the server answered {status}, a redirect; redirects are not followed \
                     (check the base URL)"
                ),
            )
            .with_dispatch(Dispatch::Sent));
        }
        body.streaming = (200..300).contains(&status);
        Ok(Exchange {
            status,
            headers,
            body,
        })
    }

    fn secret_refs(&self) -> Vec<&str> {
        self.secrets.iter().map(String::as_str).collect()
    }

    fn request_error(&self, err: backend::Error, dispatch: Dispatch) -> ModelError {
        let (kind, dispatch) = err.classify(dispatch);
        ModelError::new(kind, err.describe())
            .with_dispatch(dispatch)
            .redact(&self.secret_refs())
    }
}

/// A response of any status ([`Transport::exchange`]).
#[derive(Debug)]
#[non_exhaustive]
pub struct Exchange {
    /// The HTTP status.
    pub status: u16,
    /// Response headers, names in lower case.
    pub headers: Vec<(String, String)>,
    /// The body, read with the transport's timeouts and size limit.
    pub body: Body,
}

impl Exchange {
    /// The first header named `name` (any case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// The body of a response, read chunk by chunk with the idle and total
/// timeouts.
#[derive(Debug)]
pub struct Body {
    response: backend::Response,
    transport: Transport,
    deadline: Instant,
    /// Body of a success response (errors are `Streaming`, not `Sent`).
    streaming: bool,
    /// Bytes read so far, against `max_response_bytes`.
    received: usize,
}

impl Body {
    /// The next chunk, or `None` at the end. Fails with `timeout` if no chunk
    /// arrives within the idle timeout or the total time runs out.
    pub async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, ModelError> {
        let idle = Instant::now() + self.transport.config.idle_timeout;
        let limit = idle.min(self.deadline);
        let dispatch = if self.streaming {
            Dispatch::Streaming
        } else {
            Dispatch::Sent
        };
        let max = self.transport.config.max_response_bytes;
        let allowed = max.saturating_sub(self.received);
        match criogen_rt::timeout_at(limit, self.response.chunk(allowed)).await {
            Ok(Ok(chunk)) => {
                self.received = self
                    .received
                    .saturating_add(chunk.as_ref().map_or(0, Vec::len));
                if self.received > max {
                    return Err(too_large(max, dispatch));
                }
                Ok(chunk)
            }
            Ok(Err(err)) if err.is_too_large() => Err(too_large(max, dispatch)),
            Ok(Err(err)) => Err(self.transport.request_error(err, dispatch)),
            Err(_) => {
                let config = &self.transport.config;
                let what = if limit == self.deadline {
                    format!(
                        "the response did not finish within {} ms",
                        config.total_timeout.as_millis()
                    )
                } else {
                    format!("no data arrived for {} ms", config.idle_timeout.as_millis())
                };
                Err(timeout(what, dispatch))
            }
        }
    }
}

fn too_large(max: usize, dispatch: Dispatch) -> ModelError {
    ModelError::new(
        ModelErrorKind::InvalidResponse,
        format!("the response is larger than the limit of {max} bytes"),
    )
    .with_dispatch(dispatch)
}

fn timeout(message: String, dispatch: Dispatch) -> ModelError {
    ModelError::new(ModelErrorKind::Timeout, message).with_dispatch(dispatch)
}
