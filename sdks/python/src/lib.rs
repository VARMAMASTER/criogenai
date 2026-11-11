//! The Python SDK's native module `criogen._native` (design 0011): a
//! bridge from asyncio to the Rust engine. Python builds the configuration
//! (as JSON) and holds the user's tools; the agent loop, validation,
//! storage and approvals run here.
//!
//! Every call that waits returns an asyncio future that a task on this
//! module's Tokio runtime completes with `loop.call_soon_threadsafe`, so
//! the GIL is never held while Rust waits.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use criogen_agent::mcp::{McpClient, McpError, McpServer};
use criogen_agent::{
    Agent, AgentError, AgentEvent, AgentInput, AgentResult, Approval, Decision, Hook, HookAction,
    HookContext, HookError, Limits, Memory, OnToolException, Reply, ResilientModel, Thread,
    ThreadStream, ToolCallRequest, ToolContext, ToolError, ToolNext, ToolOutput, tool_fn,
    validate_arguments,
};
use criogen_core::{
    CancelToken, Node, NodeContext, NodeError, NodeOutput, Reducer, RetryPolicy, Router, RunError,
    RunErrorKind, RunOptions, RunOutput, Snapshot, State, Update, Workflow, WorkflowBuilder,
    node_with_context, router_fn,
};
use criogen_providers::{
    AzureApi, AzureCredential, AzureModel, BlockDelta, BoxFuture, Embedder, Endpoint, FakeEmbedder,
    FakeModel, FakeReply, KaggleModel, Message, MessageDelta, Model, ModelError, ModelErrorKind,
    ModelRequest, ModelResponse, OutputSpec, ProviderEmbedder, ProviderModel, Settings,
    ToolAnnotations, ToolSpec, preset,
};
use criogen_storage::{
    AuditQuery, Keyring, Namespace, OnBusy, Retention, SearchQuery, Storage, ThreadError, Turn,
    WorkflowThread,
};
use criogen_telemetry::{Langfuse, OtlpConfig, OtlpExporter};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyTuple};
use serde_json::{Map, Value, json};

// ---------- runtime and registries ----------

/// The run metadata key that names the caller's event loop (an id into
/// [`loops`]), so async tools run on it.
const LOOP_KEY: &str = "criogen_python_loop";

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("criogen")
            .build()
            .expect("the criogen Tokio runtime starts")
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Event loops of calls in flight, by id.
fn loops() -> &'static Mutex<HashMap<u64, Arc<Py<PyAny>>>> {
    static LOOPS: OnceLock<Mutex<HashMap<u64, Arc<Py<PyAny>>>>> = OnceLock::new();
    LOOPS.get_or_init(Mutex::default)
}

/// A registered event loop; unregistered when dropped.
struct LoopId(u64);

impl LoopId {
    fn register(event_loop: &Bound<'_, PyAny>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        lock(loops()).insert(id, Arc::new(event_loop.clone().unbind()));
        Self(id)
    }
}

impl Drop for LoopId {
    fn drop(&mut self) {
        lock(loops()).remove(&self.0);
    }
}

/// The caller's event loop of a run, from its metadata.
fn loop_of(metadata: Option<&Value>) -> Option<Arc<Py<PyAny>>> {
    let id = metadata?.get(LOOP_KEY)?.as_u64()?;
    lock(loops()).get(&id).cloned()
}

/// Exceptions raised by tools (other than `ToolError`), by call id, to
/// become the `__cause__` of the `tool_failed` error.
fn tool_exceptions() -> &'static Mutex<HashMap<String, Py<PyAny>>> {
    static EXCEPTIONS: OnceLock<Mutex<HashMap<String, Py<PyAny>>>> = OnceLock::new();
    EXCEPTIONS.get_or_init(Mutex::default)
}

// ---------- results and errors ----------

/// A failure on its way to Python: a stable code (0003), a message, details.
struct Failure {
    code: String,
    message: String,
    details: Option<Value>,
}

impl Failure {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            details: None,
        }
    }
}

impl From<AgentError> for Failure {
    fn from(error: AgentError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            details: error.details,
        }
    }
}

impl From<criogen_storage::StorageError> for Failure {
    fn from(error: criogen_storage::StorageError) -> Self {
        Self::new(error.code(), error.message)
    }
}

type Outcome = Result<Value, Failure>;

/// Runs `work` on the runtime and returns an asyncio future on
/// `event_loop` that completes with its result. Cancelling the asyncio
/// future fires `cancel`.
fn spawn<'py, F>(
    py: Python<'py>,
    event_loop: &Bound<'py, PyAny>,
    cancel: Option<CancelToken>,
    work: F,
) -> PyResult<Bound<'py, PyAny>>
where
    F: Future<Output = Outcome> + Send + 'static,
{
    let future = event_loop.call_method0("create_future")?;
    if let Some(token) = cancel {
        let on_done = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>, _: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
                if args.get_item(0)?.call_method0("cancelled")?.is_truthy()? {
                    token.cancel();
                }
                Ok(())
            },
        )?;
        future.call_method1("add_done_callback", (on_done,))?;
    }
    let future_ref = future.clone().unbind();
    let loop_ref = event_loop.clone().unbind();
    runtime().spawn(async move {
        let outcome = work.await;
        Python::attach(|py| {
            // A closed loop has nobody waiting for the result.
            let _ = deliver(py, &loop_ref, &future_ref, outcome);
        });
    });
    Ok(future)
}

