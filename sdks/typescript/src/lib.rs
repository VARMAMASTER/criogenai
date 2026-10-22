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
    Agent, AgentError, AgentEvent, AgentInput, AgentResult, Approval, Decision, Hook, HookAction,
    HookContext, HookError, Limits, Memory, OnToolException, Outcome, Reply, ResilientModel,
    Thread, ThreadStream, ToolCallRequest, ToolContext, ToolError, ToolNext, ToolOutput, tool_fn,
    validate_arguments,
};
use agentbuild_core::{
    CancelToken, NodeContext, NodeError, NodeOutput, Reducer, RetryPolicy, RunError, RunErrorKind,
    RunOptions, RunOutput, Snapshot, State, Update, Workflow, WorkflowBuilder, node_with_context,
    router_fn,
};
use agentbuild_providers::{
    BlockDelta, BoxFuture, Embedder, Endpoint, FakeEmbedder, FakeModel, FakeReply, Message,
    MessageDelta, Model, ModelError, ModelErrorKind, ModelRequest, ModelResponse, OutputSpec,
    ProviderEmbedder, ProviderModel, Settings, ToolAnnotations, ToolSpec,
};
use agentbuild_storage::{
    AuditQuery, Keyring, Namespace, OnBusy, Retention, SearchQuery, Storage, ThreadError, Turn,
    WorkflowThread,
};
use agentbuild_telemetry::{Langfuse, OtlpConfig, OtlpExporter};
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
        // JavaScript cannot read the request log: keep none.
        let mut fake = FakeModel::with_delay(delay, delay).without_request_log();
        if let Some(calls) = spec["failFirst"].as_u64() {
            fake = fake.flaky_first(calls as usize);
        }
        if spec["byTurn"].as_bool() == Some(true) {
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
    ProviderModel::new(endpoint(spec)?, name)
        .map(|m| Arc::new(m) as Arc<dyn Model>)
        .map_err(|err| failure("model_failed", err.to_string(), None))
}

/// The provider endpoint of a model or embedder spec (`kind`, `apiKey`,
/// `baseUrl`).
fn endpoint(spec: &Value) -> napi::Result<Endpoint> {
    let kind = text(spec, "kind").ok_or_else(|| bad("a model needs a `kind`"))?;
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
    Ok(endpoint)
}

/// An embedder (design 0017 §3): a provider's `/embeddings`, or the fake
/// one for tests (`{"kind": "fake", "synonyms": [[word, sameAs]],
/// "failing": bool}`).
fn embedder(spec: &Value) -> napi::Result<Arc<dyn Embedder>> {
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
        .map_err(|err| failure("model_failed", err.to_string(), None))?;
    if let Some(n) = spec["dimensions"].as_u64() {
        embedder = embedder.dimensions(n as u32);
    }
    Ok(Arc::new(embedder))
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
    if let Some(spec) = spec.get("embedder").filter(|e| e.is_object()) {
        memory = memory.semantic_shared(embedder(spec)?);
    }
    Ok(memory)
}

/// A retry policy from `{maxAttempts, initialBackoffSecs, factor,
/// maxBackoffSecs, jitter}` (missing keys keep Rust's defaults).
fn retry_policy(spec: &Value) -> RetryPolicy {
    let defaults = RetryPolicy::default();
    let mut policy = RetryPolicy::new();
    if let Some(n) = spec["maxAttempts"].as_u64() {
        policy = policy.max_attempts(n as u32);
    }
    let initial = spec["initialBackoffSecs"].as_f64().map_or(
        Duration::from_millis(defaults.initial_backoff_ms),
        Duration::from_secs_f64,
    );
    let factor = spec["factor"].as_f64().unwrap_or(defaults.backoff_factor);
    let max = spec["maxBackoffSecs"].as_f64().map_or(
        Duration::from_millis(defaults.max_backoff_ms),
        Duration::from_secs_f64,
    );
    policy = policy.backoff(initial, factor, max);
    if let Some(jitter) = spec["jitter"].as_bool() {
        policy = policy.jitter(jitter);
    }
    policy
}

