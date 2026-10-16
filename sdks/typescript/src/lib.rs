//! The TypeScript SDK's native addon (design 0012): a bridge from Node to
//! the Rust engine. TypeScript builds the configuration (as JSON) and holds
//! the user's tools; the agent loop, validation, storage and approvals run
//! here.
//!
//! Async methods return Promises (napi's Tokio runtime). Tools are
//! JavaScript functions called through thread-safe functions: each returns
//! a Promise of a JSON envelope, which Rust awaits.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
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
use napi::Status;
use napi::bindgen_prelude::{Promise, block_on, within_runtime_if_available};
use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;
use serde_json::{Map, Value, json};

/// A JavaScript tool: `(request_json) => Promise<envelope_json>`. Weak, so
/// an idle agent does not keep Node running.
type JsTool = ThreadsafeFunction<String, Promise<String>, String, Status, false, true>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------- cancellation ----------

/// Cancel tokens of calls in flight, by id (JavaScript's `AbortSignal`
/// fires them with [`cancel_call`]).
fn tokens() -> &'static Mutex<HashMap<u32, CancelToken>> {
    static TOKENS: OnceLock<Mutex<HashMap<u32, CancelToken>>> = OnceLock::new();
    TOKENS.get_or_init(Mutex::default)
}

/// A registered token; unregistered when dropped.
struct Registered(u32, CancelToken);

impl Registered {
    fn take(id: u32) -> Self {
        let token = lock(tokens()).entry(id).or_default().clone();
        Self(id, token)
    }
}

impl Drop for Registered {
    fn drop(&mut self) {
        lock(tokens()).remove(&self.0);
    }
}

/// A new cancellation id, registered at once so an abort that comes before
/// the call starts is not lost.
#[napi]
pub fn new_cancel_id() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    lock(tokens()).insert(id, CancelToken::new());
    id
}

/// Cancels the call registered under `id` (no-op once it ended).
#[napi]
pub fn cancel_call(id: u32) {
    if let Some(token) = lock(tokens()).get(&id) {
        token.cancel();
    }
}

/// Forgets an id whose call never started.
#[napi]
pub fn release_cancel_id(id: u32) {
    lock(tokens()).remove(&id);
}

// ---------- results and errors ----------

/// A failure on its way to JavaScript: the error's reason is
/// `{"code", "message", "details"}`, which the TypeScript side turns into
/// an `AgentBuildError`.
fn failure(code: &str, message: impl Into<String>, details: Option<Value>) -> napi::Error {
    let reason = json!({"code": code, "message": message.into(), "details": details});
    napi::Error::new(Status::GenericFailure, reason.to_string())
}

fn agent_error(error: AgentError) -> napi::Error {
    failure(&error.code, error.message, error.details)
}

fn storage_error(error: agentbuild_storage::StorageError) -> napi::Error {
    failure(error.code(), error.message, None)
}

fn bad(message: impl Into<String>) -> napi::Error {
    failure("invalid_value", message, None)
}

fn parse(text: &str) -> napi::Result<Value> {
    serde_json::from_str(text).map_err(|e| bad(format!("unreadable JSON: {e}")))
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
        "stopReason": to_json(&result.stop_reason),
        "modelCalls": result.model_calls,
        "toolCalls": result.tool_calls,
        "needsApproval": result.is_suspended(),
        "approvals": to_json(&result.approval_requests()),
        "runId": result.run.run_id,
    })
}

fn reply_json(reply: &Reply) -> String {
    let mut value = result_json(&reply.result);
    value["version"] = json!(reply.version);
    value["checkpointId"] = json!(reply.checkpoint_id);
    value.to_string()
}

fn event_json(event: AgentEvent) -> String {
    match event {
        AgentEvent::Text { message_id, text } => {
            json!({"type": "text", "messageId": message_id, "text": text})
        }
        AgentEvent::ToolCallStarted {
            call_id,
            tool,
            args,
        } => json!({"type": "tool_call_started", "callId": call_id, "tool": tool, "args": args}),
        AgentEvent::ToolCallFinished {
            call_id,
            tool,
            status,
        } => json!({
            "type": "tool_call_finished", "callId": call_id, "tool": tool,
            "status": to_json(&status),
        }),
        AgentEvent::Message(message) => json!({"type": "message", "message": to_json(&message)}),
        AgentEvent::ApprovalNeeded(requests) => {
            json!({"type": "approval_needed", "requests": to_json(&requests)})
        }
        other => json!({"type": "other", "event": format!("{other:?}")}),
    }
    .to_string()
}