fn deliver(
    py: Python<'_>,
    event_loop: &Py<PyAny>,
    future: &Py<PyAny>,
    outcome: Outcome,
) -> PyResult<()> {
    let bridge = py.import("criogen._bridge")?;
    match outcome {
        Ok(value) => {
            event_loop.call_method1(
                py,
                "call_soon_threadsafe",
                (bridge.getattr("resolve")?, future, value.to_string()),
            )?;
        }
        Err(failure) => {
            // A tool's exception by call id; a node's or a hook's by the
            // `cause_key` of its failure (unique to it).
            let key = failure.details.as_ref().and_then(|d| {
                d.get("call_id")
                    .or_else(|| d.get("cause_key"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
            let cause = key.and_then(|key| lock(tool_exceptions()).remove(&key));
            event_loop.call_method1(
                py,
                "call_soon_threadsafe",
                (
                    bridge.getattr("reject")?,
                    future,
                    failure.code,
                    failure.message,
                    failure.details.map(|d| d.to_string()),
                    cause,
                ),
            )?;
        }
    }
    Ok(())
}

fn to_json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn result_json(result: &AgentResult) -> Value {
    json!({
        "text": result.text(),
        "output": result.output,
        "messages": to_json(&result.messages),
        "usage": to_json(&result.usage),
        "stop_reason": to_json(&result.stop_reason),
        "model_calls": result.model_calls,
        "tool_calls": result.tool_calls,
        "needs_approval": result.is_suspended(),
        "approvals": to_json(&result.approval_requests()),
        "run_id": result.run.run_id,
    })
}

fn reply_json(reply: &Reply) -> Value {
    let mut value = result_json(&reply.result);
    value["version"] = json!(reply.version);
    value["checkpoint_id"] = json!(reply.checkpoint_id);
    value
}

fn event_json(event: AgentEvent) -> Value {
    match event {
        AgentEvent::Text { message_id, text } => {
            json!({"type": "text", "message_id": message_id, "text": text})
        }
        AgentEvent::ToolCallStarted {
            call_id,
            tool,
            args,
        } => json!({"type": "tool_call_started", "call_id": call_id, "tool": tool, "args": args}),
        AgentEvent::ToolCallFinished {
            call_id,
            tool,
            status,
        } => json!({
            "type": "tool_call_finished", "call_id": call_id, "tool": tool,
            "status": to_json(&status),
        }),
        AgentEvent::Message(message) => json!({"type": "message", "message": to_json(&message)}),
        AgentEvent::ApprovalNeeded(requests) => {
            json!({"type": "approval_needed", "requests": to_json(&requests)})
        }
        other => json!({"type": "other", "event": format!("{other:?}")}),
    }
}

// ---------- configuration ----------

fn bad(message: impl Into<String>) -> Failure {
    Failure::new("invalid_value", message)
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn model(spec: &Value) -> Result<Arc<dyn Model>, Failure> {
    let kind = text(spec, "kind").ok_or_else(|| bad("a model needs a `kind`"))?;
    if kind == "fake" {
        let delay =
            Duration::from_millis(spec.get("delay_ms").and_then(Value::as_u64).unwrap_or(0));
        // Python cannot read the request log: keep none.
        let mut fake = FakeModel::with_delay(delay, delay).without_request_log();
        if let Some(calls) = spec.get("fail_first").and_then(Value::as_u64) {
            fake = fake.flaky_first(calls as usize);
        }
        if spec.get("by_turn").and_then(Value::as_bool) == Some(true) {
            fake = fake.by_turn();
        }
        if let Some(name) = text(spec, "name") {
            fake = fake.named("fake", name);
        }
        for reply in spec["replies"].as_array().cloned().unwrap_or_default() {
            fake = fake.then(fake_reply(&reply)?);
        }
        return Ok(Arc::new(fake));
    }
    let name = text(spec, "model").ok_or_else(|| bad("a model needs a `model` name"))?;
    if kind == "kaggle" {
        return kaggle_model(spec, name);
    }
    if kind == "azure" {
        return azure_model(spec, name);
    }
    ProviderModel::new(endpoint(spec)?, name)
        .map(|m| Arc::new(m) as Arc<dyn Model>)
        .map_err(|err| Failure::new("model_failed", err.to_string()))
}

/// Azure OpenAI and Foundry (design 0006 §17): `endpoint` (resource name or
/// URL, else AZURE_OPENAI_ENDPOINT); `api_key`, `token`, `tenant_id` +
/// `client_id` + `client_secret`, or `managed_identity`, else the
/// environment; `api` (`"responses"` or `"chat"`), `scope`; `authority` and
/// `identity_endpoint` point token requests elsewhere (tests).
fn azure_model(spec: &Value, name: &str) -> Result<Arc<dyn Model>, Failure> {
    let env = |var: &str| std::env::var(var).ok().filter(|v| !v.trim().is_empty());
    let credential = if let Some(key) = text(spec, "api_key") {
        Some(AzureCredential::api_key(key))
    } else if let Some(token) = text(spec, "token") {
        Some(AzureCredential::token(token))
    } else if let Some(secret) = text(spec, "client_secret") {
        let tenant = text(spec, "tenant_id")
            .map(str::to_string)
            .or_else(|| env("AZURE_TENANT_ID"));
        let client = text(spec, "client_id")
            .map(str::to_string)
            .or_else(|| env("AZURE_CLIENT_ID"));
        let (Some(tenant), Some(client)) = (tenant, client) else {
            return Err(bad(
                "an Azure client secret needs `tenant_id` and `client_id`",
            ));
        };
        Some(AzureCredential::client_secret(tenant, client, secret))
    } else if spec.get("managed_identity").and_then(Value::as_bool) == Some(true) {
        Some(AzureCredential::managed_identity(
            text(spec, "client_id").map(str::to_string),
        ))
    } else {
        None
    };
    let mut model = AzureModel::from_parts(text(spec, "endpoint"), name, credential)
        .map_err(|err| Failure::new("model_failed", err.to_string()))?;
    if text(spec, "api") == Some("chat") {
        model = model.with_api(AzureApi::Chat);
    }
    if let Some(scope) = text(spec, "scope") {
        model = model.with_scope(scope);
    }
    if let Some(authority) = text(spec, "authority") {
        model = model.with_authority(authority);
    }
    if let Some(url) = text(spec, "identity_endpoint") {
        model =
            model.with_identity_endpoint(url, text(spec, "identity_header").map(str::to_string));
    }
    Ok(Arc::new(model))
}

/// Kaggle's Model Proxy (design 0020): `api_key` (a Kaggle API token), or
/// `proxy_url` and `proxy_token` (a pre-minted proxy token), else the
/// environment; `api_base` points the token minting elsewhere (tests).
fn kaggle_model(spec: &Value, name: &str) -> Result<Arc<dyn Model>, Failure> {
    let built = match (
        text(spec, "proxy_url"),
        text(spec, "proxy_token"),
        text(spec, "api_key"),
    ) {
        (Some(url), Some(token), _) => KaggleModel::from_proxy(url, token, name),
        (_, _, Some(key)) => Ok(KaggleModel::new(key, name)),
        _ => KaggleModel::from_env(name),
    };
    let mut model = built.map_err(|err| Failure::new("model_failed", err.to_string()))?;
    if let Some(base) = text(spec, "api_base") {
        model = model.with_api_base(base);
    }
    Ok(Arc::new(model))
}

/// The provider endpoint of a model or embedder spec (`kind`, `api_key`,
/// `base_url`).
fn endpoint(spec: &Value) -> Result<Endpoint, Failure> {
    let kind = text(spec, "kind").ok_or_else(|| bad("a model needs a `kind`"))?;
    let key = text(spec, "api_key").map(str::to_string);
    let mut endpoint = match kind {
        "openai_compatible" => Endpoint::openai_compatible(
            text(spec, "name").unwrap_or("custom"),
            text(spec, "base_url").ok_or_else(|| bad("`openai_compatible` needs a `base_url`"))?,
            key.clone(),
        ),
        "anthropic_compatible" => Endpoint::anthropic_compatible(
            text(spec, "name").unwrap_or("custom"),
            text(spec, "base_url")
                .ok_or_else(|| bad("`anthropic_compatible` needs a `base_url`"))?,
            key.clone(),
        ),
        // Every named provider (design 0006 §4.1): one table in criogen-providers.
        named => match preset(named) {
            Some(p) => p.endpoint(key.clone()).map_err(|err| bad(err.message))?,
            None => return Err(bad(format!("unknown model kind `{named}`"))),
        },
    };
    if let Some(base) = text(spec, "base_url")
        && !kind.ends_with("_compatible")
    {
        endpoint = endpoint.with_base_url(base);
    }
    if spec["allow_insecure_http"].as_bool() == Some(true) {
        endpoint = endpoint.allow_insecure_http();
    }
    Ok(endpoint)
}

/// An embedder (design 0017 §3): a provider's `/embeddings`, or the fake
/// one for tests (`{"kind": "fake", "synonyms": [[word, same_as]],
/// "failing": bool}`).
fn embedder(spec: &Value) -> Result<Arc<dyn Embedder>, Failure> {
    if text(spec, "kind") == Some("fake") {
        if spec["failing"].as_bool() == Some(true) {
            return Ok(Arc::new(FakeEmbedder::failing()));
        }
        let mut fake = FakeEmbedder::new();
        for pair in spec["synonyms"].as_array().cloned().unwrap_or_default() {
            fake = fake.synonym(
                pair[0].as_str().unwrap_or_default(),
                pair[1].as_str().unwrap_or_default(),
            );
        }
        return Ok(Arc::new(fake));
    }
    let name = text(spec, "model").ok_or_else(|| bad("an embedder needs a `model` name"))?;
    let mut embedder = ProviderEmbedder::new(endpoint(spec)?, name)
        .map_err(|err| Failure::new("model_failed", err.to_string()))?;
    if let Some(n) = spec["dimensions"].as_u64() {
        embedder = embedder.dimensions(n as u32);
    }
    Ok(Arc::new(embedder))
}

fn fake_reply(reply: &Value) -> Result<FakeReply, Failure> {
    if let Some(text) = text(reply, "text") {
        return Ok(FakeReply::text(text));
    }
    if let Some(calls) = reply.get("tool_calls").and_then(Value::as_array) {
        let calls: Vec<(String, Value)> = calls
            .iter()
            .map(|c| {
                (
                    c["name"].as_str().unwrap_or_default().to_string(),
                    c["args"].clone(),
                )
            })
            .collect();
        return Ok(FakeReply::tool_calls(calls));
    }
    Err(bad(
        "a fake reply is {\"text\": …} or {\"tool_calls\": […]}",
    ))
}

async fn storage(spec: &Value) -> Result<Storage, Failure> {
    let mut storage = match text(spec, "url") {
        None => Storage::in_memory(),
        Some(url) => match text(spec, "encryption_keys") {
            Some(keys) => Storage::connect_encrypted(url, Keyring::parse(keys)?).await?,
            None => Storage::connect(url).await?,
        },
    };
    if spec["audit"].as_bool() == Some(true) {
        storage = storage.with_audit();
    }
    if let Some(on_busy) = text(spec, "on_busy") {
        storage = storage.on_busy(match on_busy {
            "wait" => OnBusy::Wait,
            "reject" => OnBusy::Reject,
            "interrupt" => OnBusy::Interrupt,
            other => return Err(bad(format!("unknown on_busy `{other}`"))),
        });
    }
    if let Some(secs) = spec["busy_timeout_secs"].as_f64() {
        storage = storage.busy_timeout(Duration::from_secs_f64(secs));
    }
    if let Some(retention) = spec.get("retention").filter(|r| r.is_object()) {
        let mut rules = Retention::new();
        if let Some(secs) = retention["thread_ttl_secs"].as_f64() {
            rules = rules.thread_ttl(Duration::from_secs_f64(secs));
        }
        if let Some(n) = retention["keep_checkpoints"].as_u64() {
            rules = rules.keep_checkpoints(n as usize);
        }
        if retention["drop_branches"].as_bool() == Some(true) {
            rules = rules.drop_branches();
        }
        storage = storage.retention(rules);
    }
    Ok(storage)
}

fn memory(spec: &Value) -> Result<Memory, Failure> {
    let namespace = |path: &str| Namespace::parse(path).map_err(Failure::from);
    let mut memory = if let Some(key) = text(spec, "by_state") {
        Memory::by_state(key, namespace(text(spec, "prefix").unwrap_or(""))?)
    } else if let Some(shared) = text(spec, "shared") {
        Memory::shared(namespace(shared)?)
    } else {
        return Err(bad("memory needs `by_state` or `shared`"));
    };
    if let Some(n) = spec["recall"].as_u64() {
        memory = memory.recall(n as usize);
    }
    if let Some(on) = spec["tools"].as_bool() {
        memory = memory.tools(on);
    }
    if let Some(secs) = spec["ttl_secs"].as_f64() {
        memory = memory.ttl(Duration::from_secs_f64(secs));
    }
    if let Some(spec) = spec.get("embedder").filter(|e| e.is_object()) {
        memory = memory.semantic_shared(embedder(spec)?);
    }
    Ok(memory)
}

/// A retry policy from `{max_attempts, initial_backoff_secs, factor,
/// max_backoff_secs, jitter}` (missing keys keep Rust's defaults).
fn retry_policy(spec: &Value) -> RetryPolicy {
    let defaults = RetryPolicy::default();
    let mut policy = RetryPolicy::new();
    if let Some(n) = spec["max_attempts"].as_u64() {
        policy = policy.max_attempts(n as u32);
    }
    let initial = spec["initial_backoff_secs"].as_f64().map_or(
        Duration::from_millis(defaults.initial_backoff_ms),
        Duration::from_secs_f64,
    );
    let factor = spec["factor"].as_f64().unwrap_or(defaults.backoff_factor);
    let max = spec["max_backoff_secs"].as_f64().map_or(
        Duration::from_millis(defaults.max_backoff_ms),
        Duration::from_secs_f64,
    );
    policy = policy.backoff(initial, factor, max);
    if let Some(jitter) = spec["jitter"].as_bool() {
        policy = policy.jitter(jitter);
    }
    policy
}

fn limits(spec: &Value) -> Result<Limits, Failure> {
    let mut limits = Limits::default();
    if let Some(n) = spec["model_calls"].as_u64() {
        limits = limits.model_calls(n as u32);
    }
    if let Some(n) = spec["tool_calls"].as_u64() {
        limits = limits.tool_calls(n as u32);
    }
    if let Some(n) = spec["tokens"].as_u64() {
        limits = limits.tokens(n);
    }
    if let Some(secs) = spec["tool_timeout_secs"].as_f64() {
        limits = limits.tool_timeout(Duration::from_secs_f64(secs));
    }
    if let Some(policy) = spec.get("on_tool_exception").filter(|p| !p.is_null()) {
        let policy: OnToolException = serde_json::from_value(policy.clone()).map_err(|_| {
            bad("on_tool_exception is \"return_to_model\", \"return_generic\" or \"fail\"")
        })?;
        limits = limits.on_tool_exception(policy);
    }
    Ok(limits)
}

// ---------- tools ----------

/// What the Python adapter of a tool answered.
enum Answer {
    /// The adapter's JSON envelope: `{"ok": value}` or `{"tool_error": …}`.
    Envelope(String),
    /// An exception other than `ToolError`, and its text.
    Raised(Py<PyAny>, String),
}

fn raised(py: Python<'_>, err: PyErr) -> Answer {
    let text = match err.get_type(py).name() {
        Ok(name) => format!("{name}: {}", err.value(py)),
        Err(_) => err.value(py).to_string(),
    };
    Answer::Raised(err.value(py).clone().into_any().unbind(), text)
}

/// Calls `adapter(a, b)`: its JSON envelope, or (async adapters) the
/// envelope of the coroutine it returns, run on `event_loop`.
async fn call_python(
    adapter: Arc<Py<PyAny>>,
    is_async: bool,
    a: String,
    b: String,
    event_loop: Option<Arc<Py<PyAny>>>,
) -> Result<Answer, String> {
    if is_async {
        let event_loop = event_loop.ok_or_else(|| {
            "an async Python function needs the caller's event loop: use the async API \
             (or run_sync), not a run started elsewhere"
                .to_string()
        })?;
        let (sender, receiver) = tokio::sync::oneshot::channel::<Answer>();
        let sender = Mutex::new(Some(sender));
        let scheduled = Python::attach(|py| -> PyResult<()> {
            let coroutine = adapter.call1(py, (a, b))?;
            let running = py
                .import("asyncio")?
                .call_method1("run_coroutine_threadsafe", (coroutine, event_loop.bind(py)))?;
            let on_done = PyCFunction::new_closure(
                py,
                None,
                None,
                move |args: &Bound<'_, PyTuple>, _: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
                    let done = args.get_item(0)?;
                    let answer = match done
                        .call_method0("result")
                        .and_then(|v| v.extract::<String>())
                    {
                        Ok(envelope) => Answer::Envelope(envelope),
                        Err(err) => raised(done.py(), err),
                    };
                    if let Some(sender) = lock(&sender).take() {
                        let _ = sender.send(answer);
                    }
                    Ok(())
                },
            )?;
            running.call_method1("add_done_callback", (on_done,))?;
            Ok(())
        });
        match scheduled {
            Ok(()) => receiver
                .await
                .map_err(|_| "the Python function was abandoned".to_string()),
            Err(err) => Ok(Python::attach(|py| raised(py, err))),
        }
    } else {
        tokio::task::spawn_blocking(move || {
            Python::attach(|py| {
                match adapter
                    .call1(py, (a, b))
                    .and_then(|v| v.extract::<String>(py))
                {
                    Ok(envelope) => Answer::Envelope(envelope),
                    Err(err) => raised(py, err),
                }
            })
        })
        .await
        .map_err(|_| "the Python function panicked".to_string())
    }
}

/// Calls a Python tool: `adapter(args_json, ctx_json)`.
/// Calls a Python tool. An exception it raises is a crash
/// (`on_tool_exception` decides); with `keep_cause` (the run fails on it)
/// the exception object is kept to become the error's cause.
async fn call_tool(
    adapter: Arc<Py<PyAny>>,
    is_async: bool,
    keep_cause: bool,
    args: Map<String, Value>,
    ctx: ToolContext,
) -> Result<Value, ToolError> {
    let args_json = Value::Object(args).to_string();
    let ctx_json = json!({"call_id": ctx.call_id, "run_id": ctx.run_id}).to_string();
    let answer = call_python(
        adapter,
        is_async,
        args_json,
        ctx_json,
        loop_of(ctx.metadata()),
    )
    .await
    .map_err(ToolError::fatal)?;
    match answer {
        Answer::Envelope(envelope) => {
            let envelope: Value = serde_json::from_str(&envelope)
                .map_err(|err| ToolError::fatal(format!("unreadable tool answer: {err}")))?;
            if let Some(message) = envelope.get("tool_error").and_then(Value::as_str) {
                let error = ToolError::new(message);
                return Err(if envelope["retryable"].as_bool() == Some(true) {
                    error.retryable()
                } else {
                    error
                });
            }
            Ok(envelope.get("ok").cloned().unwrap_or(Value::Null))
        }
        Answer::Raised(exception, text) => {
            if keep_cause {
                lock(tool_exceptions()).insert(ctx.call_id.clone(), exception);
            }
            Err(ToolError::crashed(text))
        }
    }
}

// ---------- hooks (design 0017 §5) ----------

/// A Python hook: `adapter(stage, payload_json)` (async) returns an
/// envelope. Only the stages the hook defines are called.
struct PyHook {
    name: String,
    adapter: Arc<Py<PyAny>>,
    stages: Vec<String>,
    /// Keep exceptions of tool stages for `tool_failed` (`on_tool_exception="fail"`).
    keep_tool_causes: bool,
}

/// Where a hook's exception waits for the run error (`hook_failed`): a key
/// unique to this failure, so concurrent runs never take each other's.
fn hook_cause_key(hook: &str, stage: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("hook:{hook}:{stage}:{n}")
}

fn hook_ctx_json(ctx: &HookContext) -> String {
    json!({
        "run_id": ctx.run_id,
        "messages": to_json(&ctx.messages),
        "model_calls": ctx.model_calls,
        "hook_state": ctx.hook_state,
        "state": ctx.state.as_map(),
    })
    .to_string()
}

fn hook_action(envelope: &Value) -> Result<HookAction, HookError> {
    let mut action = HookAction::proceed();
    if let Some(state) = envelope.get("state").filter(|v| !v.is_null()) {
        action = action.with_state(state.clone());
    }
    if let Some(messages) = envelope.get("messages").filter(|v| !v.is_null()) {
        let messages: Vec<Message> = serde_json::from_value(messages.clone())
            .map_err(|err| HookError::new(format!("unreadable messages: {err}")))?;
        action = action.replace_messages(messages);
    }
    if let Some(message) = envelope.get("message").filter(|v| !v.is_null()) {
        let message: Message = serde_json::from_value(message.clone())
            .map_err(|err| HookError::new(format!("unreadable message: {err}")))?;
        action = action.replace_message(message);
    }
    if envelope.get("end") == Some(&Value::Bool(true)) {
        action = action.end(envelope.get("output").cloned().filter(|v| !v.is_null()));
    }
    Ok(action)
}

/// A tool output as a value for Python (`after_tool`).
fn output_value(output: &ToolOutput) -> Value {
    match output {
        ToolOutput::Text(text) => json!(text),
        ToolOutput::Json(value) => value.clone(),
        ToolOutput::Blocks(blocks) => to_json(blocks),
        ToolOutput::Rich { value, .. } => value.clone(),
        _ => Value::Null,
    }
}

/// `value` as the output, keeping the original's artifact and state changes.
fn replaced(original: ToolOutput, value: Value) -> ToolOutput {
    let new = match value {
        Value::String(text) => ToolOutput::from(text),
        other => ToolOutput::from(other),
    };
    match original {
        ToolOutput::Rich {
            artifact, state, ..
        } => {
            let new = match artifact {
                Some(artifact) => new.with_artifact(artifact),
                None => new,
            };
            state
                .into_iter()
                .fold(new, |output, (key, value)| output.with_state(key, value))
        }
        _ => new,
    }
}

impl PyHook {
    fn has(&self, stage: &str) -> bool {
        self.stages.iter().any(|s| s == stage)
    }

    /// Calls the hook's `stage`; an exception is kept under `cause` (if
    /// any) for the run error.
    async fn call(
        &self,
        stage: &str,
        payload: String,
        metadata: Option<&Value>,
        cause: Option<String>,
    ) -> Result<Value, String> {
        let answer = call_python(
            self.adapter.clone(),
            true,
            stage.to_string(),
            payload,
            loop_of(metadata),
        )
        .await?;
        match answer {
            Answer::Envelope(envelope) => serde_json::from_str(&envelope)
                .map_err(|err| format!("unreadable hook answer: {err}")),
            Answer::Raised(exception, text) => {
                if let Some(cause) = cause {
                    lock(tool_exceptions()).insert(cause, exception);
                }
                Err(text)
            }
        }
    }

    async fn action(&self, stage: &str, ctx: &HookContext) -> Result<HookAction, HookError> {
        if !self.has(stage) {
            return Ok(HookAction::proceed());
        }
        let key = hook_cause_key(&self.name, stage);
        let envelope = self
            .call(
                stage,
                hook_ctx_json(ctx),
                ctx.metadata.as_ref(),
                Some(key.clone()),
            )
            .await
            .map_err(|text| HookError::new(text).with_cause_key(key))?;
        hook_action(&envelope)
    }
}

impl Hook for PyHook {
    fn name(&self) -> &str {
        &self.name
    }

    fn on_start<'a>(
        &'a self,
        ctx: &'a HookContext,
    ) -> BoxFuture<'a, Result<HookAction, HookError>> {
        Box::pin(self.action("on_start", ctx))
    }

    fn before_model<'a>(
        &'a self,
        ctx: &'a HookContext,
    ) -> BoxFuture<'a, Result<HookAction, HookError>> {
        Box::pin(self.action("before_model", ctx))
    }

    fn after_model<'a>(
        &'a self,
        ctx: &'a HookContext,
    ) -> BoxFuture<'a, Result<HookAction, HookError>> {
        Box::pin(self.action("after_model", ctx))
    }

    fn wrap_tool<'a>(
        &'a self,
        mut call: ToolCallRequest,
        next: ToolNext<'a>,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            let metadata = next.context().metadata().cloned();
            let cause = self.keep_tool_causes.then(|| call.call_id.clone());
            let call_json = |call: &ToolCallRequest| json!({"call_id": call.call_id, "name": call.name, "args": call.args});
            if self.has("before_tool") {
                let envelope = self
                    .call(
                        "before_tool",
                        call_json(&call).to_string(),
                        metadata.as_ref(),
                        cause.clone(),
                    )
                    .await
                    .map_err(ToolError::crashed)?;
                if let Some(message) = envelope.get("tool_error").and_then(Value::as_str) {
                    return Err(ToolError::new(message));
                }
                if let Some(value) = envelope.get("result") {
                    return Ok(replaced(ToolOutput::from(Value::Null), value.clone()));
                }
                if let Some(changed) = envelope.get("call").filter(|c| c.is_object()) {
                    if let Some(name) = changed["name"].as_str() {
                        call.name = name.to_string();
                    }
                    if let Some(args) = changed["args"].as_object() {
                        call.args = args.clone();
                    }
                }
            }
            let snapshot = call_json(&call);
            let output = next.call(call).await?;
            if !self.has("after_tool") {
                return Ok(output);
            }
            let payload = json!({"call": snapshot, "result": output_value(&output)}).to_string();
            let envelope = self
                .call("after_tool", payload, metadata.as_ref(), cause)
                .await
                .map_err(ToolError::crashed)?;
            Ok(match envelope.get("result") {
                Some(value) => replaced(output, value.clone()),
                None => output,
            })
        })
    }

    fn on_end<'a>(
        &'a self,
        ctx: &'a HookContext,
        outcome: criogen_agent::Outcome,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if !self.has("on_end") {
                return;
            }
            let outcome = match outcome {
                criogen_agent::Outcome::Completed => "completed",
                criogen_agent::Outcome::Suspended => "suspended",
                _ => "failed",
            };
            let mut payload: Value = serde_json::from_str(&hook_ctx_json(ctx)).unwrap_or_default();
            payload["outcome"] = json!(outcome);
            // on_end cannot change the outcome: its errors are dropped.
            let _ = self
                .call("on_end", payload.to_string(), ctx.metadata.as_ref(), None)
                .await;
        })
    }
}

