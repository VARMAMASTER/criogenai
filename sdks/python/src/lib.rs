//! The Python SDK's native module `agentbuild._native` (design 0011): a
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

use agentbuild_agent::{
    Agent, AgentError, AgentEvent, AgentInput, AgentResult, Approval, Decision, Limits, Memory,
    Reply, Thread, ThreadStream, ToolContext, ToolError, tool_fn,
};
use agentbuild_core::{CancelToken, RunOptions};
use agentbuild_providers::{
    Endpoint, FakeModel, FakeReply, Model, ProviderModel, ToolAnnotations, ToolSpec,
};
use agentbuild_storage::{AuditQuery, Keyring, Namespace, OnBusy, Retention, SearchQuery, Storage};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyTuple};
use serde_json::{Map, Value, json};

// ---------- runtime and registries ----------

/// The run metadata key that names the caller's event loop (an id into
/// [`loops`]), so async tools run on it.
const LOOP_KEY: &str = "agentbuild_python_loop";

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("agentbuild")
            .build()
            .expect("the agentbuild Tokio runtime starts")
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

/// The caller's event loop of the run a tool belongs to.
fn loop_of(ctx: &ToolContext) -> Option<Arc<Py<PyAny>>> {
    let id = ctx.metadata()?.get(LOOP_KEY)?.as_u64()?;
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

impl From<agentbuild_storage::StorageError> for Failure {
    fn from(error: agentbuild_storage::StorageError) -> Self {
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
    let bridge = py.import("agentbuild._bridge")?;
    match outcome {
        Ok(value) => {
            event_loop.call_method1(
                py,
                "call_soon_threadsafe",
                (bridge.getattr("resolve")?, future, value.to_string()),
            )?;
        }
        Err(failure) => {
            let cause = failure
                .details
                .as_ref()
                .and_then(|d| d.get("call_id"))
                .and_then(Value::as_str)
                .and_then(|id| lock(tool_exceptions()).remove(id));
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
        let mut fake = FakeModel::with_delay(delay, delay);
        for reply in spec["replies"].as_array().cloned().unwrap_or_default() {
            fake = fake.then(fake_reply(&reply)?);
        }
        return Ok(Arc::new(fake));
    }
    let name = text(spec, "model").ok_or_else(|| bad("a model needs a `model` name"))?;
    let key = text(spec, "api_key").map(str::to_string);
    let need_key = || {
        key.clone()
            .ok_or_else(|| bad(format!("the `{kind}` model needs an API key")))
    };
    let mut endpoint = match kind {
        "openai" => Endpoint::openai(need_key()?),
        "gemini" => Endpoint::gemini(need_key()?),
        "anthropic" => Endpoint::anthropic(need_key()?),
        "openrouter" => Endpoint::openrouter(need_key()?),
        "ollama" => Endpoint::ollama(),
        "openai_compatible" => Endpoint::openai_compatible(
            text(spec, "name").unwrap_or("custom"),
            text(spec, "base_url").ok_or_else(|| bad("`openai_compatible` needs a `base_url`"))?,
            key.clone(),
        ),
        other => return Err(bad(format!("unknown model kind `{other}`"))),
    };
    if let Some(base) = text(spec, "base_url")
        && kind != "openai_compatible"
    {
        endpoint = endpoint.with_base_url(base);
    }
    ProviderModel::new(endpoint, name)
        .map(|m| Arc::new(m) as Arc<dyn Model>)
        .map_err(|err| Failure::new("model_failed", err.to_string()))
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
    Ok(memory)
}

fn limits(spec: &Value) -> Limits {
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
    limits
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

/// Calls a Python tool: `adapter(args_json, ctx_json)` returns the
/// envelope, or (async tools) a coroutine that does, run on the caller's
/// event loop.
async fn call_tool(
    adapter: Arc<Py<PyAny>>,
    is_async: bool,
    args: Map<String, Value>,
    ctx: ToolContext,
) -> Result<Value, ToolError> {
    let args_json = Value::Object(args).to_string();
    let ctx_json = json!({"call_id": ctx.call_id, "run_id": ctx.run_id}).to_string();
    let answer = if is_async {
        let event_loop = loop_of(&ctx).ok_or_else(|| {
            ToolError::fatal(
                "an async Python tool needs the caller's event loop: use the async API \
                 (or run_sync), not a run started elsewhere",
            )
        })?;
        let (sender, receiver) = tokio::sync::oneshot::channel::<Answer>();
        let sender = Mutex::new(Some(sender));
        let scheduled = Python::attach(|py| -> PyResult<()> {
            let coroutine = adapter.call1(py, (args_json, ctx_json))?;
            let running = py
                .import("asyncio")?
                .call_method1("run_coroutine_threadsafe", (coroutine, event_loop.bind(py)))?;
            let on_done = PyCFunction::new_closure(
                py,
                None,
                None,
                move |a: &Bound<'_, PyTuple>, _: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
                    let done = a.get_item(0)?;
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
                .map_err(|_| ToolError::fatal("the Python tool was abandoned"))?,
            Err(err) => Python::attach(|py| raised(py, err)),
        }
    } else {
        tokio::task::spawn_blocking(move || {
            Python::attach(|py| {
                match adapter
                    .call1(py, (args_json, ctx_json))
                    .and_then(|v| v.extract::<String>(py))
                {
                    Ok(envelope) => Answer::Envelope(envelope),
                    Err(err) => raised(py, err),
                }
            })
        })
        .await
        .map_err(|_| ToolError::fatal("the Python tool panicked"))?
    };
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
            lock(tool_exceptions()).insert(ctx.call_id.clone(), exception);
            Err(ToolError::fatal(text))
        }
    }
}

// ---------- the agent ----------

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

fn prune_json(report: agentbuild_storage::PruneReport) -> Value {
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
            .import("agentbuild._bridge")
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
    #[pyo3(signature = (config, tools, storage=None))]
    fn new(
        config: &str,
        tools: Vec<(Py<PyAny>, bool)>,
        storage: Option<&NativeStorage>,
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
                call_tool(adapter.clone(), is_async, args, ctx)
            }));
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
            builder = builder.limits(limits(spec));
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

/// The module.
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<NativeAgent>()?;
    m.add_class::<NativeStorage>()?;
    m.add_class::<NativeStream>()?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