// ---------- configuration ----------

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn model(spec: &Value) -> napi::Result<Arc<dyn Model>> {
    let kind = text(spec, "kind").ok_or_else(|| bad("a model needs a `kind`"))?;
    if kind == "fake" {
        let delay = Duration::from_millis(spec["delayMs"].as_u64().unwrap_or(0));
        let mut fake = FakeModel::with_delay(delay, delay);
        for reply in spec["replies"].as_array().cloned().unwrap_or_default() {
            fake = fake.then(fake_reply(&reply)?);
        }
        return Ok(Arc::new(fake));
    }
    let name = text(spec, "model").ok_or_else(|| bad("a model needs a `model` name"))?;
    let key = text(spec, "apiKey").map(str::to_string);
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
            text(spec, "baseUrl").ok_or_else(|| bad("`openai_compatible` needs a `baseUrl`"))?,
            key.clone(),
        ),
        other => return Err(bad(format!("unknown model kind `{other}`"))),
    };
    if let Some(base) = text(spec, "baseUrl")
        && kind != "openai_compatible"
    {
        endpoint = endpoint.with_base_url(base);
    }
    ProviderModel::new(endpoint, name)
        .map(|m| Arc::new(m) as Arc<dyn Model>)
        .map_err(|err| failure("model_failed", err.to_string(), None))
}