// ---------- the gateway (design 0018) ----------

/// The messages of `input`: text, or messages whose `content` may be a
/// string (one text block).
fn gateway_messages(input: &Value) -> Result<Vec<Message>, Failure> {
    match input {
        Value::String(text) => Ok(vec![Message::user(text.clone())]),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                let mut item = item.clone();
                if let Some(text) = item.get("content").and_then(Value::as_str) {
                    item["content"] = json!([{"type": "text", "text": text}]);
                }
                serde_json::from_value(item).map_err(|err| bad(format!("a message: {err}")))
            })
            .collect(),
        _ => Err(bad("input is text or a list of messages")),
    }
}

/// The request of a `chat` / `stream` payload.
fn gateway_request(payload: &Value) -> Result<ModelRequest, Failure> {
    let mut messages = gateway_messages(&payload["input"])?;
    if let Some(instructions) = text(payload, "instructions") {
        messages.insert(0, Message::system(instructions));
    }
    let mut settings = Settings::default();
    settings.temperature = payload["temperature"].as_f64();
    settings.max_tokens = payload["max_tokens"]
        .as_u64()
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX));
    let mut request = ModelRequest::new(messages).with_settings(settings);
    if let Some(schema) = payload.get("output").filter(|o| !o.is_null()) {
        request.output = Some(OutputSpec::new(schema.clone()));
    }
    Ok(request)
}

