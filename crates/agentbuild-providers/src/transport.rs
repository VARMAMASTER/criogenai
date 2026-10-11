//! HTTP transport: the only I/O in the provider layer (design 0006 §5, §6).
//!
//! It sends a prepared request and returns bytes, applying four timeouts
//! (connect, first byte, idle between chunks, total) and recording how far
//! the request got ([`Dispatch`]). It never retries: errors are classified
//! and returned, and the caller decides.

use std::time::Duration;

use tokio::time::Instant;

use crate::error::{Dispatch, HttpFailure, ModelError, ModelErrorKind, classify_http};

/// Timeouts for one attempt (0003 §5.1 defaults).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HttpConfig {
    /// Establishing the connection, including TLS. Default 10 s.
    pub connect_timeout: Duration,
    /// Until the response headers arrive. Default: the total timeout.
    pub first_byte_timeout: Option<Duration>,
    /// Between two chunks of a streamed body. Default 60 s.
    pub idle_timeout: Duration,
    /// The whole attempt, body included. Default 120 s.
    pub total_timeout: Duration,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            first_byte_timeout: None,
            idle_timeout: Duration::from_secs(60),
            total_timeout: Duration::from_secs(120),
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
}

/// A request ready to send: what a protocol's `encode` produces.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WireRequest {
    /// Full URL.
    pub url: String,
    /// Headers, including authentication.
    pub headers: Vec<(String, String)>,
    /// JSON body.
    pub body: Vec<u8>,
}

impl WireRequest {
    /// A POST of `body` to `url`.
    pub fn post(url: impl Into<String>, body: Vec<u8>) -> Self {
        Self {
            url: url.into(),
            headers: Vec::new(),
            body,
        }
    }

    /// Adds a header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// Sends [`WireRequest`]s. Cheap to clone; clones share the connection pool.
#[derive(Debug, Clone)]
pub struct Transport {
    client: reqwest::Client,
    config: HttpConfig,
    secrets: Vec<String>,
}

impl Transport {
    /// A transport with its own connection pool.
    pub fn new(config: HttpConfig) -> Result<Self, ModelError> {
        let client = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .build()
            .map_err(|err| {
                ModelError::new(
                    ModelErrorKind::Unknown,
                    format!("could not create the HTTP client: {}", describe(&err)),
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
        let deadline = Instant::now() + self.config.total_timeout;
        let first_byte_limit = self
            .config
            .first_byte_timeout
            .unwrap_or(self.config.total_timeout)
            .min(self.config.total_timeout);
        let first_byte = Instant::now() + first_byte_limit;

        let mut builder = self.client.post(&request.url).body(request.body.clone());
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        let response = match tokio::time::timeout_at(first_byte, builder.send()).await {
            Ok(Ok(response)) => response,
            Ok(Err(err)) => return Err(self.request_error(err, Dispatch::Sent)),
            Err(_) => {
                return Err(timeout(
                    format!("no response within {} ms", first_byte_limit.as_millis()),
                    Dispatch::Sent,
                ));
            }
        };

        let status = response.status().as_u16();
        let mut body = Body {
            response,
            transport: self.clone(),
            deadline,
            streaming: false,
        };
        if (200..300).contains(&status) {
            body.streaming = true;
            return Ok(body);
        }

        let headers: Vec<(String, String)> = body
            .response
            .headers()
            .iter()
            .filter_map(|(n, v)| Some((n.as_str().to_string(), v.to_str().ok()?.to_string())))
            .collect();
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
            std::time::SystemTime::now(),
        );
        Err(error.redact(&self.secret_refs()))
    }

    fn secret_refs(&self) -> Vec<&str> {
        self.secrets.iter().map(String::as_str).collect()
    }

    fn request_error(&self, err: reqwest::Error, dispatch: Dispatch) -> ModelError {
        let (kind, dispatch) = if err.is_connect() {
            // Never reached the server (DNS, refused, TLS): safe to retry.
            let kind = if err.is_timeout() {
                ModelErrorKind::Timeout
            } else {
                ModelErrorKind::Network
            };
            (kind, Dispatch::NotSent)
        } else if err.is_timeout() {
            (ModelErrorKind::Timeout, dispatch)
        } else {
            (ModelErrorKind::Network, dispatch)
        };
        ModelError::new(kind, describe(&err.without_url()))
            .with_dispatch(dispatch)
            .redact(&self.secret_refs())
    }
}

/// The body of a 2xx response, read chunk by chunk with the idle and total
/// timeouts.
#[derive(Debug)]
pub struct Body {
    response: reqwest::Response,
    transport: Transport,
    deadline: Instant,
    /// Body of a success response (errors are `Streaming`, not `Sent`).
    streaming: bool,
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
        match tokio::time::timeout_at(limit, self.response.chunk()).await {
            Ok(Ok(chunk)) => Ok(chunk.map(|bytes| bytes.to_vec())),
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

fn timeout(message: String, dispatch: Dispatch) -> ModelError {
    ModelError::new(ModelErrorKind::Timeout, message).with_dispatch(dispatch)
}

/// The error and its causes (call it on an error without its URL, which may
/// carry a key).
fn describe(err: &reqwest::Error) -> String {
    let mut text = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}