fn limits(spec: &Value) -> napi::Result<Limits> {
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
    if let Some(policy) = spec.get("onToolException").filter(|p| !p.is_null()) {
        limits = limits.on_tool_exception(match policy.as_str() {
            Some("returnToModel") => OnToolException::ReturnToModel,
            Some("returnGeneric") => OnToolException::ReturnGeneric,
            Some("fail") => OnToolException::Fail,
            _ => {
                return Err(bad(
                    "onToolException is \"returnToModel\", \"returnGeneric\" or \"fail\"",
                ));
            }
        });
    }
    Ok(limits)
}

// ---------- tools ----------

/// Calls a JavaScript tool and reads its envelope: `{"ok": value}`,
/// `{"toolError", "retryable"}` (the model sees it), `{"invalidArguments"}`
/// (a Standard Schema's issues, design 0017 §6) or `{"crash"}` (an
/// exception the tool did not handle: `onToolException` decides; when the
/// run fails on it, the TypeScript side keeps the exception as its cause).
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
        return Err(ToolError::crashed(crash));
    }
    if let Some(problems) = envelope.get("invalidArguments").and_then(Value::as_str) {
        return Err(ToolError::invalid_arguments(problems));
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

// ---------- hooks (design 0017 §5) ----------

/// A JavaScript hook: one function `({stage, payload}) => Promise<envelope>`;
/// only the stages it defines are called. A throw is `{"crash"}` (the
/// TypeScript side keeps it under `hook:<name>:<stage>`).
struct JsHook {
    name: String,
    function: Arc<JsTool>,
    stages: Vec<String>,
}

fn js_hook_ctx(ctx: &HookContext) -> Value {
    json!({
        "runId": ctx.run_id,
        "messages": to_json(&ctx.messages),
        "modelCalls": ctx.model_calls,
        "hookState": ctx.hook_state,
        "state": ctx.state.as_map(),
    })
}

fn js_hook_action(envelope: &Value) -> Result<HookAction, HookError> {
    let mut action = HookAction::proceed();
    if let Some(state) = envelope.get("state") {
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

fn js_output_value(output: &ToolOutput) -> Value {
    match output {
        ToolOutput::Text(text) => json!(text),
        ToolOutput::Json(value) => value.clone(),
        ToolOutput::Blocks(blocks) => to_json(blocks),
        ToolOutput::Rich { value, .. } => value.clone(),
        _ => Value::Null,
    }
}

/// `value` as the output, keeping the original's artifact and state changes.
fn js_replaced(original: ToolOutput, value: Value) -> ToolOutput {
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

impl JsHook {
    fn has(&self, stage: &str) -> bool {
        self.stages.iter().any(|s| s == stage)
    }

    async fn call(&self, stage: &str, payload: Value) -> Result<Value, String> {
        let request = json!({"stage": stage, "payload": payload}).to_string();
        let envelope = call_js(self.function.clone(), request).await?;
        match envelope.get("crash").and_then(Value::as_str) {
            Some(crash) => Err(crash.to_string()),
            None => Ok(envelope),
        }
    }

    async fn action(&self, stage: &str, ctx: &HookContext) -> Result<HookAction, HookError> {
        if !self.has(stage) {
            return Ok(HookAction::proceed());
        }
        let envelope = self
            .call(stage, js_hook_ctx(ctx))
            .await
            .map_err(HookError::new)?;
        js_hook_action(&envelope)
    }
}

impl Hook for JsHook {
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
            let call_json = |call: &ToolCallRequest| json!({"callId": call.call_id, "name": call.name, "args": call.args});
            if self.has("before_tool") {
                let envelope = self
                    .call("before_tool", call_json(&call))
                    .await
                    .map_err(ToolError::crashed)?;
                if let Some(message) = envelope.get("toolError").and_then(Value::as_str) {
                    return Err(ToolError::new(message));
                }
                if let Some(value) = envelope.get("result") {
                    return Ok(js_replaced(ToolOutput::from(Value::Null), value.clone()));
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
            let envelope = self
                .call(
                    "after_tool",
                    json!({"call": snapshot, "result": js_output_value(&output)}),
                )
                .await
                .map_err(ToolError::crashed)?;
            Ok(match envelope.get("result") {
                Some(value) => js_replaced(output, value.clone()),
                None => output,
            })
        })
    }

    fn on_end<'a>(&'a self, ctx: &'a HookContext, outcome: Outcome) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if !self.has("on_end") {
                return;
            }
            let mut payload = js_hook_ctx(ctx);
            payload["outcome"] = json!(match outcome {
                Outcome::Completed => "completed",
                Outcome::Suspended => "suspended",
                _ => "failed",
            });
            // on_end cannot change the outcome: its errors are dropped.
            let _ = self.call("on_end", payload).await;
        })
    }
}

