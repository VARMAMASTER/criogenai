//! An OpenAI- and Anthropic-compatible LLM gateway on AgentBuild's provider
//! layer (design 0006 §7, §12, §13).
//!
//! Clients speak the OpenAI chat completions API or the Anthropic Messages
//! API; each *route* (the `model` a client asks for) maps to one or more
//! upstream models, which can speak any protocol AgentBuild supports
//! (OpenAI, Anthropic, Gemini, Ollama, vLLM, …). Optional virtual keys
//! limit who may use which route, with per-key rate limits and token
//! budgets ([`KeyLimits`]); every request produces a [`UsageRecord`]
//! without content.
//!
//! ```no_run
//! use std::sync::Arc;
//! use agentbuild_gateway::Gateway;
//! use agentbuild_providers::{Endpoint, ProviderModel};
//!
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let openai = ProviderModel::new(Endpoint::openai(std::env::var("OPENAI_API_KEY")?), "gpt-5-mini")?;
//! let gateway = Gateway::builder()
//!     .route("support", Arc::new(openai))
//!     .build()?;
//! let listener = tokio::net::TcpListener::bind("127.0.0.1:4000").await?;
//! axum::serve(listener, gateway.router()).await?;
//! # Ok(()) }
//! ```

mod anthropic;
mod config;
mod limits;
mod openai;

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use agentbuild_providers::{Model, ModelError, ModelErrorKind, ModelResponse};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

pub use config::{BudgetConfig, Config, ConfigError, KeyConfig, ModelConfig, RouteConfig};
pub use limits::{BudgetPeriod, KeyLimits};

/// Largest request body by default (images arrive base64-encoded).
pub const DEFAULT_BODY_LIMIT: usize = 20 * 1024 * 1024;

/// Why a gateway could not be built, or a request was refused.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GatewayError {
    /// The configuration is not usable.
    #[error("invalid gateway configuration: {0}")]
    Config(String),
    /// The client's request is malformed (400).
    #[error("{0}")]
    InvalidRequest(String),
}

/// One request's usage, without content (design 0006 §12.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct UsageRecord {
    /// The virtual key's name; `None` on an open gateway.
    pub key: Option<String>,
    /// The route the client asked for.
    pub route: String,
    /// `provider/model` of the model that answered (or failed last).
    pub model: String,
    /// Input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Time to the complete answer.
    pub latency_ms: u64,
    /// `ok`, or the error kind (`rate_limited`, …), or why the gateway
    /// refused it (`key_rate_limited`, `budget_exceeded`).
    pub outcome: String,
    /// Models tried before the one in `model`.
    pub fallbacks: u32,
    /// Whether the answer was streamed.
    pub stream: bool,
}

struct Key {
    name: String,
    routes: BTreeSet<String>,
    /// Limits and what was used; `None` without limits.
    usage: Option<Mutex<limits::Usage>>,
}

struct Inner {
    routes: BTreeMap<String, Vec<Arc<dyn Model>>>,
    /// SHA-256 digest of a key → the key. Empty: an open gateway.
    keys: BTreeMap<[u8; 32], Key>,
    usage: Option<mpsc::Sender<UsageRecord>>,
    dropped: AtomicU64,
    body_limit: usize,
}

/// The gateway: build with [`Gateway::builder`] or
/// [`Config::into_gateway`], serve [`router`](Self::router).
#[derive(Clone)]
pub struct Gateway {
    inner: Arc<Inner>,
}

impl fmt::Debug for Gateway {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gateway")
            .field("routes", &self.inner.routes.keys().collect::<Vec<_>>())
            .field("keys", &self.inner.keys.len())
            .finish_non_exhaustive()
    }
}

/// Builds a [`Gateway`].
#[derive(Default)]
pub struct GatewayBuilder {
    routes: BTreeMap<String, Vec<Arc<dyn Model>>>,
    keys: Vec<(String, String, Vec<String>)>,
    limits: BTreeMap<String, KeyLimits>,
    usage: Option<mpsc::Sender<UsageRecord>>,
    body_limit: Option<usize>,
}

impl fmt::Debug for GatewayBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GatewayBuilder")
            .field("routes", &self.routes.keys().collect::<Vec<_>>())
            .field("keys", &self.keys.len())
            .finish_non_exhaustive()
    }
}