fn fake_reply(reply: &Value) -> napi::Result<FakeReply> {
    if let Some(text) = text(reply, "text") {
        return Ok(FakeReply::text(text));
    }
    if let Some(calls) = reply.get("toolCalls").and_then(Value::as_array) {
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
    Err(bad("a fake reply is {text} or {toolCalls: […]}"))
}

async fn open_storage(spec: &Value) -> napi::Result<Storage> {
    let mut storage = match text(spec, "url") {
        None => Storage::in_memory(),
        Some(url) => match text(spec, "encryptionKeys") {
            Some(keys) => {
                let keys = Keyring::parse(keys).map_err(storage_error)?;
                Storage::connect_encrypted(url, keys)
                    .await
                    .map_err(storage_error)?
            }
            None => Storage::connect(url).await.map_err(storage_error)?,
        },
    };
    if spec["audit"].as_bool() == Some(true) {
        storage = storage.with_audit();
    }
    if let Some(on_busy) = text(spec, "onBusy") {
        storage = storage.on_busy(match on_busy {
            "wait" => OnBusy::Wait,
            "reject" => OnBusy::Reject,
            "interrupt" => OnBusy::Interrupt,
            other => return Err(bad(format!("unknown onBusy `{other}`"))),
        });
    }
    if let Some(secs) = spec["busyTimeoutSecs"].as_f64() {
        storage = storage.busy_timeout(Duration::from_secs_f64(secs));
    }
    if let Some(retention) = spec.get("retention").filter(|r| r.is_object()) {
        let mut rules = Retention::new();
        if let Some(secs) = retention["threadTtlSecs"].as_f64() {
            rules = rules.thread_ttl(Duration::from_secs_f64(secs));
        }
        if let Some(n) = retention["keepCheckpoints"].as_u64() {
            rules = rules.keep_checkpoints(n as usize);
        }
        if retention["dropBranches"].as_bool() == Some(true) {
            rules = rules.drop_branches();
        }
        storage = storage.retention(rules);
    }
    Ok(storage)
}

fn memory(spec: &Value) -> napi::Result<Memory> {
    let namespace = |path: &str| Namespace::parse(path).map_err(storage_error);
    let mut memory = if let Some(key) = text(spec, "byState") {
        Memory::by_state(key, namespace(text(spec, "prefix").unwrap_or(""))?)
    } else if let Some(shared) = text(spec, "shared") {
        Memory::shared(namespace(shared)?)
    } else {
        return Err(bad("memory needs `byState` or `shared`"));
    };
    if let Some(n) = spec["recall"].as_u64() {
        memory = memory.recall(n as usize);
    }
    if let Some(on) = spec["tools"].as_bool() {
        memory = memory.tools(on);
    }
    if let Some(secs) = spec["ttlSecs"].as_f64() {
        memory = memory.ttl(Duration::from_secs_f64(secs));
    }
    Ok(memory)
}

fn limits(spec: &Value) -> Limits {
    let mut limits = Limits::default();
    if let Some(n) = spec["modelCalls"].as_u64() {
        limits = limits.model_calls(n as u32);
    }
    if let Some(n) = spec["toolCalls"].as_u64() {
        limits = limits.tool_calls(n as u32);
    }
    if let Some(n) = spec["tokens"].as_u64() {
        limits = limits.tokens(n);
    }
    if let Some(secs) = spec["toolTimeoutSecs"].as_f64() {
        limits = limits.tool_timeout(Duration::from_secs_f64(secs));
    }
    limits
}

// ---------- tools ----------

/// Calls a JavaScript tool and reads its envelope: `{"ok": value}`,
/// `{"toolError", "retryable"}` (the model sees it) or `{"crash"}` (the run
/// fails with `tool_failed`; the TypeScript side keeps the exception).
async fn call_tool(
    tool: Arc<JsTool>,
    args: Map<String, Value>,
    ctx: ToolContext,
) -> Result<Value, ToolError> {
    let request = json!({"args": args, "callId": ctx.call_id, "runId": ctx.run_id}).to_string();
    let promise = tool
        .call_async(request)
        .await
        .map_err(|err| ToolError::fatal(format!("could not call the tool: {err}")))?;
    let envelope = promise
        .await
        .map_err(|err| ToolError::fatal(format!("the tool failed: {}", err.reason)))?;
    let envelope: Value = serde_json::from_str(&envelope)
        .map_err(|err| ToolError::fatal(format!("unreadable tool answer: {err}")))?;
    if let Some(crash) = envelope.get("crash").and_then(Value::as_str) {
        return Err(ToolError::fatal(crash));
    }
    if let Some(message) = envelope.get("toolError").and_then(Value::as_str) {
        let error = ToolError::new(message);
        return Err(if envelope["retryable"].as_bool() == Some(true) {
            error.retryable()
        } else {
            error
        });
    }
    Ok(envelope.get("ok").cloned().unwrap_or(Value::Null))
}

// ---------- storage ----------

/// Storage built from a TypeScript configuration; agents built with it
/// share it (threads, memory, leases, audit trail).
#[napi]
pub struct NativeStorage {
    storage: Storage,
}

#[napi]
impl NativeStorage {
    /// Connects (blocks briefly: SQLite opens a file, Postgres connects).
    #[napi(constructor)]
    pub fn new(spec: String) -> napi::Result<Self> {
        let spec = parse(&spec)?;
        let storage = block_on(open_storage(&spec))?;
        Ok(Self { storage })
    }

    /// Closes the database's connections.
    #[napi]
    pub async fn close(&self) {
        self.storage.close().await;
    }

    /// A storage operation (memory, prune, audit); the result as JSON.
    #[napi]
    pub async fn op(&self, op: String, payload: String) -> napi::Result<String> {
        let payload = parse(&payload)?;
        storage_operation(self.storage.clone(), &op, payload)
            .await
            .map(|v| v.to_string())
    }
}

async fn storage_operation(storage: Storage, op: &str, payload: Value) -> napi::Result<Value> {
    let namespace = |key: &str| {
        Namespace::parse(text(&payload, key).unwrap_or_default()).map_err(storage_error)
    };
    let audit = || {
        storage
            .audit()
            .cloned()
            .ok_or_else(|| bad("the audit trail is off: new Storage({ audit: true })"))
    };
    match op {
        "memory_put" => {
            let ttl = payload["ttlSecs"].as_f64().map(Duration::from_secs_f64);
            storage
                .memory()
                .put(
                    &namespace("namespace")?,
                    text(&payload, "key").unwrap_or_default(),
                    payload["value"].clone(),
                    ttl,
                )
                .await
                .map_err(storage_error)?;
            Ok(Value::Null)
        }
        "memory_get" => Ok(to_json(
            &storage
                .memory()
                .get(
                    &namespace("namespace")?,
                    text(&payload, "key").unwrap_or_default(),
                )
                .await
                .map_err(storage_error)?,
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
                    .await
                    .map_err(storage_error)?,
            ))
        }
        "memory_delete_namespace" => Ok(json!(
            storage
                .memory()
                .delete_namespace(&namespace("namespace")?)
                .await
                .map_err(storage_error)?
        )),
        "prune" => {
            let report = storage.prune().await.map_err(storage_error)?;
            Ok(json!({
                "threadsDeleted": report.threads_deleted,
                "threadsBusy": report.threads_busy,
                "checkpointsDeleted": report.checkpoints_deleted,
                "memoryItemsDeleted": report.memory_items_deleted,
            }))
        }
        "audit_query" => {
            let mut query =
                AuditQuery::new().limit(payload["limit"].as_u64().unwrap_or(100) as usize);
            if let Some(thread) = text(&payload, "threadId") {
                query = query.thread(thread);
            }
            if let Some(action) = text(&payload, "action") {
                query = query.action(action);
            }
            if let Some(after) = payload["afterSeq"].as_u64() {
                query = query.after_seq(after);
            }
            Ok(to_json(
                &audit()?.query(&query).await.map_err(storage_error)?,
            ))
        }
        "audit_verify" => Ok(to_json(&audit()?.verify().await.map_err(storage_error)?)),
        other => Err(bad(format!("unknown storage operation `{other}`"))),
    }
}

