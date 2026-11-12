//! Support chat demo server: an CriogenAI agent for "Mugshot", a small
//! coffee gear shop, behind a JSON API for the Next.js UI in `../web`.
//!
//! - conversations are CriogenAI threads in a SQLite file (`support.db`)
//! - refunds wait for a human (approve / reject in the UI)
//! - a run that is killed (stop the server mid-answer) continues with retry
//! - each request may pick the model: OpenAI or Gemini, same thread
//!
//! Keys come from the environment (`OPENAI_API_KEY`, `GEMINI_API_KEY`), or
//! from the file in `CRIOGEN_KEYS_FILE` (default
//! `~/.criogen-keys.env`, `KEY=value` lines). They are never logged or
//! sent anywhere but the matching provider.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use criogenai_agent::{
    Agent, AgentError, Approval, ApprovalRule, Decision, Reply, Thread, ToolError, tool_fn,
};
use criogenai_providers::{Endpoint, ProviderModel, Role, ToolSpec};
use criogenai_storage::Storage;
use serde::Deserialize;
use serde_json::{Map, Value, json};

const INSTRUCTIONS: &str = "You are the customer support agent for Mugshot, a small online shop \
for coffee gear. Be brief and friendly. Always look orders up with lookup_order before you say \
anything about them; never guess order details. Check stock with check_stock. If an item \
arrived damaged or the customer is owed money, call issue_refund with the order id, the amount \
in US dollars and a short reason: a human reviews every refund before it is paid, so tell the \
customer it is pending if it has not been approved yet. If a refund is rejected, explain \
politely and offer alternatives.";

/// One configured model.
struct Provider {
    label: String,
    model: String,
    agent: Agent,
}

struct App {
    providers: BTreeMap<String, Provider>,
    default: String,
    refunds: Refunds,
}

type Refunds = Arc<Mutex<Vec<Value>>>;

fn main() {
    // Before the runtime starts any threads (`set_var` is not thread-safe).
    load_keys_file();
    tokio::runtime::Runtime::new()
        .expect("Tokio runtime")
        .block_on(serve());
}

async fn serve() {
    let storage =
        Storage::connect(&std::env::var("SUPPORT_DB").unwrap_or("sqlite://support.db".into()))
            .await
            .unwrap_or_else(|err| panic!("cannot open the database: {err}"));
    let refunds = Refunds::default();

    let mut providers = BTreeMap::new();
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        let model = std::env::var("OPENAI_MODEL").unwrap_or("gpt-5-mini".into());
        let llm = ProviderModel::new(Endpoint::openai(key), &model).expect("HTTP client");
        providers.insert(
            "openai".to_string(),
            Provider {
                label: "OpenAI".into(),
                agent: build_agent(llm, &storage, &refunds),
                model,
            },
        );
    }
    if let Ok(key) = std::env::var("GEMINI_API_KEY") {
        let model = std::env::var("GEMINI_MODEL").unwrap_or("gemini-2.5-flash".into());
        let endpoint = Endpoint::openai_compatible(
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta/openai",
            Some(key),
        );
        let llm = ProviderModel::new(endpoint, &model).expect("HTTP client");
        providers.insert(
            "gemini".to_string(),
            Provider {
                label: "Gemini".into(),
                agent: build_agent(llm, &storage, &refunds),
                model,
            },
        );
    }
    let Some(default) = providers.keys().next().cloned() else {
        eprintln!("No model configured: set OPENAI_API_KEY and/or GEMINI_API_KEY.");
        std::process::exit(1);
    };
    for (id, p) in &providers {
        println!("model {id}: {} {}", p.label, p.model);
    }

    let app = Arc::new(App {
        providers,
        default,
        refunds,
    });
    let router = Router::new()
        .route("/api/config", get(config))
        .route("/api/threads/{id}", get(show).delete(remove))
        .route("/api/threads/{id}/messages", post(send))
        .route("/api/threads/{id}/approve", post(approve))
        .route("/api/threads/{id}/reject", post(reject))
        .route("/api/threads/{id}/retry", post(retry))
        .route("/api/threads/{id}/rewind", post(rewind))
        .with_state(app);
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8787);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    println!("support-chat server on http://{addr}");
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("serve");
}