impl GatewayBuilder {
    /// Clients asking for `name` get `model`.
    pub fn route(self, name: impl Into<String>, model: Arc<dyn Model>) -> Self {
        self.route_with_fallbacks(name, [model])
    }

    /// Clients asking for `name` get the first of `models` that answers
    /// (design 0006 §12.4).
    pub fn route_with_fallbacks(
        mut self,
        name: impl Into<String>,
        models: impl IntoIterator<Item = Arc<dyn Model>>,
    ) -> Self {
        self.routes
            .insert(name.into(), models.into_iter().collect());
        self
    }

    /// A virtual key: requests with `Authorization: Bearer <secret>` may
    /// use `routes`. With any key, every request needs one.
    pub fn key(
        mut self,
        name: impl Into<String>,
        secret: impl Into<String>,
        routes: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.keys.push((
            name.into(),
            secret.into(),
            routes.into_iter().map(Into::into).collect(),
        ));
        self
    }

    /// Rate limits and a token budget for the key named `name` (design 0006
    /// §13), kept in memory.
    pub fn key_limits(mut self, name: impl Into<String>, limits: KeyLimits) -> Self {
        self.limits.insert(name.into(), limits);
        self
    }

    /// Sends a [`UsageRecord`] per request to `sender` (`try_send`: a full
    /// channel drops records and counts them, never slowing a request).
    pub fn usage(mut self, sender: mpsc::Sender<UsageRecord>) -> Self {
        self.usage = Some(sender);
        self
    }

    /// Largest request body (default [`DEFAULT_BODY_LIMIT`]).
    pub fn body_limit(mut self, bytes: usize) -> Self {
        self.body_limit = Some(bytes);
        self
    }

    /// Checks the routes and keys.
    pub fn build(self) -> Result<Gateway, GatewayError> {
        if self.routes.is_empty() {
            return Err(GatewayError::Config("no routes".into()));
        }
        if let Some((name, _)) = self.routes.iter().find(|(_, models)| models.is_empty()) {
            return Err(GatewayError::Config(format!(
                "route `{name}` has no models"
            )));
        }
        let mut keys = BTreeMap::new();
        let mut limits = self.limits;
        for (name, secret, routes) in self.keys {
            if secret.len() < 16 {
                return Err(GatewayError::Config(format!(
                    "key `{name}` is shorter than 16 characters"
                )));
            }
            if let Some(unknown) = routes.iter().find(|r| !self.routes.contains_key(*r)) {
                return Err(GatewayError::Config(format!(
                    "key `{name}` names an unknown route `{unknown}`"
                )));
            }
            let key = Key {
                name: name.clone(),
                routes: routes.into_iter().collect(),
                usage: limits
                    .remove(&name)
                    .filter(|l| !l.is_empty())
                    .map(|l| Mutex::new(limits::Usage::new(&l))),
            };
            if keys.insert(digest(&secret), key).is_some() {
                return Err(GatewayError::Config(format!(
                    "key `{name}` has the same secret as another key"
                )));
            }
        }
        if let Some(name) = limits.keys().next() {
            return Err(GatewayError::Config(format!(
                "limits for an unknown key `{name}`"
            )));
        }
        Ok(Gateway {
            inner: Arc::new(Inner {
                routes: self.routes,
                keys,
                usage: self.usage,
                dropped: AtomicU64::new(0),
                body_limit: self.body_limit.unwrap_or(DEFAULT_BODY_LIMIT),
            }),
        })
    }
}