fn model_failure(error: ModelError) -> Failure {
    Failure {
        code: "model_failed".into(),
        message: error.to_string(),
        details: Some(json!({"model_error": error})),
    }
}

/// The reply, with the parsed and checked output when one was asked for.
fn chat_reply(response: ModelResponse, schema: Option<&Value>) -> Outcome {
    let text = response.message.text();
    let output = match schema {
        None => Value::Null,
        Some(schema) => {
            let value: Value = serde_json::from_str(text.trim()).map_err(|err| {
                Failure::new("output_invalid", format!("the reply is not JSON: {err}"))
            })?;
            let problems = validate_arguments(schema, &value);
            if !problems.is_empty() {
                return Err(Failure {
                    code: "output_invalid".into(),
                    message: format!(
                        "the reply does not match the schema: {}",
                        problems.join("; ")
                    ),
                    details: Some(json!({"problems": problems})),
                });
            }
            value
        }
    };
    let meta = &response.message.metadata;
    let model = format!(
        "{}:{}",
        meta.provider.as_deref().unwrap_or_default(),
        meta.model.as_deref().unwrap_or_default()
    );
    Ok(json!({
        "text": text,
        "message": to_json(&response.message),
        "usage": to_json(&response.usage),
        "stop_reason": to_json(&response.finish_reason),
        "model": model,
        "output": output,
    }))
}

/// `work`, or `cancelled` once `cancel` fires (the HTTP request is dropped).
async fn or_cancelled(work: impl Future<Output = Outcome>, cancel: CancelToken) -> Outcome {
    let mut work = std::pin::pin!(work);
    let mut cancelled = std::pin::pin!(cancel.cancelled());
    std::future::poll_fn(|cx| {
        if let std::task::Poll::Ready(outcome) = work.as_mut().poll(cx) {
            return std::task::Poll::Ready(outcome);
        }
        if cancelled.as_mut().poll(cx).is_ready() {
            return std::task::Poll::Ready(Err(Failure::new(
                "cancelled",
                "the call was cancelled",
            )));
        }
        std::task::Poll::Pending
    })
    .await
}

/// Model calls without an agent: the agent's retries and fallbacks
/// (`ResilientModel`) around the model each call names.
#[pyclass(frozen)]
struct NativeGateway {
    fallbacks: Vec<Arc<dyn Model>>,
    fallback_on: Vec<ModelErrorKind>,
    retry: RetryPolicy,
    max_retry_after: Duration,
    /// Models by their spec: each provider client is made once.
    models: Mutex<HashMap<String, Arc<dyn Model>>>,
}

impl NativeGateway {
    fn chain(&self, spec: &Value) -> Result<ResilientModel, Failure> {
        let key = spec.to_string();
        // The guard must end before the insert below.
        let cached = lock(&self.models).get(&key).cloned();
        let primary = match cached {
            Some(model) => model,
            None => {
                // A fake model's script goes on from call to call, as an agent's does.
                let made = model(spec)?;
                lock(&self.models).insert(key, made.clone());
                made
            }
        };
        let mut chain = ResilientModel::new(primary)
            .retry(self.retry.clone())
            .max_retry_after(self.max_retry_after);
        for fallback in &self.fallbacks {
            chain = chain.fallback(fallback.clone());
        }
        for kind in &self.fallback_on {
            chain = chain.fallback_on(*kind);
        }
        Ok(chain)
    }
}

#[pymethods]
impl NativeGateway {
    #[new]
    fn new(config: &str) -> PyResult<Self> {
        let config: Value =
            serde_json::from_str(config).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let fallbacks = config["fallbacks"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(model)
            .collect::<Result<Vec<_>, _>>()
            .map_err(py_failure)?;
        let fallback_on = match config.get("fallback_on").filter(|k| k.is_array()) {
            Some(kinds) => serde_json::from_value(kinds.clone())
                .map_err(|err| py_failure(bad(format!("fallback_on: {err}"))))?,
            None => Vec::new(),
        };
        Ok(Self {
            fallbacks,
            fallback_on,
            retry: config
                .get("retry")
                .filter(|s| s.is_object())
                .map_or_else(RetryPolicy::new, retry_policy),
            max_retry_after: Duration::from_secs_f64(
                config["max_retry_after_secs"].as_f64().unwrap_or(60.0),
            ),
            models: Mutex::default(),
        })
    }

    /// One model call: payload `{model, input, instructions, temperature,
    /// max_tokens, output}`.
    fn chat<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
        payload: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let payload: Value =
            serde_json::from_str(payload).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let chain = self.chain(&payload["model"]).map_err(py_failure)?;
        let request = gateway_request(&payload).map_err(py_failure)?;
        let cancel = CancelToken::new();
        let token = cancel.clone();
        spawn(py, event_loop, Some(cancel), async move {
            let work = async {
                let response = chain.invoke(&request).await.map_err(model_failure)?;
                chat_reply(response, request.output.as_ref().map(|o| &o.schema))
            };
            or_cancelled(work, token).await
        })
    }

    /// The same call, streamed: text chunks, then the reply.
    fn stream(&self, payload: &str) -> PyResult<NativeChatStream> {
        let payload: Value =
            serde_json::from_str(payload).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let chain = self.chain(&payload["model"]).map_err(py_failure)?;
        let request = gateway_request(&payload).map_err(py_failure)?;
        let (chunks, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (reply, reply_receiver) = tokio::sync::oneshot::channel();
        let cancel = CancelToken::new();
        let token = cancel.clone();
        runtime().spawn(async move {
            let work = async {
                // A new message id means a retry started over (design 0018 §2).
                let mut current: Option<String> = None;
                let mut sink = |delta: MessageDelta| {
                    if current.as_deref() != Some(delta.message_id.as_str()) {
                        if current.is_some() {
                            let _ = chunks.send(json!({"type": "restart", "text": ""}));
                        }
                        current = Some(delta.message_id.clone());
                    }
                    if let BlockDelta::Text { text } = delta.delta {
                        let _ = chunks.send(json!({"type": "text", "text": text}));
                    }
                };
                let response = chain
                    .stream(&request, &mut sink)
                    .await
                    .map_err(model_failure)?;
                chat_reply(response, request.output.as_ref().map(|o| &o.schema))
            };
            let outcome = or_cancelled(work, token).await;
            let _ = reply.send(outcome);
        });
        Ok(NativeChatStream {
            chunks: Arc::new(tokio::sync::Mutex::new(receiver)),
            reply: Arc::new(tokio::sync::Mutex::new(Some(reply_receiver))),
            cancel,
        })
    }

    /// Embeddings: payload `{embedder, texts}`.
    fn embed<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
        payload: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let payload: Value =
            serde_json::from_str(payload).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let embedder = embedder(&payload["embedder"]).map_err(py_failure)?;
        let texts: Vec<String> = serde_json::from_value(payload["texts"].clone())
            .map_err(|err| py_failure(bad(format!("texts: {err}"))))?;
        let cancel = CancelToken::new();
        let token = cancel.clone();
        spawn(py, event_loop, Some(cancel), async move {
            let work = async {
                let vectors = embedder.embed(&texts).await.map_err(model_failure)?;
                Ok(json!(vectors))
            };
            or_cancelled(work, token).await
        })
    }
}

/// A streamed gateway call.
#[pyclass(frozen)]
struct NativeChatStream {
    chunks: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Value>>>,
    reply: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<Outcome>>>>,
    cancel: CancelToken,
}

