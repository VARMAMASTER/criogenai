//! A local trace viewer for CriogenAI runs (design 0023).
//!
//! An OpenTelemetry receiver (OTLP/HTTP with a JSON body, what
//! `criogenai-telemetry` exports) that keeps runs in SQLite and shows them on a
//! page: each run's model calls, tool calls and steps on a time axis, with
//! durations, tokens, cost (from your prices) and failures.
//!
//! ```no_run
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let viewer = criogenai_trace::Viewer::builder().memory().build().await?;
//! let listener = tokio::net::TcpListener::bind("127.0.0.1:4318").await?;
//! axum::serve(listener, viewer.router()).await?;
//! # Ok(()) }
//! ```
//!
//! What arrives is data: ids are checked, strings are cut, the page shows
//! every value as text. The viewer listens on this computer only unless a
//! token is set (see [`ViewerBuilder::token`]).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

mod otlp;
mod store;

pub use otlp::{MAX_ATTRIBUTE_BYTES, MAX_SPANS_PER_RUN};

/// The largest export request accepted.
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// The address the viewer listens on by default (OTLP/HTTP's usual port).
pub const DEFAULT_ADDRESS: &str = "127.0.0.1:4318";

/// USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[non_exhaustive]
pub struct Price {
    /// Input (prompt) tokens.
    #[serde(default)]
    pub input: f64,
    /// Output (completion) tokens.
    #[serde(default)]
    pub output: f64,
}

impl Price {
    /// A price: USD per million input and output tokens.
    pub fn new(input: f64, output: f64) -> Self {
        Self { input, output }
    }
}

/// Prices by `provider:model` or model name.
pub type Prices = HashMap<String, Price>;

/// Why the viewer could not start or answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TraceError {
    /// The database failed.
    #[error("trace storage: {0}")]
    Storage(String),
    /// A setting is not usable.
    #[error("{0}")]
    Config(String),
}

/// How to build a [`Viewer`].
#[derive(Clone)]
pub struct ViewerBuilder {
    path: Option<PathBuf>,
    keep_runs: usize,
    keep_days: u64,
    prices: Prices,
    token: Option<String>,
}

impl std::fmt::Debug for ViewerBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewerBuilder")
            .field("path", &self.path)
            .field("keep_runs", &self.keep_runs)
            .field("keep_days", &self.keep_days)
            .field("prices", &self.prices.len())
            .field("token", &self.token.is_some())
            .finish()
    }
}

/// The default database: `~/.criogen/traces.db`.
pub fn default_db() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".criogenai").join("traces.db"))
}

impl ViewerBuilder {
    /// Keeps runs in a SQLite file.
    pub fn sqlite(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Keeps runs in memory only (gone when the viewer stops). The default.
    pub fn memory(mut self) -> Self {
        self.path = None;
        self
    }

    /// The newest runs kept. Default 10 000.
    pub fn keep_runs(mut self, runs: usize) -> Self {
        self.keep_runs = runs.max(1);
        self
    }

    /// Days a run is kept. Default 14.
    pub fn keep_days(mut self, days: u64) -> Self {
        self.keep_days = days.max(1);
        self
    }

    /// Prices for the cost column. Without them, cost is not shown.
    pub fn prices(mut self, prices: Prices) -> Self {
        self.prices = prices;
        self
    }

    /// Requires `Authorization: Bearer <token>` on the API and the receiver
    /// (needed to listen on another address than this computer's).
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into()).filter(|t| !t.is_empty());
        self
    }

    /// Opens the store.
    pub async fn build(self) -> Result<Viewer, TraceError> {
        let store = store::Store::open(
            self.path.as_deref(),
            self.keep_runs,
            Duration::from_secs(self.keep_days.saturating_mul(86_400)),
        )
        .await?;
        Ok(Viewer {
            inner: Arc::new(Inner {
                store,
                prices: self.prices,
                token: self.token,
            }),
        })
    }
}

#[derive(Debug)]
struct Inner {
    store: store::Store,
    prices: Prices,
    token: Option<String>,
}

/// The viewer: serve [`router`](Self::router) with axum.
#[derive(Debug, Clone)]
pub struct Viewer {
    inner: Arc<Inner>,
}

impl Viewer {
    /// Defaults: in memory, 10 000 runs, 14 days, no prices, no token.
    pub fn builder() -> ViewerBuilder {
        ViewerBuilder {
            path: None,
            keep_runs: 10_000,
            keep_days: 14,
            prices: Prices::new(),
            token: None,
        }
    }

    /// Whether requests need a token.
    pub fn has_token(&self) -> bool {
        self.inner.token.is_some()
    }

    /// The routes: `POST /v1/traces`, `GET /api/runs`, `GET
    /// /api/runs/{id}`, `DELETE /api/runs`, `GET /` (the page).
    pub fn router(&self) -> Router {
        Router::new()
            .route("/", get(page))
            .route("/v1/traces", post(receive))
            .route("/api/runs", get(list).delete(clear))
            .route("/api/runs/{id}", get(detail))
            .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
            .with_state(self.inner.clone())
    }

    /// Closes the database.
    pub async fn close(&self) {
        self.inner.store.close().await;
    }
}

/// Serves `viewer` on `address` until `shutdown` completes. Refuses another
/// address than this computer's without a token.
pub async fn serve(
    viewer: Viewer,
    listener: tokio::net::TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), TraceError> {
    let address = listener
        .local_addr()
        .map_err(|e| TraceError::Config(e.to_string()))?;
    if !address.ip().is_loopback() && !viewer.has_token() {
        return Err(TraceError::Config(format!(
            "refusing to serve traces on {address} without a token: listen on 127.0.0.1 or set one"
        )));
    }
    axum::serve(listener, viewer.router())
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|e| TraceError::Config(e.to_string()))
}