/// The agent: three tools on a pretend order system; refunds need approval.
fn build_agent(model: ProviderModel, storage: &Storage, refunds: &Refunds) -> Agent {
    let lookup = tool_fn(
        ToolSpec::new(
            "lookup_order",
            "Look up an order by id (like A-1001): items, price, status.",
            json!({
                "type": "object",
                "properties": {"order_id": {"type": "string", "description": "e.g. A-1001"}},
                "required": ["order_id"]
            }),
        ),
        |args, _ctx| async move {
            let id = args["order_id"].as_str().unwrap_or_default().to_uppercase();
            match order(&id) {
                Some(order) => Ok(order),
                None => Err(ToolError::new(format!("no order `{id}`"))),
            }
        },
    );
    let stock = tool_fn(
        ToolSpec::new(
            "check_stock",
            "How many of a product are in stock.",
            json!({
                "type": "object",
                "properties": {"product": {"type": "string"}},
                "required": ["product"]
            }),
        ),
        |args, _ctx| async move {
            let product = args["product"].as_str().unwrap_or_default().to_lowercase();
            let count = if product.contains("mug") {
                12
            } else if product.contains("grinder") {
                0
            } else if product.contains("bean") {
                40
            } else {
                3
            };
            Ok::<_, ToolError>(json!({"product": product, "in_stock": count}))
        },
    );
    let paid = refunds.clone();
    let refund = tool_fn(
        ToolSpec::new(
            "issue_refund",
            "Refund money for an order. A human approves it first.",
            json!({
                "type": "object",
                "properties": {
                    "order_id": {"type": "string"},
                    "amount": {"type": "number", "description": "US dollars"},
                    "reason": {"type": "string"}
                },
                "required": ["order_id", "amount", "reason"]
            }),
        ),
        move |args, ctx| {
            let paid = paid.clone();
            async move {
                let mut paid = paid.lock().unwrap();
                let id = format!("RF-{}", 501 + paid.len());
                paid.push(json!({"refund_id": id, "call_id": ctx.call_id, "args": args}));
                println!("payments: refund {id} paid ({} calls so far)", paid.len());
                Ok::<_, ToolError>(json!({"refund_id": id, "status": "paid"}))
            }
        },
    );
    let describe = |_: &str, args: &Map<String, Value>| {
        format!(
            "Refund ${} for order {}: {}",
            args.get("amount").map(Value::to_string).unwrap_or_default(),
            args.get("order_id").and_then(Value::as_str).unwrap_or("?"),
            args.get("reason")
                .and_then(Value::as_str)
                .unwrap_or("no reason given"),
        )
    };
    Agent::builder(model)
        .instructions(INSTRUCTIONS)
        .tool(lookup)
        .tool(stock)
        .tool(refund)
        .approval(Approval::new().rule(ApprovalRule::new("issue_refund").describe(describe)))
        .storage(storage.clone())
        .build()
        .expect("valid agent")
}

fn order(id: &str) -> Option<Value> {
    Some(match id {
        "A-1001" => json!({
            "order_id": id, "items": ["Blue ceramic mug"], "total_usd": 24.0,
            "status": "delivered", "delivered": "2026-10-09"
        }),
        "A-1002" => json!({
            "order_id": id, "items": ["Ethiopia Yirgacheffe beans, 1 kg"], "total_usd": 38.5,
            "status": "shipped", "carrier": "UPS", "tracking": "1Z999AA10123456784",
            "expected": "2026-10-14"
        }),
        "A-1003" => json!({
            "order_id": id, "items": ["Burr grinder"], "total_usd": 89.0,
            "status": "processing", "note": "waiting for stock"
        }),
        _ => return None,
    })
}

// ---- HTTP handlers --------------------------------------------------------

#[derive(Deserialize, Default)]
struct Body {
    text: Option<String>,
    message: Option<String>,
    provider: Option<String>,
    checkpoint_id: Option<String>,
}

type Shared = State<Arc<App>>;

async fn config(State(app): Shared) -> Json<Value> {
    let providers: Vec<Value> = app
        .providers
        .iter()
        .map(|(id, p)| json!({"id": id, "label": p.label, "model": p.model}))
        .collect();
    Json(json!({"providers": providers, "default": app.default}))
}

impl App {
    fn thread(&self, id: &str, provider: Option<&str>) -> Result<(Thread, &Provider), ApiError> {
        let key = provider.unwrap_or(&self.default);
        let provider = self.providers.get(key).ok_or_else(|| ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "unknown_provider".into(),
            message: format!("no model `{key}` is configured"),
        })?;
        Ok((provider.agent.thread(id), provider))
    }
}

async fn show(State(app): Shared, Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    let (thread, _) = app.thread(&id, None)?;
    Ok(Json(view(&thread, None, &app).await?))
}

async fn remove(State(app): Shared, Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    let (thread, _) = app.thread(&id, None)?;
    let existed = thread.delete().await?;
    Ok(Json(json!({"deleted": existed})))
}

async fn send(
    State(app): Shared,
    Path(id): Path<String>,
    Json(body): Json<Body>,
) -> Result<Json<Value>, ApiError> {
    let text = body.text.unwrap_or_default();
    if text.trim().is_empty() {
        return Err(ApiError::bad("empty_message", "type a message first"));
    }
    let (thread, provider) = app.thread(&id, body.provider.as_deref())?;
    let started = Instant::now();
    let reply = thread.send(text.as_str()).await;
    respond(&thread, provider, reply, started, &app).await
}

async fn approve(
    State(app): Shared,
    Path(id): Path<String>,
    body: Option<Json<Body>>,
) -> Result<Json<Value>, ApiError> {
    let body = body.map(|b| b.0).unwrap_or_default();
    let (thread, provider) = app.thread(&id, body.provider.as_deref())?;
    let started = Instant::now();
    let reply = thread.approve().await;
    respond(&thread, provider, reply, started, &app).await
}