#[pymethods]
impl NativeChatStream {
    /// The next chunk as JSON, or `null` after the last.
    fn next<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let chunks = self.chunks.clone();
        spawn(py, event_loop, None, async move {
            Ok(chunks.lock().await.recv().await.unwrap_or(Value::Null))
        })
    }

    /// Waits for the call; its reply.
    fn reply<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let reply = self.reply.clone();
        spawn(py, event_loop, None, async move {
            let taken = reply.lock().await.take();
            let Some(receiver) = taken else {
                return Err(bad("the reply was already taken"));
            };
            receiver
                .await
                .unwrap_or_else(|_| Err(Failure::new("cancelled", "the call was cancelled")))
        })
    }

    /// Cancels the call.
    fn cancel(&self) {
        self.cancel.cancel();
    }
}

// ---------- the agent ----------

/// A telemetry exporter (design 0017 §4): OTLP over HTTP, or Langfuse.
/// Agents built with it send every run to it.
#[pyclass(frozen)]
struct NativeTelemetry {
    exporter: Arc<OtlpExporter>,
}

#[pymethods]
impl NativeTelemetry {
    /// `{"kind": "langfuse", public_key?, secret_key?, base_url?}` (missing
    /// keys from the LANGFUSE_* variables) or `{"kind": "otlp", endpoint,
    /// headers}`; both take `service_name` and `capture_content`.
    #[new]
    fn new(spec: &str) -> PyResult<Self> {
        let spec: Value =
            serde_json::from_str(spec).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let mut config = match text(&spec, "kind") {
            Some("langfuse") => match (text(&spec, "public_key"), text(&spec, "secret_key")) {
                (Some(public), Some(secret)) => Langfuse::config(
                    public,
                    secret,
                    text(&spec, "base_url").unwrap_or(criogen_telemetry::DEFAULT_BASE_URL),
                ),
                _ => Langfuse::from_env().map_err(|err| py_failure(bad(err.to_string())))?,
            },
            Some("otlp") => {
                let mut config = OtlpConfig::new(
                    text(&spec, "endpoint")
                        .ok_or_else(|| py_failure(bad("otlp needs an endpoint")))?,
                );
                for (name, value) in spec["headers"].as_object().cloned().unwrap_or_default() {
                    config = config.header(name, value.as_str().unwrap_or_default());
                }
                config
            }
            _ => return Err(py_failure(bad("telemetry is \"langfuse\" or \"otlp\""))),
        };
        if let Some(name) = text(&spec, "service_name") {
            config = config.service_name(name);
        }
        if let Some(capture) = spec["capture_content"].as_bool() {
            config = config.capture_content(capture);
        }
        let _runtime = runtime().enter();
        let exporter =
            OtlpExporter::start(config).map_err(|err| py_failure(bad(err.to_string())))?;
        Ok(Self {
            exporter: Arc::new(exporter),
        })
    }

    /// Waits until everything sent so far is exported.
    fn flush<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let exporter = self.exporter.clone();
        spawn(py, event_loop, None, async move {
            exporter.flush().await;
            Ok(Value::Null)
        })
    }

    /// Exports what is left and stops.
    fn shutdown<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let exporter = self.exporter.clone();
        spawn(py, event_loop, None, async move {
            exporter.shutdown().await;
            Ok(Value::Null)
        })
    }

    /// `{"exported", "failed", "dropped", "last_error"}`.
    fn stats(&self) -> String {
        let stats = self.exporter.stats();
        json!({
            "exported": stats.exported,
            "failed": stats.failed,
            "dropped": stats.dropped_events,
            "last_error": self.exporter.last_error(),
        })
        .to_string()
    }
}

/// Storage built from a Python configuration; agents built with it share
/// it (threads, memory, leases, audit trail).
#[pyclass(frozen)]
struct NativeStorage {
    storage: Storage,
}

#[pymethods]
impl NativeStorage {
    /// Connects (blocking briefly, without the GIL).
    #[new]
    fn new(py: Python<'_>, spec: &str) -> PyResult<Self> {
        let spec: Value =
            serde_json::from_str(spec).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let storage = py
            .detach(|| runtime().block_on(storage(&spec)))
            .map_err(py_failure)?;
        Ok(Self { storage })
    }

    /// Closes the database's connections.
    fn close<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let storage = self.storage.clone();
        spawn(py, event_loop, None, async move {
            storage.close().await;
            Ok(Value::Null)
        })
    }

    /// A storage operation (memory, prune, audit).
    fn op<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
        op: String,
        payload: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let payload: Value =
            serde_json::from_str(payload).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let storage = self.storage.clone();
        spawn(
            py,
            event_loop,
            None,
            storage_operation(storage, op, payload),
        )
    }
}

/// An agent built from a Python configuration.
#[pyclass(frozen)]
struct NativeAgent {
    agent: Agent,
}

/// Options for one call: the caller's loop in the metadata, and a cancel
/// token Python cancellation fires.
fn call_options(agent: &Agent, event_loop: &LoopId, cancel: &CancelToken) -> RunOptions {
    agent
        .run_options()
        .metadata(json!({ LOOP_KEY: event_loop.0 }))
        .cancel_token(cancel.clone())
}

fn input(payload: &Value) -> AgentInput {
    let mut input = AgentInput::from(text(payload, "input").unwrap_or_default());
    if let Some(state) = payload.get("state").and_then(Value::as_object) {
        for (key, value) in state {
            input = input.with(key.clone(), value.clone());
        }
    }
    input
}

fn thread(agent: &Agent, payload: &Value, options: RunOptions) -> Thread {
    let mut thread = agent
        .thread(text(payload, "thread_id").unwrap_or_default())
        .with_options(options);
    if let Some(actor) = text(payload, "actor") {
        thread = thread.actor(actor);
    }
    thread
}

fn decisions(payload: &Value) -> Result<Vec<(String, Decision)>, Failure> {
    payload["decisions"]
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|(call_id, decision)| {
            serde_json::from_value(decision)
                .map(|d| (call_id, d))
                .map_err(|err| {
                    Failure::new("resume_invalid", format!("unreadable decision: {err}"))
                })
        })
        .collect()
}

async fn thread_operation(thread: Thread, op: String, payload: Value) -> Outcome {
    let reply = match op.as_str() {
        "send" => thread.send(input(&payload)).await?,
        "approve" => thread.approve().await?,
        "reject" => {
            thread
                .reject(text(&payload, "message").unwrap_or("rejected"))
                .await?
        }
        "decide" => thread.decide(decisions(&payload)?).await?,
        "retry" => thread.retry().await?,
        "history" => {
            let limit = payload["limit"].as_u64().unwrap_or(20) as usize;
            return Ok(to_json(&thread.history(limit).await?));
        }
        "messages" => return Ok(to_json(&thread.messages().await?)),
        "rewind" => {
            let id = text(&payload, "checkpoint_id").unwrap_or_default();
            return Ok(to_json(&thread.rewind(id).await?));
        }
        "delete" => return Ok(json!(thread.delete().await?)),
        other => return Err(bad(format!("unknown thread operation `{other}`"))),
    };
    Ok(reply_json(&reply))
}

async fn storage_operation(storage: Storage, op: String, payload: Value) -> Outcome {
    let namespace = |key: &str| {
        Namespace::parse(text(&payload, key).unwrap_or_default()).map_err(Failure::from)
    };
    match op.as_str() {
        "memory_put" => {
            let ttl = payload["ttl_secs"].as_f64().map(Duration::from_secs_f64);
            storage
                .memory()
                .put(
                    &namespace("namespace")?,
                    text(&payload, "key").unwrap_or_default(),
                    payload["value"].clone(),
                    ttl,
                )
                .await?;
            Ok(Value::Null)
        }
        "memory_get" => Ok(to_json(
            &storage
                .memory()
                .get(
                    &namespace("namespace")?,
                    text(&payload, "key").unwrap_or_default(),
                )
                .await?,
        )),
        "memory_search" => {
            let mut query =
                SearchQuery::new().limit(payload["limit"].as_u64().unwrap_or(10) as usize);
            if let Some(words) = text(&payload, "text") {
                query = query.text(words);
            }
            Ok(to_json(
                &storage
                    .memory()
                    .search(&namespace("namespace")?, &query)
                    .await?,
            ))
        }
        "memory_delete_namespace" => Ok(json!(
            storage
                .memory()
                .delete_namespace(&namespace("namespace")?)
                .await?
        )),
        "prune" => Ok(to_json(&prune_json(storage.prune().await?))),
        "audit_query" => {
            let log = storage
                .audit()
                .ok_or_else(|| bad("the audit trail is off: Storage(audit=True)"))?;
            let mut query =
                AuditQuery::new().limit(payload["limit"].as_u64().unwrap_or(100) as usize);
            if let Some(thread) = text(&payload, "thread_id") {
                query = query.thread(thread);
            }
            if let Some(action) = text(&payload, "action") {
                query = query.action(action);
            }
            if let Some(after) = payload["after_seq"].as_u64() {
                query = query.after_seq(after);
            }
            Ok(to_json(&log.query(&query).await?))
        }
        "audit_verify" => {
            let log = storage
                .audit()
                .ok_or_else(|| bad("the audit trail is off: Storage(audit=True)"))?;
            Ok(to_json(&log.verify().await?))
        }
        other => Err(bad(format!("unknown storage operation `{other}`"))),
    }
}

fn prune_json(report: criogen_storage::PruneReport) -> Value {
    json!({
        "threads_deleted": report.threads_deleted,
        "threads_busy": report.threads_busy,
        "checkpoints_deleted": report.checkpoints_deleted,
        "memory_items_deleted": report.memory_items_deleted,
    })
}

fn py_failure(failure: Failure) -> PyErr {
    let details = failure.details.map(|d| d.to_string());
    Python::attach(|py| {
        match py
            .import("criogen._bridge")
            .and_then(|b| b.getattr("make_error"))
            .and_then(|f| f.call1((failure.code.clone(), failure.message.clone(), details)))
        {
            Ok(exception) => PyErr::from_value(exception),
            Err(_) => PyRuntimeError::new_err(format!("{}: {}", failure.code, failure.message)),
        }
    })
}