// ---------- the agent ----------

/// An agent built from a TypeScript configuration.
#[napi]
pub struct NativeAgent {
    agent: Agent,
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

fn options(agent: &Agent, cancel: &CancelToken) -> RunOptions {
    agent.run_options().cancel_token(cancel.clone())
}

fn thread(agent: &Agent, payload: &Value, options: RunOptions) -> Thread {
    let mut thread = agent
        .thread(text(payload, "threadId").unwrap_or_default())
        .with_options(options);
    if let Some(actor) = text(payload, "actor") {
        thread = thread.actor(actor);
    }
    thread
}

fn decisions(payload: &Value) -> napi::Result<Vec<(String, Decision)>> {
    payload["decisions"]
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|(call_id, decision)| {
            serde_json::from_value(decision)
                .map(|d| (call_id, d))
                .map_err(|err| {
                    failure(
                        "resume_invalid",
                        format!("unreadable decision: {err}"),
                        None,
                    )
                })
        })
        .collect()
}

async fn thread_operation(thread: Thread, op: &str, payload: Value) -> napi::Result<String> {
    let reply = match op {
        "send" => thread.send(input(&payload)).await,
        "approve" => thread.approve().await,
        "reject" => {
            thread
                .reject(text(&payload, "message").unwrap_or("rejected"))
                .await
        }
        "decide" => thread.decide(decisions(&payload)?).await,
        "retry" => thread.retry().await,
        "history" => {
            let limit = payload["limit"].as_u64().unwrap_or(20) as usize;
            let history = thread.history(limit).await.map_err(agent_error)?;
            return Ok(to_json(&history).to_string());
        }
        "messages" => {
            let messages = thread.messages().await.map_err(agent_error)?;
            return Ok(to_json(&messages).to_string());
        }
        "rewind" => {
            let id = text(&payload, "checkpointId").unwrap_or_default();
            return Ok(to_json(&thread.rewind(id).await.map_err(agent_error)?).to_string());
        }
        "delete" => return Ok(json!(thread.delete().await.map_err(agent_error)?).to_string()),
        other => return Err(bad(format!("unknown thread operation `{other}`"))),
    };
    reply.map(|r| reply_json(&r)).map_err(agent_error)
}