fn digest(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

impl Gateway {
    /// A builder.
    pub fn builder() -> GatewayBuilder {
        GatewayBuilder::default()
    }

    /// `true` if requests need a key.
    pub fn has_keys(&self) -> bool {
        !self.inner.keys.is_empty()
    }

    /// Usage records dropped because the channel was full.
    pub fn dropped_usage_records(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// Tokens the key named `name` has spent in its budget's current period
    /// (or in all, without a budget period); `None` for a key without
    /// limits.
    pub fn spent(&self, name: &str) -> Option<u64> {
        let key = self.inner.keys.values().find(|k| k.name == name)?;
        let usage = key.usage.as_ref()?;
        Some(lock(usage).spent())
    }

    /// The HTTP routes: `POST /v1/chat/completions`, `POST /v1/messages`,
    /// `GET /v1/models`, `GET /health`.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/v1/chat/completions", post(chat))
            .route("/v1/messages", post(messages))
            .route("/v1/models", get(models))
            .route("/health", get(|| async { "ok" }))
            .layer(DefaultBodyLimit::max(self.inner.body_limit))
            .with_state(self.clone())
    }

    /// The key of a request (`Authorization: Bearer <key>` or `x-api-key`):
    /// `Ok(None)` on an open gateway.
    fn authorize(&self, headers: &HeaderMap) -> Result<Option<&Key>, Unauthorized> {
        if self.inner.keys.is_empty() {
            return Ok(None);
        }
        let secret = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()))
            .map(str::trim);
        secret
            .and_then(|s| self.inner.keys.get(&digest(s)))
            .map(Some)
            .ok_or(Unauthorized)
    }

    fn key_named(&self, name: Option<&str>) -> Option<&Key> {
        let name = name?;
        self.inner.keys.values().find(|k| k.name == name)
    }

    /// Admits a request under its key's limits.
    fn admit(&self, key: Option<&str>) -> Result<(), limits::Refusal> {
        match self.key_named(key).and_then(|k| k.usage.as_ref()) {
            Some(usage) => lock(usage).admit(),
            None => Ok(()),
        }
    }

    /// Charges an answer's tokens to its key.
    fn charge(&self, key: Option<&str>, tokens: u64) {
        if let Some(usage) = self.key_named(key).and_then(|k| k.usage.as_ref()) {
            lock(usage).charge(tokens);
        }
    }

    fn record(&self, record: UsageRecord) {
        if let Some(sender) = &self.inner.usage
            && sender.try_send(record).is_err()
        {
            self.inner.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------- errors ----------

/// The client's wire format: errors are shaped like the provider's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    OpenAi,
    Anthropic,
}

impl Dialect {
    fn error_response(self, status: StatusCode, code: &str, message: &str) -> Response {
        match self {
            Self::OpenAi => error_response(status, code, message),
            Self::Anthropic => anthropic_error_response(status, message),
        }
    }

    fn model_error(self, error: &ModelError) -> Response {
        self.error_response(status_for(error.kind), error.kind.code(), &error.message)
    }
}

/// A request without a valid key (401).
struct Unauthorized;

const UNAUTHORIZED: &str = "a valid API key is required (Authorization: Bearer <key> or x-api-key)";

impl IntoResponse for Unauthorized {
    fn into_response(self) -> Response {
        error_response(StatusCode::UNAUTHORIZED, "invalid_api_key", UNAUTHORIZED)
    }
}

/// Anthropic's error shape; its `type` follows the status (design 0006
/// §13.1).
fn anthropic_error_type(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 | 413 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        429 => "rate_limit_error",
        503 | 529 => "overloaded_error",
        _ => "api_error",
    }
}

fn anthropic_error_response(status: StatusCode, message: &str) -> Response {
    let body = json!({"type": "error", "error": {"type": anthropic_error_type(status), "message": message}});
    (status, axum::Json(body)).into_response()
}

/// A request the key's limits refuse: 429 with `Retry-After`.
fn refusal_response(dialect: Dialect, refusal: limits::Refusal) -> Response {
    let mut response = dialect.error_response(
        StatusCode::TOO_MANY_REQUESTS,
        refusal.code(),
        refusal.message(),
    );
    if let Some(secs) = refusal.retry_after_secs() {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(secs));
    }
    response
}

fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    let kind = if status == StatusCode::UNAUTHORIZED {
        "authentication_error"
    } else if status.is_client_error() {
        "invalid_request_error"
    } else {
        "api_error"
    };
    (
        status,
        axum::Json(json!({"error": {"message": message, "type": kind, "code": code}})),
    )
        .into_response()
}

/// The HTTP status for an upstream failure (design 0006 §12.5).
fn status_for(kind: ModelErrorKind) -> StatusCode {
    match kind {
        ModelErrorKind::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        ModelErrorKind::InvalidRequest | ModelErrorKind::ContextOverflow => StatusCode::BAD_REQUEST,
        ModelErrorKind::ContentFilter => StatusCode::BAD_REQUEST,
        ModelErrorKind::Timeout => StatusCode::GATEWAY_TIMEOUT,
        ModelErrorKind::Overloaded => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_GATEWAY,
    }
}

/// Kinds that move on to the route's next model (design 0006 §12.4).
fn falls_back(kind: ModelErrorKind) -> bool {
    kind.is_transient() || kind == ModelErrorKind::QuotaExhausted
}