#[pymethods]
impl NativeAgent {
    /// Builds the agent. `tools` holds `(adapter, is_async)` per tool in
    /// the configuration's order; without `storage` it keeps threads in
    /// memory.
    #[new]
    #[pyo3(signature = (config, tools, storage=None, agent_tools=Vec::new(), telemetry=None, hooks=Vec::new(), mcp=Vec::new(), collections=Vec::new()))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        config: &str,
        tools: Vec<(Py<PyAny>, bool)>,
        storage: Option<&NativeStorage>,
        agent_tools: Vec<(String, String, Py<NativeAgent>)>,
        telemetry: Option<&NativeTelemetry>,
        hooks: Vec<(String, Py<PyAny>, Vec<String>)>,
        mcp: Vec<(Py<NativeMcp>, Option<Vec<String>>)>,
        collections: Vec<(Py<NativeCollection>, String, String)>,
    ) -> PyResult<Self> {
        let config: Value =
            serde_json::from_str(config).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let storage = storage.map_or_else(Storage::in_memory, |s| s.storage.clone());
        let mut builder = Agent::builder_shared(model(&config["model"]).map_err(py_failure)?)
            .storage(storage.clone());
        if let Some(instructions) = text(&config, "instructions") {
            builder = builder.instructions(instructions);
        }
        let specs = config["tools"].as_array().cloned().unwrap_or_default();
        // Only a run that fails on a tool's exception reports it as a cause.
        let keep_cause = config["limits"]["on_tool_exception"].as_str() == Some("fail");
        if specs.len() != tools.len() {
            return Err(PyValueError::new_err("one adapter per tool"));
        }
        for (spec, (adapter, is_async)) in specs.iter().zip(tools) {
            let mut annotations = ToolAnnotations::default();
            annotations.read_only = spec["read_only"].as_bool() == Some(true);
            annotations.destructive = spec["destructive"].as_bool() == Some(true);
            annotations.idempotent = spec["idempotent"].as_bool() == Some(true);
            let tool_spec = ToolSpec::new(
                text(spec, "name").unwrap_or_default(),
                text(spec, "description").unwrap_or_default(),
                spec["schema"].clone(),
            )
            .with_annotations(annotations);
            let adapter = Arc::new(adapter);
            builder = builder.tool(tool_fn(tool_spec, move |args, ctx: ToolContext| {
                call_tool(adapter.clone(), is_async, keep_cause, args, ctx)
            }));
        }
        // Hooks, in order (design 0017 §5).
        let keep_tool_causes = config["limits"]["on_tool_exception"].as_str() == Some("fail");
        for (name, adapter, stages) in hooks {
            builder = builder.hook(PyHook {
                name,
                adapter: Arc::new(adapter),
                stages,
                keep_tool_causes,
            });
        }
        // Every run of this agent goes to the exporter (design 0017 §4).
        if let Some(telemetry) = telemetry {
            builder = builder.observe(telemetry.exporter.observer());
        }
        // MCP servers' tools (design 0022): all of a server's, or the named.
        for (server, names) in mcp {
            let client = server.get().client().map_err(py_failure)?;
            for tool in client.tools() {
                if names.as_ref().is_none_or(|n| n.contains(&tool.spec().name)) {
                    builder = builder.shared_tool(tool);
                }
            }
        }
        // Collections as search tools (design 0025).
        for (collection, name, description) in collections {
            let collection = collection.get().get().map_err(py_failure)?;
            builder = builder.tool(collection.as_tool(name, description));
        }
        // Agents as tools of this one (design 0017 §1).
        for (name, description, agent) in agent_tools {
            builder = builder.tool(agent.get().agent.as_tool(name, description));
        }
        // Retries and fallback models (design 0017 §2).
        for spec in config["fallbacks"].as_array().cloned().unwrap_or_default() {
            builder = builder.fallback_shared(model(&spec).map_err(py_failure)?);
        }
        if let Some(kinds) = config.get("fallback_on").filter(|k| k.is_array()) {
            let kinds: Vec<ModelErrorKind> = serde_json::from_value(kinds.clone())
                .map_err(|err| py_failure(bad(format!("fallback_on: {err}"))))?;
            builder = builder.fallback_on(kinds);
        }
        if let Some(spec) = config.get("model_retry").filter(|s| s.is_object()) {
            builder = builder.model_retry(retry_policy(spec));
        }
        if let Some(spec) = config.get("tool_retry").filter(|s| s.is_object()) {
            builder = builder.tool_retry(retry_policy(spec));
        }
        if let Some(secs) = config["max_retry_after_secs"].as_f64() {
            builder = builder.max_retry_after(Duration::from_secs_f64(secs));
        }
        if let Some(names) = config["approve"].as_array().filter(|n| !n.is_empty()) {
            let mut approval = Approval::new();
            for name in names.iter().filter_map(Value::as_str) {
                approval = approval.tool(name);
            }
            builder = builder.approval(approval);
        }
        if let Some(schema) = config.get("output").filter(|s| s.is_object()) {
            builder = builder.output(schema.clone());
        }
        if let Some(spec) = config.get("limits").filter(|s| s.is_object()) {
            builder = builder.limits(limits(spec).map_err(py_failure)?);
        }
        if let Some(spec) = config.get("memory").filter(|s| s.is_object()) {
            builder = builder.memory(memory(spec).map_err(py_failure)?);
        }
        let agent = builder
            .build()
            .map_err(|err| py_failure(Failure::new("invalid_agent", err.to_string())))?;
        Ok(Self { agent })
    }

    /// `agent.run`: payload `{"input", "state"}`.
    fn run<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
        payload: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let payload: Value =
            serde_json::from_str(payload).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let loop_id = LoopId::register(event_loop);
        let cancel = CancelToken::new();
        let options = call_options(&self.agent, &loop_id, &cancel);
        let agent = self.agent.clone();
        spawn(py, event_loop, Some(cancel), async move {
            let _loop_id = loop_id;
            let result = agent.run_with(input(&payload), options).await?;
            Ok(result_json(&result))
        })
    }

    /// A thread operation: payload `{"thread_id", "actor", …}`.
    fn thread<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
        op: String,
        payload: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let payload: Value =
            serde_json::from_str(payload).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let loop_id = LoopId::register(event_loop);
        let cancel = CancelToken::new();
        let thread = thread(
            &self.agent,
            &payload,
            call_options(&self.agent, &loop_id, &cancel),
        );
        spawn(py, event_loop, Some(cancel), async move {
            let _loop_id = loop_id;
            thread_operation(thread, op, payload).await
        })
    }

    /// Streams a thread operation (`send`, `approve`, `reject`, `decide`,
    /// `retry`).
    fn stream(
        &self,
        event_loop: &Bound<'_, PyAny>,
        op: &str,
        payload: &str,
    ) -> PyResult<NativeStream> {
        let payload: Value =
            serde_json::from_str(payload).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let loop_id = LoopId::register(event_loop);
        let cancel = CancelToken::new();
        let thread = thread(
            &self.agent,
            &payload,
            call_options(&self.agent, &loop_id, &cancel),
        );
        let _runtime = runtime().enter();
        let stream = match op {
            "send" => thread.stream(input(&payload)),
            "approve" => thread.stream_approve(),
            "reject" => thread.stream_reject(text(&payload, "message").unwrap_or("rejected")),
            "decide" => thread.stream_decide(decisions(&payload).map_err(py_failure)?),
            "retry" => thread.stream_retry(),
            other => return Err(PyValueError::new_err(format!("cannot stream `{other}`"))),
        };
        Ok(NativeStream {
            cancel: stream.cancel_token(),
            stream: Arc::new(tokio::sync::Mutex::new(Some(stream))),
            _loop_id: Arc::new(loop_id),
        })
    }
}

// ---------- workflows (design 0015) ----------

/// A run error for Python: the node's exception is found by `cause_key`.
fn run_failure(error: RunError) -> Failure {
    let code = error.kind.code().to_string();
    let mut details = json!({"step": error.step});
    if let Some(snapshot) = error.snapshot() {
        details["snapshot"] = to_json(snapshot);
        details["run_id"] = json!(snapshot.run_id);
    }
    match &error.kind {
        RunErrorKind::NodeFailed { node, source } | RunErrorKind::RouterFailed { node, source } => {
            details["node"] = json!(node);
            details["cause_key"] = source
                .details
                .as_ref()
                .and_then(|d| d.get("cause_key"))
                .cloned()
                .unwrap_or(Value::Null);
            if let Some(kind) = &source.kind {
                details["kind"] = json!(kind);
            }
        }
        RunErrorKind::NodeTimeout { node, .. } | RunErrorKind::NodePanicked { node, .. } => {
            details["node"] = json!(node);
        }
        _ => {}
    }
    Failure {
        code,
        message: error.to_string(),
        details: Some(details),
    }
}

/// Where a node's or router's exception waits for its error: a key unique
/// to this failure, carried by the node's error, so concurrent runs that
/// fail the same way never take each other's exception.
fn cause_key(node: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("node:{node}:{n}")
}

fn thread_failure(error: ThreadError) -> Failure {
    match error {
        ThreadError::Run(run) => run_failure(*run),
        other => Failure::new(other.code(), other.to_string()),
    }
}

/// A Python function as a node: `adapter(state_json, ctx_json)` returns
/// an envelope with the update, and a suspend payload or goto targets.
fn python_node(name: String, adapter: Arc<Py<PyAny>>, is_async: bool) -> impl Node {
    node_with_context(move |ctx: NodeContext, state: Arc<State>| {
        let (name, adapter) = (name.clone(), adapter.clone());
        async move {
            let ctx_json = json!({
                "run_id": ctx.run_id(), "step": ctx.step(), "node": ctx.node(),
                "attempt": ctx.attempt(), "resume": ctx.resume(),
            })
            .to_string();
            let state_json = serde_json::to_string(state.as_map()).unwrap_or_default();
            let answer = call_python(
                adapter,
                is_async,
                state_json,
                ctx_json,
                loop_of(ctx.metadata()),
            )
            .await
            .map_err(NodeError::new)?;
            let envelope = match answer {
                Answer::Envelope(envelope) => envelope,
                Answer::Raised(exception, text) => {
                    let key = cause_key(&name);
                    lock(tool_exceptions()).insert(key.clone(), exception);
                    return Err(NodeError::new(text).with_details(json!({"cause_key": key})));
                }
            };
            let envelope: Value = serde_json::from_str(&envelope)
                .map_err(|err| NodeError::new(format!("unreadable node answer: {err}")))?;
            let update = match envelope.get("update") {
                Some(Value::Object(map)) => Update::from(map.clone()),
                Some(Value::Null) | None => Update::new(),
                Some(other) => {
                    return Err(NodeError::new(format!(
                        "a node returns a dict (the update), not {other}"
                    )));
                }
            };
            if let Some(payload) = envelope.get("suspend") {
                return Ok(NodeOutput::suspend(update, payload.clone()));
            }
            if let Some(targets) = envelope.get("goto").and_then(Value::as_array) {
                let targets: Vec<String> = targets
                    .iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect();
                return Ok(NodeOutput::goto(update, targets));
            }
            Ok(NodeOutput::from(update))
        }
    })
}