#[napi]
impl NativeAgent {
    /// Builds the agent. `tools` holds one function per tool, in the
    /// configuration's order.
    #[napi(constructor)]
    pub fn new(config: String, tools: Vec<JsTool>, storage: &NativeStorage) -> napi::Result<Self> {
        let config = parse(&config)?;
        let mut builder =
            Agent::builder_shared(model(&config["model"])?).storage(storage.storage.clone());
        if let Some(instructions) = text(&config, "instructions") {
            builder = builder.instructions(instructions);
        }
        let specs = config["tools"].as_array().cloned().unwrap_or_default();
        if specs.len() != tools.len() {
            return Err(bad("one function per tool"));
        }
        for (spec, tool) in specs.iter().zip(tools) {
            let mut annotations = ToolAnnotations::default();
            annotations.read_only = spec["readOnly"].as_bool() == Some(true);
            annotations.destructive = spec["destructive"].as_bool() == Some(true);
            annotations.idempotent = spec["idempotent"].as_bool() == Some(true);
            let tool_spec = ToolSpec::new(
                text(spec, "name").unwrap_or_default(),
                text(spec, "description").unwrap_or_default(),
                spec["input"].clone(),
            )
            .with_annotations(annotations);
            let tool = Arc::new(tool);
            builder = builder.tool(tool_fn(tool_spec, move |args, ctx: ToolContext| {
                call_tool(tool.clone(), args, ctx)
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
            builder = builder.memory(memory(spec)?);
        }
        let agent = builder
            .build()
            .map_err(|err| failure("invalid_agent", err.to_string(), None))?;
        Ok(Self { agent })
    }

    /// `agent.run`: payload `{input, state}`; `cancelId` from
    /// [`new_cancel_id`].
    #[napi]
    pub async fn run(&self, payload: String, cancel_id: u32) -> napi::Result<String> {
        let payload = parse(&payload)?;
        let cancel = Registered::take(cancel_id);
        let result = self
            .agent
            .run_with(input(&payload), options(&self.agent, &cancel.1))
            .await
            .map_err(agent_error)?;
        Ok(result_json(&result).to_string())
    }

    /// A thread operation: payload `{threadId, actor, …}`.
    #[napi]
    pub async fn thread(
        &self,
        op: String,
        payload: String,
        cancel_id: u32,
    ) -> napi::Result<String> {
        let payload = parse(&payload)?;
        let cancel = Registered::take(cancel_id);
        let thread = thread(&self.agent, &payload, options(&self.agent, &cancel.1));
        thread_operation(thread, &op, payload).await
    }

    /// Streams a thread operation (`send`, `approve`, `reject`, `decide`,
    /// `retry`).
    #[napi]
    pub fn stream(&self, op: String, payload: String) -> napi::Result<NativeStream> {
        let payload = parse(&payload)?;
        let thread = thread(&self.agent, &payload, self.agent.run_options());
        let stream = match op.as_str() {
            "decide" => {
                let decisions = decisions(&payload)?;
                within_runtime_if_available(|| thread.stream_decide(decisions))
            }
            "send" => within_runtime_if_available(|| thread.stream(input(&payload))),
            "approve" => within_runtime_if_available(|| thread.stream_approve()),
            "reject" => {
                let message = text(&payload, "message").unwrap_or("rejected").to_string();
                within_runtime_if_available(|| thread.stream_reject(message))
            }
            "retry" => within_runtime_if_available(|| thread.stream_retry()),
            other => return Err(bad(format!("cannot stream `{other}`"))),
        };
        Ok(NativeStream {
            cancel: stream.cancel_token(),
            stream: Arc::new(tokio::sync::Mutex::new(Some(stream))),
        })
    }
}

/// A streamed thread operation.
#[napi]
pub struct NativeStream {
    stream: Arc<tokio::sync::Mutex<Option<ThreadStream>>>,
    /// Cancels the run even while `next` holds the stream.
    cancel: CancelToken,
}

#[napi]
impl NativeStream {
    /// The next event as JSON, or `null` after the last.
    #[napi]
    pub async fn next(&self) -> napi::Result<Option<String>> {
        let mut guard = self.stream.lock().await;
        let Some(stream) = guard.as_mut() else {
            return Ok(None);
        };
        Ok(stream.next().await.map(event_json))
    }

    /// Waits for the operation; its reply as JSON.
    #[napi]
    pub async fn finish(&self) -> napi::Result<String> {
        let taken = self.stream.lock().await.take();
        let Some(stream) = taken else {
            return Err(bad("the stream was already finished"));
        };
        stream
            .finish()
            .await
            .map(|r| reply_json(&r))
            .map_err(agent_error)
    }

    /// Cancels the run (it fails with `cancelled` and is saved).
    #[napi]
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}