fn name_of(model: &dyn Model) -> String {
    format!("{}/{}", model.provider(), model.model())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ---------- handlers ----------

async fn models(State(gateway): State<Gateway>, headers: HeaderMap) -> Response {
    let key = match gateway.authorize(&headers) {
        Ok(key) => key,
        Err(unauthorized) => return unauthorized.into_response(),
    };
    let data: Vec<Value> = gateway
        .inner
        .routes
        .keys()
        .filter(|route| key.is_none_or(|k| k.routes.contains(*route)))
        .map(
            |route| json!({"id": route, "object": "model", "created": 0, "owned_by": "agentbuild"}),
        )
        .collect();
    axum::Json(json!({"object": "list", "data": data})).into_response()
}

/// A request's key: its name and the routes it may use.
type KeyRoutes = Option<(String, BTreeSet<String>)>;

/// An admitted request: its key's name and the route's models.
type Admitted = (Option<String>, Vec<Arc<dyn Model>>);

/// The key and the body of a request, or the response refusing it.
fn read(
    gateway: &Gateway,
    headers: &HeaderMap,
    body: &Bytes,
    dialect: Dialect,
) -> Result<(KeyRoutes, Value), Box<Response>> {
    let key = match gateway.authorize(headers) {
        Ok(key) => key.map(|k| (k.name.clone(), k.routes.clone())),
        Err(_) => {
            return Err(Box::new(dialect.error_response(
                StatusCode::UNAUTHORIZED,
                "invalid_api_key",
                UNAUTHORIZED,
            )));
        }
    };
    let body: Value = serde_json::from_slice(body).map_err(|err| {
        Box::new(dialect.error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            &format!("the body is not JSON: {err}"),
        ))
    })?;
    Ok((key, body))
}

/// The route's models, once the key may use the route and its limits
/// admit the request (a refusal is recorded).
fn admit(
    gateway: &Gateway,
    key: KeyRoutes,
    route: &str,
    stream: bool,
    dialect: Dialect,
) -> Result<Admitted, Box<Response>> {
    let allowed = key
        .as_ref()
        .is_none_or(|(_, routes)| routes.contains(route));
    let Some(models) = gateway.inner.routes.get(route).filter(|_| allowed).cloned() else {
        return Err(Box::new(dialect.error_response(
            StatusCode::NOT_FOUND,
            "model_not_found",
            &format!("no model `{route}`"),
        )));
    };
    let key = key.map(|(name, _)| name);
    if let Err(refusal) = gateway.admit(key.as_deref()) {
        gateway.record(UsageRecord {
            key,
            route: route.to_string(),
            model: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            latency_ms: 0,
            outcome: refusal.code().to_string(),
            fallbacks: 0,
            stream,
        });
        return Err(Box::new(refusal_response(dialect, refusal)));
    }
    Ok((key, models))
}

async fn chat(State(gateway): State<Gateway>, headers: HeaderMap, body: Bytes) -> Response {
    let dialect = Dialect::OpenAi;
    let (key, body) = match read(&gateway, &headers, &body, dialect) {
        Ok(read) => read,
        Err(response) => return *response,
    };
    let chat = match openai::decode_request(&body) {
        Ok(chat) => chat,
        Err(err) => {
            return error_response(StatusCode::BAD_REQUEST, "invalid_request", &err.to_string());
        }
    };
    let (key, models) = match admit(&gateway, key, &chat.route, chat.stream, dialect) {
        Ok(admitted) => admitted,
        Err(response) => return *response,
    };
    let id = format!("chatcmpl-{}", uuid::Uuid::now_v7().simple());
    if chat.stream {
        stream(gateway, key, models, chat, id)
    } else {
        complete(gateway, key, models, chat, id).await
    }
}

async fn messages(State(gateway): State<Gateway>, headers: HeaderMap, body: Bytes) -> Response {
    let dialect = Dialect::Anthropic;
    let (key, body) = match read(&gateway, &headers, &body, dialect) {
        Ok(read) => read,
        Err(response) => return *response,
    };
    let request = match anthropic::decode_request(&body) {
        Ok(request) => request,
        Err(err) => return anthropic_error_response(StatusCode::BAD_REQUEST, &err.to_string()),
    };
    let (key, models) = match admit(&gateway, key, &request.route, request.stream, dialect) {
        Ok(admitted) => admitted,
        Err(response) => return *response,
    };
    let id = format!("msg_{}", uuid::Uuid::now_v7().simple());
    if request.stream {
        stream_messages(gateway, key, models, request, id)
    } else {
        complete_messages(gateway, key, models, request, id).await
    }
}