// ---------- telemetry (design 0017 §4) ----------

/// Exporters by handle, so an agent's configuration can name one.
fn exporters() -> &'static Mutex<HashMap<u32, Arc<OtlpExporter>>> {
    static EXPORTERS: OnceLock<Mutex<HashMap<u32, Arc<OtlpExporter>>>> = OnceLock::new();
    EXPORTERS.get_or_init(Mutex::default)
}

/// A telemetry exporter: OTLP over HTTP, or Langfuse. Agents built with
/// its handle send every run to it.
#[napi]
pub struct NativeTelemetry {
    exporter: Arc<OtlpExporter>,
    handle: u32,
}

impl Drop for NativeTelemetry {
    fn drop(&mut self) {
        lock(exporters()).remove(&self.handle);
    }
}

#[napi]
impl NativeTelemetry {
    /// `{"kind": "langfuse", publicKey?, secretKey?, baseUrl?}` (missing keys
    /// from the LANGFUSE_* variables) or `{"kind": "otlp", endpoint,
    /// headers}`; both take `serviceName` and `captureContent`.
    #[napi(constructor)]
    pub fn new(spec: String) -> napi::Result<Self> {
        let spec = parse(&spec)?;
        let mut config = match text(&spec, "kind") {
            Some("langfuse") => match (text(&spec, "publicKey"), text(&spec, "secretKey")) {
                (Some(public), Some(secret)) => Langfuse::config(
                    public,
                    secret,
                    text(&spec, "baseUrl").unwrap_or(agentbuild_telemetry::DEFAULT_BASE_URL),
                ),
                _ => Langfuse::from_env().map_err(|err| bad(err.to_string()))?,
            },
            Some("otlp") => {
                let mut config = OtlpConfig::new(
                    text(&spec, "endpoint").ok_or_else(|| bad("otlp needs an endpoint"))?,
                );
                for (name, value) in spec["headers"].as_object().cloned().unwrap_or_default() {
                    config = config.header(name, value.as_str().unwrap_or_default());
                }
                config
            }
            _ => return Err(bad("telemetry is \"langfuse\" or \"otlp\"")),
        };
        if let Some(name) = text(&spec, "serviceName") {
            config = config.service_name(name);
        }
        if let Some(capture) = spec["captureContent"].as_bool() {
            config = config.capture_content(capture);
        }
        let exporter = within_runtime_if_available(|| OtlpExporter::start(config))
            .map_err(|err| bad(err.to_string()))?;
        let exporter = Arc::new(exporter);
        static NEXT: AtomicU32 = AtomicU32::new(1);
        let handle = NEXT.fetch_add(1, Ordering::Relaxed);
        lock(exporters()).insert(handle, exporter.clone());
        Ok(Self { exporter, handle })
    }

    /// This exporter's handle, for an agent's configuration.
    #[napi(getter)]
    pub fn handle(&self) -> u32 {
        self.handle
    }

    /// Waits until everything sent so far is exported.
    #[napi]
    pub async fn flush(&self) {
        self.exporter.flush().await;
    }

    /// Exports what is left and stops.
    #[napi]
    pub async fn shutdown(&self) {
        self.exporter.shutdown().await;
    }

    /// `{"exported", "failed", "dropped", "lastError"}`.
    #[napi]
    pub fn stats(&self) -> String {
        let stats = self.exporter.stats();
        json!({
            "exported": stats.exported,
            "failed": stats.failed,
            "dropped": stats.dropped_events,
            "lastError": self.exporter.last_error(),
        })
        .to_string()
    }
}

/// An agent built from a TypeScript configuration.
#[napi]
pub struct NativeAgent {
    agent: Agent,
    handle: u32,
}