/// A sync Python function as a router: `adapter(state_json, "")` returns
/// `{"label"}`. (A router has no run context, so it cannot reach the
/// caller's event loop: routers are sync functions.)
fn python_router(from: String, adapter: Arc<Py<PyAny>>) -> impl Router {
    router_fn(move |state: Arc<State>| {
        let (from, adapter) = (from.clone(), adapter.clone());
        async move {
            let state_json = serde_json::to_string(state.as_map()).unwrap_or_default();
            let answer = call_python(adapter, false, state_json, String::new(), None)
                .await
                .map_err(NodeError::new)?;
            match answer {
                Answer::Envelope(envelope) => {
                    let envelope: Value = serde_json::from_str(&envelope).map_err(|err| {
                        NodeError::new(format!("unreadable router answer: {err}"))
                    })?;
                    envelope
                        .get("label")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .ok_or_else(|| NodeError::new("a router returns a label (a string)"))
                }
                Answer::Raised(exception, text) => {
                    let key = cause_key(&from);
                    lock(tool_exceptions()).insert(key.clone(), exception);
                    Err(NodeError::new(text).with_details(json!({"cause_key": key})))
                }
            }
        }
    })
}

fn reducer(name: &str) -> Result<Reducer, Failure> {
    match name {
        "replace" => Ok(Reducer::Replace),
        "append" => Ok(Reducer::Append),
        "upsert_by_id" => Ok(Reducer::UpsertById),
        other => Err(bad(format!("unknown reducer `{other}`"))),
    }
}

fn run_json(run: &RunOutput) -> Value {
    json!({
        "state": run.state,
        "steps": run.steps.len(),
        "suspended": run.is_suspended(),
        "interrupts": to_json(&run.interrupts()),
        "snapshot": run.snapshot().map(to_json),
        "run_id": run.run_id,
    })
}

fn workflow_options(payload: &Value, loop_id: &LoopId, cancel: &CancelToken) -> RunOptions {
    let mut options = RunOptions::default()
        .metadata(json!({ LOOP_KEY: loop_id.0 }))
        .cancel_token(cancel.clone());
    if let Some(n) = payload["max_steps"].as_u64() {
        options = options.max_steps(n as usize);
    }
    if let Some(ms) = payload["deadline_ms"].as_u64() {
        options = options.deadline(Duration::from_millis(ms));
    }
    options
}

/// A compiled workflow (design 0015).
#[pyclass(frozen)]
struct NativeWorkflow {
    workflow: Workflow,
    storage: Storage,
}

#[pymethods]
impl NativeWorkflow {
    /// Builds the graph. `nodes`: `(name, adapter, is_async)`; `routers`:
    /// `(from, adapter)` in the order of `config.routes`; `agents`:
    /// `(name, agent)`.
    #[new]
    #[pyo3(signature = (config, nodes, routers, agents, storage=None))]
    fn new(
        config: &str,
        nodes: Vec<(String, Py<PyAny>, bool)>,
        routers: Vec<(String, Py<PyAny>)>,
        agents: Vec<(String, Py<NativeAgent>)>,
        storage: Option<&NativeStorage>,
    ) -> PyResult<Self> {
        let config: Value =
            serde_json::from_str(config).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let mut builder = WorkflowBuilder::new();
        for (name, adapter, is_async) in nodes {
            builder.add_node(name.clone(), python_node(name, Arc::new(adapter), is_async));
        }
        for (name, agent) in agents {
            builder.add_node(name, agent.get().agent.as_node());
        }
        if let Some(entry) = text(&config, "entry") {
            builder.set_entry(entry);
        }
        for edge in config["edges"].as_array().cloned().unwrap_or_default() {
            builder.add_edge(
                edge[0].as_str().unwrap_or_default(),
                edge[1].as_str().unwrap_or_default(),
            );
        }
        let routes = config["routes"].as_array().cloned().unwrap_or_default();
        if routes.len() != routers.len() {
            return Err(PyValueError::new_err("one function per router"));
        }
        for (route, (from, adapter)) in routes.iter().zip(routers) {
            let table: Vec<(String, String)> = route["routes"]
                .as_object()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|(label, target)| (label, target.as_str().unwrap_or_default().to_string()))
                .collect();
            builder.add_router(from.clone(), python_router(from, Arc::new(adapter)), table);
        }
        for join in config["joins"].as_array().cloned().unwrap_or_default() {
            let sources: Vec<String> = join[0]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect();
            builder.add_join(sources, join[1].as_str().unwrap_or_default());
        }
        for goto in config["gotos"].as_array().cloned().unwrap_or_default() {
            let targets: Vec<String> = goto[1]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect();
            builder.may_goto(goto[0].as_str().unwrap_or_default(), targets);
        }
        for (key, name) in config["reducers"].as_object().cloned().unwrap_or_default() {
            let reducer = reducer(name.as_str().unwrap_or_default()).map_err(py_failure)?;
            builder.reducer(key, reducer);
        }
        let workflow = builder.build().map_err(|err| {
            py_failure(Failure {
                code: "invalid_workflow".into(),
                message: err.to_string(),
                details: Some(json!({
                    "issues": err.issues.iter().map(ToString::to_string).collect::<Vec<_>>(),
                })),
            })
        })?;
        let storage = storage.map_or_else(Storage::in_memory, |s| s.storage.clone());
        Ok(Self { workflow, storage })
    }

    /// Runs once: payload `{"input", "max_steps", "deadline_ms"}`.
    fn run<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
        payload: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let payload: Value =
            serde_json::from_str(payload).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let loop_id = LoopId::register(event_loop);
        let cancel = CancelToken::new();
        let options = workflow_options(&payload, &loop_id, &cancel);
        let workflow = self.workflow.clone();
        spawn(py, event_loop, Some(cancel), async move {
            let _loop_id = loop_id;
            let input = State::try_from(payload["input"].clone())
                .map_err(|_| bad("the input must be a dict"))?;
            let run = workflow
                .run_with(input, options)
                .await
                .map_err(run_failure)?;
            Ok(run_json(&run))
        })
    }

    /// Continues a snapshot (no storage): payload `{"snapshot", "values"}`.
    fn resume<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
        payload: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let payload: Value =
            serde_json::from_str(payload).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let loop_id = LoopId::register(event_loop);
        let cancel = CancelToken::new();
        let options = workflow_options(&payload, &loop_id, &cancel);
        let workflow = self.workflow.clone();
        spawn(py, event_loop, Some(cancel), async move {
            let _loop_id = loop_id;
            let snapshot: Snapshot =
                serde_json::from_value(payload["snapshot"].clone()).map_err(|err| {
                    Failure::new("resume_invalid", format!("unreadable snapshot: {err}"))
                })?;
            let values = payload["values"].as_object().cloned().unwrap_or_default();
            let run = workflow
                .resume_with(&snapshot, values, options)
                .await
                .map_err(run_failure)?;
            Ok(run_json(&run))
        })
    }

    /// A thread operation: payload `{"thread_id", "actor", …}`.
    fn thread<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
        op: String,
        payload: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let payload: Value =
            serde_json::from_str(payload).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let loop_id = LoopId::register(event_loop);
        let cancel = CancelToken::new();
        let mut thread = self
            .storage
            .workflow_thread(
                &self.workflow,
                text(&payload, "thread_id").unwrap_or_default(),
            )
            .with_options(workflow_options(&payload, &loop_id, &cancel));
        if let Some(actor) = text(&payload, "actor") {
            thread = thread.actor(actor);
        }
        spawn(py, event_loop, Some(cancel), async move {
            let _loop_id = loop_id;
            workflow_thread_operation(thread, &op, payload).await
        })
    }
}

fn turn_json(turn: &Turn) -> Value {
    let mut value = run_json(&turn.run);
    value["version"] = json!(turn.version);
    value["checkpoint_id"] = json!(turn.checkpoint_id);
    value
}

async fn workflow_thread_operation(thread: WorkflowThread, op: &str, payload: Value) -> Outcome {
    let turn = match op {
        "run" => {
            let input = State::try_from(payload["input"].clone())
                .map_err(|_| bad("the input must be a dict"))?;
            thread.run(input).await
        }
        "resume" => {
            let values = payload["values"].as_object().cloned().unwrap_or_default();
            thread.resume(values).await
        }
        "retry" => thread.retry().await,
        "state" => return Ok(to_json(&thread.state().await.map_err(thread_failure)?)),
        "interrupts" => return Ok(to_json(&thread.interrupts().await.map_err(thread_failure)?)),
        "history" => {
            let limit = payload["limit"].as_u64().unwrap_or(20) as usize;
            return Ok(to_json(
                &thread.history(limit).await.map_err(thread_failure)?,
            ));
        }
        "rewind" => {
            let id = text(&payload, "checkpoint_id").unwrap_or_default();
            return Ok(to_json(&thread.rewind(id).await.map_err(thread_failure)?));
        }
        "delete" => return Ok(json!(thread.delete().await.map_err(thread_failure)?)),
        other => return Err(bad(format!("unknown workflow thread operation `{other}`"))),
    };
    turn.map(|t| turn_json(&t)).map_err(thread_failure)
}

/// A streamed thread operation.
#[pyclass(frozen)]
struct NativeStream {
    stream: Arc<tokio::sync::Mutex<Option<ThreadStream>>>,
    /// Cancels the run even while `next` holds the stream.
    cancel: CancelToken,
    /// Keeps the caller's loop known while the run may call tools.
    _loop_id: Arc<LoopId>,
}

#[pymethods]
impl NativeStream {
    /// The next event as JSON, or `null` after the last.
    fn next<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.stream.clone();
        spawn(py, event_loop, None, async move {
            let mut guard = stream.lock().await;
            let Some(stream) = guard.as_mut() else {
                return Ok(Value::Null);
            };
            Ok(stream.next().await.map_or(Value::Null, event_json))
        })
    }

    /// Waits for the operation; its reply.
    fn finish<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.stream.clone();
        let loop_id = self._loop_id.clone();
        spawn(py, event_loop, None, async move {
            let _loop_id = loop_id;
            let taken = stream.lock().await.take();
            let Some(stream) = taken else {
                return Err(bad("the stream was already finished"));
            };
            Ok(reply_json(&stream.finish().await?))
        })
    }

    /// Cancels the run.
    fn cancel(&self) {
        self.cancel.cancel();
    }
}