/// Tries the route's models in order; the answer, the model that gave it
/// (or failed last) and how many fell back.
async fn invoke(
    models: &[Arc<dyn Model>],
    request: &agentbuild_providers::ModelRequest,
) -> (Result<ModelResponse, ModelError>, String, u32) {
    let mut fallbacks = 0;
    for (i, model) in models.iter().enumerate() {
        match model.invoke(request).await {
            Err(error) if falls_back(error.kind) && i + 1 < models.len() => fallbacks += 1,
            result => return (result, name_of(model.as_ref()), fallbacks),
        }
    }
    unreachable!("a route has at least one model")
}

async fn complete(
    gateway: Gateway,
    key: Option<String>,
    models: Vec<Arc<dyn Model>>,
    chat: openai::ChatRequest,
    id: String,
) -> Response {
    let started = Instant::now();
    let (result, model, fallbacks) = invoke(&models, &chat.request).await;
    let (usage, outcome) = match &result {
        Ok(response) => (response.usage, "ok".to_string()),
        Err(error) => (Default::default(), error.kind.code().to_string()),
    };
    gateway.charge(key.as_deref(), usage.input_tokens + usage.output_tokens);
    gateway.record(UsageRecord {
        key,
        route: chat.route.clone(),
        model: model.clone(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        latency_ms: started.elapsed().as_millis() as u64,
        outcome,
        fallbacks,
        stream: false,
    });
    let mut response = match result {
        Ok(answer) => axum::Json(openai::encode_response(
            &id,
            now_secs(),
            &chat.route,
            &answer,
        ))
        .into_response(),
        Err(error) => Dialect::OpenAi.model_error(&error),
    };
    if let Ok(value) = HeaderValue::from_str(&model) {
        response.headers_mut().insert("x-agentbuild-model", value);
    }
    response
}

/// Aborts the upstream call when the client goes away (the body stream is
/// dropped).
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn sse(value: &Value) -> Bytes {
    Bytes::from(format!("data: {value}\n\n"))
}

fn stream(
    gateway: Gateway,
    key: Option<String>,
    models: Vec<Arc<dyn Model>>,
    chat: openai::ChatRequest,
    id: String,
) -> Response {
    let (sender, receiver) = mpsc::unbounded_channel::<Bytes>();
    let task = tokio::spawn(async move {
        let started = Instant::now();
        let mut encoder = openai::ChunkEncoder::new(id, now_secs(), chat.route.clone());
        let mut fallbacks = 0;
        let mut last = (Err(None), String::new());
        for (i, model) in models.iter().enumerate() {
            let mut sent = false;
            let result = {
                let sender = &sender;
                let encoder = &mut encoder;
                let sent = &mut sent;
                let mut on_delta = |delta: agentbuild_providers::MessageDelta| {
                    *sent = true;
                    for chunk in encoder.delta(&delta) {
                        let _ = sender.send(sse(&chunk));
                    }
                };
                model.stream(&chat.request, &mut on_delta).await
            };
            let fall_back =
                matches!(&result, Err(e) if falls_back(e.kind) && !sent && i + 1 < models.len());
            last = (result.map_err(Some), name_of(model.as_ref()));
            if fall_back {
                fallbacks += 1;
                continue;
            }
            break;
        }
        let (result, model) = last;
        let (usage, outcome) = match &result {
            Ok(response) => {
                for chunk in encoder.finish(response, chat.include_usage) {
                    let _ = sender.send(sse(&chunk));
                }
                (response.usage, "ok".to_string())
            }
            Err(Some(error)) => {
                // Mid-stream (or before): an error event, as OpenAI sends.
                let body = json!({"error": {
                    "message": error.message, "type": "api_error", "code": error.kind.code(),
                }});
                let _ = sender.send(sse(&body));
                (Default::default(), error.kind.code().to_string())
            }
            Err(None) => (Default::default(), "unknown".to_string()),
        };
        let _ = sender.send(Bytes::from_static(b"data: [DONE]\n\n"));
        gateway.charge(key.as_deref(), usage.input_tokens + usage.output_tokens);
        gateway.record(UsageRecord {
            key,
            route: chat.route.clone(),
            model,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            latency_ms: started.elapsed().as_millis() as u64,
            outcome,
            fallbacks,
            stream: true,
        });
    });
    let body = futures_util::stream::unfold(
        (receiver, AbortOnDrop(task)),
        |(mut receiver, guard)| async move {
            receiver
                .recv()
                .await
                .map(|bytes| (Ok::<_, Infallible>(bytes), (receiver, guard)))
        },
    );
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn complete_messages(
    gateway: Gateway,
    key: Option<String>,
    models: Vec<Arc<dyn Model>>,
    request: anthropic::MessagesRequest,
    id: String,
) -> Response {
    let started = Instant::now();
    let (result, model, fallbacks) = invoke(&models, &request.request).await;
    let (usage, outcome) = match &result {
        Ok(response) => (response.usage, "ok".to_string()),
        Err(error) => (Default::default(), error.kind.code().to_string()),
    };
    gateway.charge(key.as_deref(), usage.input_tokens + usage.output_tokens);
    gateway.record(UsageRecord {
        key,
        route: request.route.clone(),
        model: model.clone(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        latency_ms: started.elapsed().as_millis() as u64,
        outcome,
        fallbacks,
        stream: false,
    });
    let mut response = match result {
        Ok(answer) => {
            axum::Json(anthropic::encode_response(&id, &request.route, &answer)).into_response()
        }
        Err(error) => Dialect::Anthropic.model_error(&error),
    };
    if let Ok(value) = HeaderValue::from_str(&model) {
        response.headers_mut().insert("x-agentbuild-model", value);
    }
    response
}

fn sse_event((name, value): anthropic::Event) -> Bytes {
    Bytes::from(format!("event: {name}\ndata: {value}\n\n"))
}

fn stream_messages(
    gateway: Gateway,
    key: Option<String>,
    models: Vec<Arc<dyn Model>>,
    request: anthropic::MessagesRequest,
    id: String,
) -> Response {
    let (sender, receiver) = mpsc::unbounded_channel::<Bytes>();
    let task = tokio::spawn(async move {
        let started = Instant::now();
        let mut encoder = anthropic::EventEncoder::new(id, request.route.clone());
        let mut fallbacks = 0;
        let mut last = (Err(None), String::new());
        for (i, model) in models.iter().enumerate() {
            let mut sent = false;
            let result = {
                let sender = &sender;
                let encoder = &mut encoder;
                let sent = &mut sent;
                let mut on_delta = |delta: agentbuild_providers::MessageDelta| {
                    *sent = true;
                    for event in encoder.delta(&delta) {
                        let _ = sender.send(sse_event(event));
                    }
                };
                model.stream(&request.request, &mut on_delta).await
            };
            let fall_back =
                matches!(&result, Err(e) if falls_back(e.kind) && !sent && i + 1 < models.len());
            last = (result.map_err(Some), name_of(model.as_ref()));
            if fall_back {
                fallbacks += 1;
                continue;
            }
            break;
        }
        let (result, model) = last;
        let (usage, outcome) = match &result {
            Ok(response) => {
                for event in encoder.finish(response) {
                    let _ = sender.send(sse_event(event));
                }
                (response.usage, "ok".to_string())
            }
            Err(Some(error)) => {
                // Before or after the first event: an `error` event.
                let status = status_for(error.kind);
                let body = json!({"type": "error", "error": {
                    "type": anthropic_error_type(status), "message": error.message,
                }});
                let _ = sender.send(sse_event(("error", body)));
                (Default::default(), error.kind.code().to_string())
            }
            Err(None) => (Default::default(), "unknown".to_string()),
        };
        gateway.charge(key.as_deref(), usage.input_tokens + usage.output_tokens);
        gateway.record(UsageRecord {
            key,
            route: request.route.clone(),
            model,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            latency_ms: started.elapsed().as_millis() as u64,
            outcome,
            fallbacks,
            stream: true,
        });
    });
    let body = futures_util::stream::unfold(
        (receiver, AbortOnDrop(task)),
        |(mut receiver, guard)| async move {
            receiver
                .recv()
                .await
                .map(|bytes| (Ok::<_, Infallible>(bytes), (receiver, guard)))
        },
    );
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