async fn reject(
    State(app): Shared,
    Path(id): Path<String>,
    Json(body): Json<Body>,
) -> Result<Json<Value>, ApiError> {
    let (thread, provider) = app.thread(&id, body.provider.as_deref())?;
    let message = body
        .message
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| "The reviewer rejected this refund.".into());
    let started = Instant::now();
    let pending = thread.pending_approvals().await?;
    let decisions = pending
        .iter()
        .map(|r| (r.call_id.clone(), Decision::reject(message.clone())));
    let reply = thread.decide(decisions).await;
    respond(&thread, provider, reply, started, &app).await
}

async fn retry(
    State(app): Shared,
    Path(id): Path<String>,
    body: Option<Json<Body>>,
) -> Result<Json<Value>, ApiError> {
    let body = body.map(|b| b.0).unwrap_or_default();
    let (thread, provider) = app.thread(&id, body.provider.as_deref())?;
    let started = Instant::now();
    let reply = thread.retry().await;
    respond(&thread, provider, reply, started, &app).await
}

async fn rewind(
    State(app): Shared,
    Path(id): Path<String>,
    Json(body): Json<Body>,
) -> Result<Json<Value>, ApiError> {
    let (thread, _) = app.thread(&id, None)?;
    let target = body
        .checkpoint_id
        .ok_or_else(|| ApiError::bad("missing_checkpoint", "checkpoint_id is required"))?;
    thread.rewind(&target).await?;
    Ok(Json(view(&thread, None, &app).await?))
}

/// The thread's view after a call. A failed run is still shown (its state
/// is saved); the error goes in `error`.
async fn respond(
    thread: &Thread,
    provider: &Provider,
    reply: Result<Reply, AgentError>,
    started: Instant,
    app: &App,
) -> Result<Json<Value>, ApiError> {
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let last = match &reply {
        Ok(reply) => json!({
            "ok": true,
            "provider": provider.label,
            "model": provider.model,
            "elapsed_ms": elapsed_ms,
            "model_calls": reply.result.model_calls,
            "tool_calls": reply.result.tool_calls,
            "usage": reply.result.usage,
            "version": reply.version,
        }),
        Err(error) => {
            // Errors before anything was saved (busy, invalid input) are
            // plain HTTP errors.
            if error.snapshot().is_none() {
                return Err(ApiError::from(error.clone()));
            }
            println!("run failed: {} ({})", error.code, provider.label);
            json!({
                "ok": false,
                "provider": provider.label,
                "model": provider.model,
                "elapsed_ms": elapsed_ms,
                "error": {"code": error.code, "message": error.message},
            })
        }
    };
    Ok(Json(view(thread, Some(last), app).await?))
}

async fn view(thread: &Thread, last: Option<Value>, app: &App) -> Result<Value, ApiError> {
    let head = thread.head().await?;
    let messages: Vec<Value> = match &head {
        Some(_) => thread
            .messages()
            .await?
            .into_iter()
            .filter(|m| m.role != Role::System)
            .map(|m| serde_json::to_value(m).unwrap_or(Value::Null))
            .collect(),
        None => Vec::new(),
    };
    let history = thread.history(50).await?;
    let refunds: Vec<Value> = app.refunds.lock().unwrap().clone();
    Ok(json!({
        "id": thread.id(),
        "status": head.as_ref().map(|h| h.status()),
        "version": head.as_ref().map(|h| h.version),
        "messages": messages,
        "approvals": thread.pending_approvals().await?,
        "history": history,
        "last": last,
        "refunds_paid": refunds.len(),
    }))
}

// ---- errors ---------------------------------------------------------------

struct ApiError {
    status: StatusCode,
    code: String,
    message: String,
}

impl ApiError {
    fn bad(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: code.into(),
            message: message.into(),
        }
    }
}

impl From<AgentError> for ApiError {
    fn from(error: AgentError) -> Self {
        let status = match error.code.as_str() {
            "thread_busy" | "thread_suspended" | "thread_interrupted" | "resume_invalid" => {
                StatusCode::CONFLICT
            }
            "checkpoint_not_found" => StatusCode::NOT_FOUND,
            "storage_unavailable" | "storage_failed" => StatusCode::SERVICE_UNAVAILABLE,
            "model_failed" => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            code: error.code,
            message: error.message,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({"error": {"code": self.code, "message": self.message}});
        (self.status, Json(body)).into_response()
    }
}

/// Reads `KEY=value` lines into the environment, for keys not already set.
/// Values are never printed.
fn load_keys_file() {
    let path = std::env::var("CRIOGEN_KEYS_FILE").ok().or_else(|| {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()?;
        Some(format!("{home}/.criogen-keys.env"))
    });
    let Some(text) = path.and_then(|p| std::fs::read_to_string(p).ok()) else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let (key, value) = (key.trim(), value.trim().trim_matches('"'));
            if !key.is_empty() && !value.is_empty() && std::env::var_os(key).is_none() {
                // SAFETY: called from `main` before the runtime (or any
                // other thread) starts.
                unsafe { std::env::set_var(key, value) };
            }
        }
    }
}