/// A viewer serving in the background ([`start`]).
#[derive(Debug)]
pub struct RunningViewer {
    address: std::net::SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<Result<(), TraceError>>,
    viewer: Viewer,
}

impl RunningViewer {
    /// The page's address, e.g. `http://127.0.0.1:4318`.
    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Stops serving and closes the database.
    pub async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = (&mut self.task).await;
        self.viewer.close().await;
    }
}

/// Builds the viewer and serves it on `address` in the background. Another
/// address than this computer's needs a token.
pub async fn start(
    builder: ViewerBuilder,
    address: std::net::SocketAddr,
) -> Result<RunningViewer, TraceError> {
    if !address.ip().is_loopback() && builder.token.is_none() {
        return Err(TraceError::Config(format!(
            "refusing to serve traces on {address} without a token: listen on 127.0.0.1 or set one"
        )));
    }
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|e| TraceError::Config(format!("cannot listen on {address}: {e}")))?;
    let address = listener
        .local_addr()
        .map_err(|e| TraceError::Config(e.to_string()))?;
    let viewer = builder.build().await?;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(serve(viewer.clone(), listener, async {
        let _ = stopped.await;
    }));
    Ok(RunningViewer {
        address,
        stop: Some(stop),
        task,
        viewer,
    })
}

impl ViewerBuilder {
    /// A builder and an address from JSON, for the SDK bridges: `{"listen":
    /// "127.0.0.1:4318", "db": "default" | "memory" | <path>, "keep_runs",
    /// "keep_days", "prices": {...}, "token"}`.
    pub fn from_spec(spec: &Value) -> Result<(Self, std::net::SocketAddr), TraceError> {
        let listen = spec["listen"].as_str().unwrap_or(DEFAULT_ADDRESS);
        let address = listen
            .parse()
            .map_err(|_| TraceError::Config(format!("not an address: {listen}")))?;
        let mut builder = Viewer::builder();
        match spec["db"].as_str() {
            None | Some("default") => {
                if let Some(path) = default_db() {
                    builder = builder.sqlite(path);
                }
            }
            Some("memory") => {}
            Some(path) => builder = builder.sqlite(path),
        }
        if let Some(n) = spec["keep_runs"].as_u64() {
            builder = builder.keep_runs(usize::try_from(n).unwrap_or(usize::MAX));
        }
        if let Some(n) = spec["keep_days"].as_u64() {
            builder = builder.keep_days(n);
        }
        if let Some(prices) = spec.get("prices").filter(|p| p.is_object()) {
            let prices: Prices = serde_json::from_value(prices.clone())
                .map_err(|e| TraceError::Config(format!("prices: {e}")))?;
            builder = builder.prices(prices);
        }
        if let Some(token) = spec["token"].as_str() {
            builder = builder.token(token);
        }
        Ok((builder, address))
    }
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({"error": message}))).into_response()
}

/// Equal in time whatever the bytes (the token is a secret).
fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The refusal, if the request lacks the token.
fn denied(inner: &Inner, headers: &HeaderMap) -> Option<Response> {
    let token = inner.token.as_ref()?;
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    (!same(given.as_bytes(), token.as_bytes()))
        .then(|| error(StatusCode::UNAUTHORIZED, "a valid token is needed"))
}

async fn receive(State(inner): State<Arc<Inner>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Some(refusal) = denied(&inner, &headers) {
        return refusal;
    }
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.to_ascii_lowercase().starts_with("application/json"));
    if !json {
        return error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "send OTLP/HTTP with a JSON body (Content-Type: application/json); protobuf is not supported",
        );
    }
    let Ok(request) = serde_json::from_slice::<Value>(&body) else {
        return error(StatusCode::BAD_REQUEST, "the body is not JSON");
    };
    let rows = otlp::parse(&request);
    match inner.store.insert(rows, &inner.prices).await {
        Ok(()) => axum::Json(json!({})).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    limit: Option<i64>,
    before: Option<f64>,
    q: Option<String>,
}

async fn list(
    State(inner): State<Arc<Inner>>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    if let Some(refusal) = denied(&inner, &headers) {
        return refusal;
    }
    let q = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty());
    match inner
        .store
        .runs(query.limit.unwrap_or(100), query.before, q)
        .await
    {
        Ok(runs) => {
            axum::Json(json!({ "runs": runs, "costs": !inner.prices.is_empty() })).into_response()
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

async fn detail(
    State(inner): State<Arc<Inner>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(refusal) = denied(&inner, &headers) {
        return refusal;
    }
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return error(StatusCode::BAD_REQUEST, "a run id is 32 hex characters");
    }
    match inner.store.run(&id.to_ascii_lowercase()).await {
        Ok(Some((summary, spans))) => {
            axum::Json(json!({ "run": summary, "spans": spans })).into_response()
        }
        Ok(None) => error(StatusCode::NOT_FOUND, "no such run"),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

async fn clear(State(inner): State<Arc<Inner>>, headers: HeaderMap) -> Response {
    if let Some(refusal) = denied(&inner, &headers) {
        return refusal;
    }
    match inner.store.clear().await {
        Ok(()) => axum::Json(json!({})).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

/// The page: one file, nothing loaded from elsewhere.
const PAGE: &str = include_str!("page.html");

/// The page's Content-Security-Policy: its own inline script and style only.
pub const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
                       connect-src 'self'; img-src data:; base-uri 'none'; form-action 'none'; \
                       frame-ancestors 'none'";

async fn page() -> Response {
    let mut response = PAGE.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}