// ---------- collections (design 0025) ----------

fn from_retrieval(error: criogen_agent::retrieval::RetrievalError) -> Failure {
    Failure::new(error.code(), error.to_string())
}

fn hit_json(hit: &criogen_agent::retrieval::Hit) -> Value {
    json!({ "id": hit.id, "text": hit.text, "metadata": hit.metadata, "score": hit.score })
}

/// Documents searched by meaning. Made empty, filled by `open`.
#[pyclass(frozen)]
struct NativeCollection {
    collection: Arc<Mutex<Option<criogen_agent::retrieval::Collection>>>,
}

impl NativeCollection {
    fn get(&self) -> Result<criogen_agent::retrieval::Collection, Failure> {
        lock(&self.collection)
            .clone()
            .ok_or_else(|| bad("the collection is not open"))
    }
}

#[pymethods]
impl NativeCollection {
    #[new]
    fn new() -> Self {
        Self {
            collection: Arc::new(Mutex::new(None)),
        }
    }

    /// Opens `name` in `storage` (in memory without one): spec `{name,
    /// embedder, index: "flat" | "hnsw", ef_search?}`. Returns the count.
    fn open<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
        storage: Option<&NativeStorage>,
        spec: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let spec: Value =
            serde_json::from_str(spec).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let store = storage.map_or_else(Storage::in_memory, |s| s.storage.clone());
        let embedder = embedder(&spec["embedder"]).map_err(py_failure)?;
        let kind = match text(&spec, "index") {
            Some("hnsw") => {
                let mut config = criogen_storage::vector::HnswConfig::default();
                if let Some(ef) = spec["ef_search"].as_u64() {
                    config = config.ef_search(usize::try_from(ef).unwrap_or(usize::MAX));
                }
                criogen_agent::retrieval::IndexKind::Hnsw(config)
            }
            _ => criogen_agent::retrieval::IndexKind::Flat,
        };
        let name = text(&spec, "name").unwrap_or_default().to_string();
        let slot = self.collection.clone();
        spawn(py, event_loop, None, async move {
            let collection = criogen_agent::retrieval::Collection::open_with(
                store.memory().clone(),
                &name,
                embedder,
                kind,
            )
            .await
            .map_err(from_retrieval)?;
            let count = collection.len().await;
            *lock(&slot) = Some(collection);
            Ok(json!(count))
        })
    }

    /// Adds `[{id, text, metadata}]`.
    fn add<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
        documents: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let raw: Vec<Value> =
            serde_json::from_str(documents).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let mut docs = Vec::with_capacity(raw.len());
        for d in raw {
            let mut doc = criogen_agent::retrieval::Document::new(
                text(&d, "id").unwrap_or_default(),
                text(&d, "text").unwrap_or_default(),
            );
            for (k, v) in d["metadata"].as_object().cloned().unwrap_or_default() {
                doc = doc.meta(k, v);
            }
            docs.push(doc);
        }
        let collection = self.get().map_err(py_failure)?;
        spawn(py, event_loop, None, async move {
            collection.add(docs).await.map_err(from_retrieval)?;
            Ok(json!(collection.len().await))
        })
    }

    /// The hits for `query`: `[{id, text, metadata, score}]`.
    fn search<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
        query: String,
        k: usize,
        filter: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let filter: Map<String, Value> =
            serde_json::from_str(filter).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let collection = self.get().map_err(py_failure)?;
        spawn(py, event_loop, None, async move {
            let hits = collection
                .search_where(&query, k, &filter)
                .await
                .map_err(from_retrieval)?;
            Ok(Value::Array(hits.iter().map(hit_json).collect()))
        })
    }

    /// `remove` (`true` if it was there), `reload` or `count`.
    fn op<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
        op: String,
        id: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let collection = self.get().map_err(py_failure)?;
        spawn(py, event_loop, None, async move {
            match op.as_str() {
                "remove" => Ok(json!(
                    collection
                        .remove(&id.unwrap_or_default())
                        .await
                        .map_err(from_retrieval)?
                )),
                "reload" => {
                    collection.reload().await.map_err(from_retrieval)?;
                    Ok(json!(collection.len().await))
                }
                _ => Ok(json!(collection.len().await)),
            }
        })
    }
}

// ---------- the trace viewer (design 0023) ----------

/// The local trace viewer, serving in the background.
#[pyclass(frozen)]
struct NativeTraceViewer {
    running: Arc<Mutex<Option<criogen_trace::RunningViewer>>>,
    url: String,
}

#[pymethods]
impl NativeTraceViewer {
    /// Starts serving (blocking briefly, without the GIL): spec as
    /// `criogen_trace::ViewerBuilder::from_spec`.
    #[new]
    fn new(py: Python<'_>, spec: &str) -> PyResult<Self> {
        let spec: Value =
            serde_json::from_str(spec).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let running = py
            .detach(|| {
                runtime().block_on(async {
                    let (builder, address) = criogen_trace::ViewerBuilder::from_spec(&spec)?;
                    criogen_trace::start(builder, address).await
                })
            })
            .map_err(|e| py_failure(bad(e.to_string())))?;
        Ok(Self {
            url: running.url(),
            running: Arc::new(Mutex::new(Some(running))),
        })
    }

    /// The page's address.
    #[getter]
    fn url(&self) -> String {
        self.url.clone()
    }

    /// Stops serving.
    fn stop<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let running = lock(&self.running).take();
        spawn(py, event_loop, None, async move {
            if let Some(running) = running {
                running.stop().await;
            }
            Ok(Value::Null)
        })
    }
}

// ---------- MCP servers (design 0022) ----------

fn from_mcp(error: McpError) -> Failure {
    Failure::new(error.code(), error.message)
}

/// A tool's output as JSON for Python: its text, its value, or its
/// content blocks.
fn tool_output_json(output: &ToolOutput) -> Value {
    match output {
        ToolOutput::Text(text) => json!({ "text": text }),
        ToolOutput::Json(value) => json!({ "value": value }),
        ToolOutput::Blocks(blocks) => json!({ "content": to_json(blocks) }),
        ToolOutput::Rich { content, .. } => json!({ "content": to_json(content) }),
        _ => Value::Null,
    }
}

/// A connected MCP server. Made empty, filled by `connect`.
#[pyclass(frozen)]
struct NativeMcp {
    client: Arc<Mutex<Option<McpClient>>>,
}

impl NativeMcp {
    fn client(&self) -> Result<McpClient, Failure> {
        lock(&self.client)
            .clone()
            .ok_or_else(|| Failure::new("mcp_closed", "the MCP server is not connected"))
    }
}

#[pymethods]
impl NativeMcp {
    #[new]
    fn new() -> Self {
        Self {
            client: Arc::new(Mutex::new(None)),
        }
    }

    /// Starts or reaches the server: `{"info", "tools"}`.
    fn connect<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
        spec: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let spec: Value =
            serde_json::from_str(spec).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let server = McpServer::from_spec(&spec).map_err(|e| py_failure(from_mcp(e)))?;
        let slot = self.client.clone();
        spawn(py, event_loop, None, async move {
            let client = server.connect().await.map_err(from_mcp)?;
            let described = client.describe();
            *lock(&slot) = Some(client);
            Ok(described)
        })
    }

    /// Lists the tools again: `{"info", "tools"}`.
    fn refresh<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = self.client().map_err(py_failure)?;
        spawn(py, event_loop, None, async move {
            client.refresh().await.map_err(from_mcp)?;
            Ok(client.describe())
        })
    }

    /// Calls a tool: `{"ok": true, ...output}` or `{"ok": false, "error"}`.
    fn call<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
        name: String,
        args: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let args: Map<String, Value> =
            serde_json::from_str(args).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let client = self.client().map_err(py_failure)?;
        spawn(py, event_loop, None, async move {
            Ok(match client.call(&name, args).await {
                Ok(output) => {
                    let mut value = tool_output_json(&output);
                    value["ok"] = json!(true);
                    value
                }
                Err(error) => json!({ "ok": false, "error": error.message }),
            })
        })
    }

    /// Stops the server.
    fn close<'py>(
        &self,
        py: Python<'py>,
        event_loop: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = lock(&self.client).clone();
        spawn(py, event_loop, None, async move {
            if let Some(client) = client {
                client.close().await;
            }
            Ok(Value::Null)
        })
    }
}

/// The named providers (design 0006 §4.1) as JSON: `[{"name", "title",
/// "base_url", "key_env", "anthropic"}]`. The SDK's own key table is
/// checked against it in the tests.
fn presets_json() -> String {
    let all: Vec<Value> = criogen_providers::PRESETS
        .iter()
        .map(|p| {
            serde_json::json!({"name": p.name, "title": p.title, "base_url": p.base_url,
                "key_env": p.key_env, "anthropic": p.anthropic})
        })
        .collect();
    Value::Array(all).to_string()
}

/// The problems with `value` against JSON Schema `schema` (both JSON), as a
/// JSON list; empty when it matches. The engine's validator (0002), for
/// evals' `json_schema` scorer (0024).
#[pyfunction]
fn validate_json(schema: &str, value: &str) -> PyResult<String> {
    let schema: Value =
        serde_json::from_str(schema).map_err(|e| PyValueError::new_err(e.to_string()))?;
    let value: Value =
        serde_json::from_str(value).map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(json!(validate_arguments(&schema, &value)).to_string())
}

/// The module.
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<NativeAgent>()?;
    m.add_class::<NativeGateway>()?;
    m.add_class::<NativeChatStream>()?;
    m.add_class::<NativeStorage>()?;
    m.add_class::<NativeWorkflow>()?;
    m.add_class::<NativeTelemetry>()?;
    m.add_class::<NativeStream>()?;
    m.add_class::<NativeMcp>()?;
    m.add_class::<NativeTraceViewer>()?;
    m.add_class::<NativeCollection>()?;
    m.add_function(wrap_pyfunction!(validate_json, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("PRESETS", presets_json())?;
    Ok(())
}