/// Agents by handle, so a workflow (built from JavaScript, where only
/// numbers and strings cross) can use one as a node.
fn agents() -> &'static Mutex<HashMap<u32, Agent>> {
    static AGENTS: OnceLock<Mutex<HashMap<u32, Agent>>> = OnceLock::new();
    AGENTS.get_or_init(Mutex::default)
}

impl Drop for NativeAgent {
    fn drop(&mut self) {
        lock(agents()).remove(&self.handle);
    }
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
    pub fn new(
        config: String,
        tools: Vec<JsTool>,
        storage: &NativeStorage,
        hooks: Vec<JsTool>,
    ) -> napi::Result<Self> {
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
        // Hooks, in order (design 0017 §5): `config.hooks` names their
        // stages, one function each.
        let specs = config["hooks"].as_array().cloned().unwrap_or_default();
        if specs.len() != hooks.len() {
            return Err(bad("one function per hook"));
        }
        for (spec, function) in specs.iter().zip(hooks) {
            builder = builder.hook(JsHook {
                name: spec["name"].as_str().unwrap_or("hook").to_string(),
                function: Arc::new(function),
                stages: strings(&spec["stages"]),
            });
        }
        // Every run of this agent goes to the exporter (design 0017 §4).
        if let Some(handle) = config["telemetry"].as_u64() {
            let exporter = lock(exporters())
                .get(&(handle as u32))
                .cloned()
                .ok_or_else(|| bad("the telemetry exporter is gone"))?;
            builder = builder.observe(exporter.observer());
        }
        // Agents as tools of this one (design 0017 §1), by handle.
        for spec in config["agentTools"].as_array().cloned().unwrap_or_default() {
            let handle = spec["handle"].as_u64().unwrap_or_default() as u32;
            let agent = lock(agents())
                .get(&handle)
                .cloned()
                .ok_or_else(|| bad("an agent tool's agent is gone"))?;
            builder = builder.tool(agent.as_tool(
                text(&spec, "name").unwrap_or_default(),
                text(&spec, "description").unwrap_or_default(),
            ));
        }
        // Retries and fallback models (design 0017 §2).
        for spec in config["fallbacks"].as_array().cloned().unwrap_or_default() {
            builder = builder.fallback_shared(model(&spec)?);
        }
        if let Some(kinds) = config.get("fallbackOn").filter(|k| k.is_array()) {
            let kinds: Vec<ModelErrorKind> = serde_json::from_value(kinds.clone())
                .map_err(|err| bad(format!("fallbackOn: {err}")))?;
            builder = builder.fallback_on(kinds);
        }
        if let Some(spec) = config.get("modelRetry").filter(|s| s.is_object()) {
            builder = builder.model_retry(retry_policy(spec));
        }
        if let Some(spec) = config.get("toolRetry").filter(|s| s.is_object()) {
            builder = builder.tool_retry(retry_policy(spec));
        }
        if let Some(secs) = config["maxRetryAfterSecs"].as_f64() {
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
            builder = builder.limits(limits(spec)?);
        }
        if let Some(spec) = config.get("memory").filter(|s| s.is_object()) {
            builder = builder.memory(memory(spec)?);
        }
        let agent = builder
            .build()
            .map_err(|err| failure("invalid_agent", err.to_string(), None))?;
        static NEXT: AtomicU32 = AtomicU32::new(1);
        let handle = NEXT.fetch_add(1, Ordering::Relaxed);
        lock(agents()).insert(handle, agent.clone());
        Ok(Self { agent, handle })
    }

    /// This agent's handle, for a workflow node.
    #[napi(getter)]
    pub fn handle(&self) -> u32 {
        self.handle
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

// ---------- the gateway (design 0018) ----------

/// The messages of `input`: text, or messages whose `content` may be a
/// string (one text block).
fn gateway_messages(input: &Value) -> napi::Result<Vec<Message>> {
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
fn gateway_request(payload: &Value) -> napi::Result<ModelRequest> {
    let mut messages = gateway_messages(&payload["input"])?;
    if let Some(instructions) = text(payload, "instructions") {
        messages.insert(0, Message::system(instructions));
    }
    let mut settings = Settings::default();
    settings.temperature = payload["temperature"].as_f64();
    settings.max_tokens = payload["maxTokens"]
        .as_u64()
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX));
    let mut request = ModelRequest::new(messages).with_settings(settings);
    if let Some(schema) = payload.get("output").filter(|o| !o.is_null()) {
        request.output = Some(OutputSpec::new(schema.clone()));
    }
    Ok(request)
}

fn model_failure(error: ModelError) -> napi::Error {
    failure(
        "model_failed",
        error.to_string(),
        Some(json!({"model_error": error})),
    )
}

/// The reply, with the parsed and checked output when one was asked for.
fn chat_reply(response: ModelResponse, schema: Option<&Value>) -> napi::Result<Value> {
    let text = response.message.text();
    let output = match schema {
        None => Value::Null,
        Some(schema) => {
            let value: Value = serde_json::from_str(text.trim()).map_err(|err| {
                failure(
                    "output_invalid",
                    format!("the reply is not JSON: {err}"),
                    None,
                )
            })?;
            let problems = validate_arguments(schema, &value);
            if !problems.is_empty() {
                return Err(failure(
                    "output_invalid",
                    format!(
                        "the reply does not match the schema: {}",
                        problems.join("; ")
                    ),
                    Some(json!({"problems": problems})),
                ));
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
        "stopReason": to_json(&response.finish_reason),
        "model": model,
        "output": output,
    }))
}

/// `work`, or `cancelled` once `cancel` fires (the HTTP request is dropped).
async fn or_cancelled<T>(
    work: impl std::future::Future<Output = napi::Result<T>>,
    cancel: CancelToken,
) -> napi::Result<T> {
    use std::future::Future as _;
    let mut work = std::pin::pin!(work);
    let mut cancelled = std::pin::pin!(cancel.cancelled());
    std::future::poll_fn(|cx| {
        if let std::task::Poll::Ready(outcome) = work.as_mut().poll(cx) {
            return std::task::Poll::Ready(outcome);
        }
        if cancelled.as_mut().poll(cx).is_ready() {
            return std::task::Poll::Ready(Err(failure(
                "cancelled",
                "the call was cancelled",
                None,
            )));
        }
        std::task::Poll::Pending
    })
    .await
}

/// Model calls without an agent: the agent's retries and fallbacks
/// (`ResilientModel`) around the model each call names.
#[napi]
pub struct NativeGateway {
    fallbacks: Vec<Arc<dyn Model>>,
    fallback_on: Vec<ModelErrorKind>,
    retry: RetryPolicy,
    max_retry_after: Duration,
    /// Models by their spec: each provider client is made once.
    models: Mutex<HashMap<String, Arc<dyn Model>>>,
}

impl NativeGateway {
    fn chain(&self, spec: &Value) -> napi::Result<ResilientModel> {
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

#[napi]
impl NativeGateway {
    /// Config `{fallbacks, fallbackOn, retry, maxRetryAfterSecs}`.
    #[napi(constructor)]
    pub fn new(config: String) -> napi::Result<Self> {
        let config = parse(&config)?;
        let fallbacks = config["fallbacks"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(model)
            .collect::<napi::Result<Vec<_>>>()?;
        let fallback_on = match config.get("fallbackOn").filter(|k| k.is_array()) {
            Some(kinds) => serde_json::from_value(kinds.clone())
                .map_err(|err| bad(format!("fallbackOn: {err}")))?,
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
                config["maxRetryAfterSecs"].as_f64().unwrap_or(60.0),
            ),
            models: Mutex::default(),
        })
    }

    /// One model call: payload `{model, input, instructions, temperature,
    /// maxTokens, output}`.
    #[napi]
    pub async fn chat(&self, payload: String, cancel_id: u32) -> napi::Result<String> {
        let cancel = Registered::take(cancel_id);
        let payload = parse(&payload)?;
        let chain = self.chain(&payload["model"])?;
        let request = gateway_request(&payload)?;
        let work = async {
            let response = chain.invoke(&request).await.map_err(model_failure)?;
            chat_reply(response, request.output.as_ref().map(|o| &o.schema))
        };
        Ok(or_cancelled(work, cancel.1.clone()).await?.to_string())
    }

    /// The same call, streamed: text chunks, then the reply.
    #[napi]
    pub fn stream(&self, payload: String) -> napi::Result<NativeChatStream> {
        let payload = parse(&payload)?;
        let chain = self.chain(&payload["model"])?;
        let request = gateway_request(&payload)?;
        let (chunks, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (reply, reply_receiver) = tokio::sync::oneshot::channel();
        let cancel = CancelToken::new();
        let token = cancel.clone();
        let task = async move {
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
            let _ = reply.send(or_cancelled(work, token).await);
        };
        napi::bindgen_prelude::spawn(task);
        Ok(NativeChatStream {
            chunks: Arc::new(tokio::sync::Mutex::new(receiver)),
            reply: Arc::new(tokio::sync::Mutex::new(Some(reply_receiver))),
            cancel,
        })
    }

    /// Embeddings: payload `{embedder, texts}`.
    #[napi]
    pub async fn embed(&self, payload: String, cancel_id: u32) -> napi::Result<String> {
        let cancel = Registered::take(cancel_id);
        let payload = parse(&payload)?;
        let embedder = embedder(&payload["embedder"])?;
        let texts: Vec<String> = serde_json::from_value(payload["texts"].clone())
            .map_err(|err| bad(format!("texts: {err}")))?;
        let work = async {
            let vectors = embedder.embed(&texts).await.map_err(model_failure)?;
            Ok(json!(vectors))
        };
        Ok(or_cancelled(work, cancel.1.clone()).await?.to_string())
    }
}

/// A streamed gateway call.
#[napi]
pub struct NativeChatStream {
    chunks: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Value>>>,
    reply: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<napi::Result<Value>>>>>,
    cancel: CancelToken,
}

#[napi]
impl NativeChatStream {
    /// The next chunk as JSON, or `null` after the last.
    #[napi]
    pub async fn next(&self) -> napi::Result<Option<String>> {
        Ok(self.chunks.lock().await.recv().await.map(|c| c.to_string()))
    }

    /// Waits for the call; its reply as JSON.
    #[napi]
    pub async fn reply(&self) -> napi::Result<String> {
        let taken = self.reply.lock().await.take();
        let Some(receiver) = taken else {
            return Err(bad("the reply was already taken"));
        };
        let reply = receiver
            .await
            .unwrap_or_else(|_| Err(failure("cancelled", "the call was cancelled", None)))?;
        Ok(reply.to_string())
    }

    /// Cancels the call.
    #[napi]
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

// ---------- workflows (design 0015) ----------

/// Calls a JavaScript node or router: request JSON in, envelope JSON out.
async fn call_js(function: Arc<JsTool>, request: String) -> Result<Value, String> {
    let promise = function
        .call_async(request)
        .await
        .map_err(|err| format!("could not call the function: {err}"))?;
    let envelope = promise
        .await
        .map_err(|err| format!("the function failed: {}", err.reason))?;
    serde_json::from_str(&envelope).map_err(|err| format!("unreadable answer: {err}"))
}

/// A node's or router's exception, for the run error: the TypeScript side
/// keeps what was thrown under this key.
fn cause_key(node: &str, message: &str) -> String {
    format!("node:{node}:{message}")
}

/// A JavaScript function as a node. Envelopes: `{"update"}`,
/// `{"suspend", "update"}`, `{"goto", "update"}` or `{"crash"}`.
fn js_node(name: String, function: Arc<JsTool>) -> impl agentbuild_core::Node {
    node_with_context(move |ctx: NodeContext, state: Arc<State>| {
        let (name, function) = (name.clone(), function.clone());
        async move {
            let request = json!({
                "node": name,
                "state": state.as_map(),
                "ctx": {
                    "runId": ctx.run_id(), "step": ctx.step(), "node": ctx.node(),
                    "attempt": ctx.attempt(), "resume": ctx.resume(),
                },
            })
            .to_string();
            let envelope = call_js(function, request).await.map_err(NodeError::new)?;
            if let Some(crash) = envelope.get("crash").and_then(Value::as_str) {
                return Err(NodeError::new(crash));
            }
            let update = match envelope.get("update") {
                Some(Value::Object(map)) => Update::from(map.clone()),
                Some(Value::Null) | None => Update::new(),
                Some(other) => {
                    return Err(NodeError::new(format!(
                        "a node returns an object (the update), not {other}"
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

/// A JavaScript function as a router: `{"label"}` or `{"crash"}`.
fn js_router(from: String, function: Arc<JsTool>) -> impl agentbuild_core::Router {
    router_fn(move |state: Arc<State>| {
        let (from, function) = (from.clone(), function.clone());
        async move {
            let request = json!({"node": from, "state": state.as_map()}).to_string();
            let envelope = call_js(function, request).await.map_err(NodeError::new)?;
            if let Some(crash) = envelope.get("crash").and_then(Value::as_str) {
                return Err(NodeError::new(crash));
            }
            envelope
                .get("label")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| NodeError::new("a router returns a label (a string)"))
        }
    })
}

fn run_failure(error: RunError) -> napi::Error {
    let code = error.kind.code().to_string();
    let mut details = json!({"step": error.step});
    if let Some(snapshot) = error.snapshot() {
        details["snapshot"] = to_json(snapshot);
        details["runId"] = json!(snapshot.run_id);
    }
    match &error.kind {
        RunErrorKind::NodeFailed { node, source } | RunErrorKind::RouterFailed { node, source } => {
            details["node"] = json!(node);
            details["cause_key"] = json!(cause_key(node, &source.message));
        }
        RunErrorKind::NodeTimeout { node, .. } | RunErrorKind::NodePanicked { node, .. } => {
            details["node"] = json!(node);
        }
        _ => {}
    }
    failure(&code, error.to_string(), Some(details))
}

fn thread_failure(error: ThreadError) -> napi::Error {
    match error {
        ThreadError::Run(run) => run_failure(*run),
        other => failure(other.code(), other.to_string(), None),
    }
}

fn reducer(name: &str) -> napi::Result<Reducer> {
    match name {
        "replace" => Ok(Reducer::Replace),
        "append" => Ok(Reducer::Append),
        "upsertById" | "upsert_by_id" => Ok(Reducer::UpsertById),
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
        "runId": run.run_id,
    })
}

fn workflow_options(payload: &Value, cancel: &CancelToken) -> RunOptions {
    let mut options = RunOptions::default().cancel_token(cancel.clone());
    if let Some(n) = payload["maxSteps"].as_u64() {
        options = options.max_steps(n as usize);
    }
    if let Some(ms) = payload["deadlineMs"].as_u64() {
        options = options.deadline(Duration::from_millis(ms));
    }
    options
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect()
}

/// A compiled workflow (design 0015).
#[napi]
pub struct NativeWorkflow {
    workflow: Workflow,
    storage: Storage,
}

#[napi]
impl NativeWorkflow {
    /// Builds the graph. `nodes`: one function per name in `config.nodes`;
    /// `routers`: one per entry of `config.routes`; agents come by handle in
    /// `config.agents`.
    #[napi(constructor)]
    pub fn new(
        config: String,
        nodes: Vec<JsTool>,
        routers: Vec<JsTool>,
        storage: &NativeStorage,
    ) -> napi::Result<Self> {
        let config = parse(&config)?;
        let mut builder = WorkflowBuilder::new();
        let names = strings(&config["nodes"]);
        if names.len() != nodes.len() {
            return Err(bad("one function per node"));
        }
        for (name, function) in names.into_iter().zip(nodes) {
            builder.add_node(name.clone(), js_node(name, Arc::new(function)));
        }
        for (name, handle) in config["agents"].as_object().cloned().unwrap_or_default() {
            let agent = lock(agents())
                .get(&(handle.as_u64().unwrap_or_default() as u32))
                .cloned()
                .ok_or_else(|| bad(format!("node `{name}`: the agent is gone")))?;
            builder.add_node(name, agent.as_node());
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
            return Err(bad("one function per router"));
        }
        for (route, function) in routes.iter().zip(routers) {
            let from = text(route, "from").unwrap_or_default().to_string();
            let table: Vec<(String, String)> = route["routes"]
                .as_object()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|(label, target)| (label, target.as_str().unwrap_or_default().to_string()))
                .collect();
            builder.add_router(from.clone(), js_router(from, Arc::new(function)), table);
        }
        for join in config["joins"].as_array().cloned().unwrap_or_default() {
            builder.add_join(strings(&join[0]), join[1].as_str().unwrap_or_default());
        }
        for goto in config["gotos"].as_array().cloned().unwrap_or_default() {
            builder.may_goto(goto[0].as_str().unwrap_or_default(), strings(&goto[1]));
        }
        for (key, name) in config["reducers"].as_object().cloned().unwrap_or_default() {
            builder.reducer(key, reducer(name.as_str().unwrap_or_default())?);
        }
        let workflow = builder.build().map_err(|err| {
            failure(
                "invalid_workflow",
                err.to_string(),
                Some(json!({
                    "issues": err.issues.iter().map(ToString::to_string).collect::<Vec<_>>(),
                })),
            )
        })?;
        Ok(Self {
            workflow,
            storage: storage.storage.clone(),
        })
    }

    /// Runs once: payload `{input, maxSteps, deadlineMs}`.
    #[napi]
    pub async fn run(&self, payload: String, cancel_id: u32) -> napi::Result<String> {
        let payload = parse(&payload)?;
        let cancel = Registered::take(cancel_id);
        let input = State::try_from(payload["input"].clone())
            .map_err(|_| bad("the input must be an object"))?;
        let run = self
            .workflow
            .run_with(input, workflow_options(&payload, &cancel.1))
            .await
            .map_err(run_failure)?;
        Ok(run_json(&run).to_string())
    }

    /// Continues a snapshot (no storage): payload `{snapshot, values}`.
    #[napi]
    pub async fn resume(&self, payload: String, cancel_id: u32) -> napi::Result<String> {
        let payload = parse(&payload)?;
        let cancel = Registered::take(cancel_id);
        let snapshot: Snapshot =
            serde_json::from_value(payload["snapshot"].clone()).map_err(|err| {
                failure(
                    "resume_invalid",
                    format!("unreadable snapshot: {err}"),
                    None,
                )
            })?;
        let values = payload["values"].as_object().cloned().unwrap_or_default();
        let run = self
            .workflow
            .resume_with(&snapshot, values, workflow_options(&payload, &cancel.1))
            .await
            .map_err(run_failure)?;
        Ok(run_json(&run).to_string())
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
        let mut thread = self
            .storage
            .workflow_thread(
                &self.workflow,
                text(&payload, "threadId").unwrap_or_default(),
            )
            .with_options(workflow_options(&payload, &cancel.1));
        if let Some(actor) = text(&payload, "actor") {
            thread = thread.actor(actor);
        }
        workflow_thread_operation(thread, &op, payload).await
    }
}

fn turn_json(turn: &Turn) -> Value {
    let mut value = run_json(&turn.run);
    value["version"] = json!(turn.version);
    value["checkpointId"] = json!(turn.checkpoint_id);
    value
}

async fn workflow_thread_operation(
    thread: WorkflowThread,
    op: &str,
    payload: Value,
) -> napi::Result<String> {
    let turn = match op {
        "run" => {
            let input = State::try_from(payload["input"].clone())
                .map_err(|_| bad("the input must be an object"))?;
            thread.run(input).await
        }
        "resume" => {
            let values = payload["values"].as_object().cloned().unwrap_or_default();
            thread.resume(values).await
        }
        "retry" => thread.retry().await,
        "state" => return Ok(to_json(&thread.state().await.map_err(thread_failure)?).to_string()),
        "interrupts" => {
            return Ok(to_json(&thread.interrupts().await.map_err(thread_failure)?).to_string());
        }
        "history" => {
            let limit = payload["limit"].as_u64().unwrap_or(20) as usize;
            return Ok(to_json(&thread.history(limit).await.map_err(thread_failure)?).to_string());
        }
        "rewind" => {
            let id = text(&payload, "checkpointId").unwrap_or_default();
            return Ok(to_json(&thread.rewind(id).await.map_err(thread_failure)?).to_string());
        }
        "delete" => return Ok(json!(thread.delete().await.map_err(thread_failure)?).to_string()),
        other => return Err(bad(format!("unknown workflow thread operation `{other}`"))),
    };
    turn.map(|t| turn_json(&t).to_string())
        .map_err(thread_failure)
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
