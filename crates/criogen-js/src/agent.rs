//! Agents from TypeScript: JavaScript tools and hooks, runs, threads and
//! streams.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use criogen_agent::{
    Agent, AgentBuilder, AgentError, AgentEvent, AgentInput, AgentResult, Approval, Decision, Hook,
    HookAction, HookContext, HookError, Outcome, Reply, Thread, ThreadStream, ToolCallRequest,
    ToolContext, ToolError, ToolNext, ToolOutput, tool_fn,
};
use criogen_core::{CancelToken, RunOptions};
use criogen_providers::{BoxFuture, Message, ModelErrorKind, ToolAnnotations, ToolSpec};
use criogen_storage::Storage;
use serde_json::{Map, Value, json};

use crate::config::{limits, memory, model, retry_policy};
use crate::{
    CallError, Failure, JsFn, Registered, Result, bad, call_js, failure, lock, parse, strings,
    text, to_json,
};

pub(crate) fn agent_error(error: AgentError) -> Failure {
    failure(&error.code, error.message, error.details)
}

// ---------- results ----------

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

// ---------- tools ----------

/// Calls a JavaScript tool and reads its envelope: `{"ok": value}`,
/// `{"toolError", "retryable"}` (the model sees it), `{"invalidArguments"}`
/// (a Standard Schema's issues, design 0017 §6) or `{"crash"}` (an
/// exception the tool did not handle: `onToolException` decides; when the
/// run fails on it, the TypeScript side keeps the exception as its cause).
async fn call_tool(
    tool: JsFn,
    args: Map<String, Value>,
    ctx: ToolContext,
) -> std::result::Result<Value, ToolError> {
    let request = json!({"args": args, "callId": ctx.call_id, "runId": ctx.run_id}).to_string();
    let envelope = tool.call(request).await.map_err(|err| match err {
        CallError::Call(err) => ToolError::fatal(format!("could not call the tool: {err}")),
        CallError::Rejected(reason) => ToolError::fatal(format!("the tool failed: {reason}")),
    })?;
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

// ---------- hooks (design 0017 §5) ----------

/// A JavaScript hook: one function `({stage, payload}) => Promise<envelope>`;
/// only the stages it defines are called. A throw is `{"crash"}` (the
/// TypeScript side keeps it under `hook:<name>:<stage>`).
struct JsHook {
    name: String,
    function: JsFn,
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

fn js_hook_action(envelope: &Value) -> std::result::Result<HookAction, HookError> {
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

    /// Calls the hook's `stage`. A throw is `Err((text, cause key))`: the
    /// key is unique to that throw, and names where the TypeScript side
    /// keeps what was thrown.
    async fn call(
        &self,
        stage: &str,
        payload: Value,
    ) -> std::result::Result<Value, (String, Option<String>)> {
        let request = json!({"stage": stage, "payload": payload}).to_string();
        let envelope = call_js(self.function.clone(), request)
            .await
            .map_err(|text| (text, None))?;
        match envelope.get("crash").and_then(Value::as_str) {
            Some(crash) => Err((
                crash.to_string(),
                envelope
                    .get("causeKey")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )),
            None => Ok(envelope),
        }
    }

    async fn action(
        &self,
        stage: &str,
        ctx: &HookContext,
    ) -> std::result::Result<HookAction, HookError> {
        if !self.has(stage) {
            return Ok(HookAction::proceed());
        }
        let envelope = self
            .call(stage, js_hook_ctx(ctx))
            .await
            .map_err(|(text, key)| {
                let error = HookError::new(text);
                match key {
                    Some(key) => error.with_cause_key(key),
                    None => error,
                }
            })?;
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
    ) -> BoxFuture<'a, std::result::Result<HookAction, HookError>> {
        Box::pin(self.action("on_start", ctx))
    }

    fn before_model<'a>(
        &'a self,
        ctx: &'a HookContext,
    ) -> BoxFuture<'a, std::result::Result<HookAction, HookError>> {
        Box::pin(self.action("before_model", ctx))
    }

    fn after_model<'a>(
        &'a self,
        ctx: &'a HookContext,
    ) -> BoxFuture<'a, std::result::Result<HookAction, HookError>> {
        Box::pin(self.action("after_model", ctx))
    }

    fn wrap_tool<'a>(
        &'a self,
        mut call: ToolCallRequest,
        next: ToolNext<'a>,
    ) -> BoxFuture<'a, std::result::Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            let call_json = |call: &ToolCallRequest| json!({"callId": call.call_id, "name": call.name, "args": call.args});
            if self.has("before_tool") {
                let envelope = self
                    .call("before_tool", call_json(&call))
                    .await
                    .map_err(|(text, _)| ToolError::crashed(text))?;
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
                .map_err(|(text, _)| ToolError::crashed(text))?;
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

// ---------- agents ----------

/// Agents by handle, so a workflow (built from JavaScript, where only
/// numbers and strings cross) can use one as a node or a tool.
fn agents() -> &'static Mutex<HashMap<u32, Agent>> {
    static AGENTS: OnceLock<Mutex<HashMap<u32, Agent>>> = OnceLock::new();
    AGENTS.get_or_init(Mutex::default)
}

pub(crate) fn agent_by_handle(handle: u32) -> Option<Agent> {
    lock(agents()).get(&handle).cloned()
}

/// Forgets the agent behind `handle` (its JavaScript object is gone).
pub fn forget_agent(handle: u32) {
    lock(agents()).remove(&handle);
}

/// Builds an agent from a TypeScript configuration and registers it; the
/// handle names it to workflows and other agents. `tools` holds one
/// function per entry of `config.tools`, `hooks` one per `config.hooks`.
/// `extend` adds what only one engine has (the Node addon's telemetry).
pub fn build_agent(
    config: &Value,
    tools: Vec<JsFn>,
    storage: &Storage,
    hooks: Vec<JsFn>,
    extend: impl FnOnce(AgentBuilder) -> Result<AgentBuilder>,
) -> Result<(Agent, u32)> {
    let mut builder = Agent::builder_shared(model(&config["model"])?).storage(storage.clone());
    if let Some(instructions) = text(config, "instructions") {
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
            function,
            stages: strings(&spec["stages"]),
        });
    }
    builder = extend(builder)?;
    // Agents as tools of this one (design 0017 §1), by handle.
    for spec in config["agentTools"].as_array().cloned().unwrap_or_default() {
        let handle = spec["handle"].as_u64().unwrap_or_default() as u32;
        let agent = agent_by_handle(handle).ok_or_else(|| bad("an agent tool's agent is gone"))?;
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
    Ok((agent, handle))
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

fn decisions(payload: &Value) -> Result<Vec<(String, Decision)>> {
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

async fn thread_operation(thread: Thread, op: &str, payload: Value) -> Result<String> {
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

/// `agent.run`: payload `{input, state}`; `cancel_id` from
/// [`new_cancel_id`](crate::new_cancel_id).
pub async fn agent_run(agent: Agent, payload: String, cancel_id: u32) -> Result<String> {
    let payload = parse(&payload)?;
    let cancel = Registered::take(cancel_id);
    let result = agent
        .run_with(input(&payload), options(&agent, cancel.token()))
        .await
        .map_err(agent_error)?;
    Ok(result_json(&result).to_string())
}

/// A thread operation: payload `{threadId, actor, …}`.
pub async fn agent_thread(
    agent: Agent,
    op: String,
    payload: String,
    cancel_id: u32,
) -> Result<String> {
    let payload = parse(&payload)?;
    let cancel = Registered::take(cancel_id);
    let thread = thread(&agent, &payload, options(&agent, cancel.token()));
    thread_operation(thread, &op, payload).await
}

/// Starts a streamed thread operation (`send`, `approve`, `reject`,
/// `decide`, `retry`). The run starts at once, as a task: on servers call
/// this inside the Tokio runtime.
pub fn agent_stream(agent: &Agent, op: &str, payload: &str) -> Result<EventStream> {
    let payload = parse(payload)?;
    let thread = thread(agent, &payload, agent.run_options());
    let stream = match op {
        "decide" => thread.stream_decide(decisions(&payload)?),
        "send" => thread.stream(input(&payload)),
        "approve" => thread.stream_approve(),
        "reject" => {
            let message = text(&payload, "message").unwrap_or("rejected").to_string();
            thread.stream_reject(message)
        }
        "retry" => thread.stream_retry(),
        other => return Err(bad(format!("cannot stream `{other}`"))),
    };
    Ok(EventStream {
        cancel: stream.cancel_token(),
        stream: Arc::new(tokio::sync::Mutex::new(Some(stream))),
    })
}

/// A streamed thread operation. Cheap to clone; clones share the stream.
#[derive(Debug, Clone)]
pub struct EventStream {
    stream: Arc<tokio::sync::Mutex<Option<ThreadStream>>>,
    /// Cancels the run even while `next` holds the stream.
    cancel: CancelToken,
}

impl EventStream {
    /// The next event as JSON, or `None` after the last.
    pub async fn next(&self) -> Result<Option<String>> {
        let mut guard = self.stream.lock().await;
        let Some(stream) = guard.as_mut() else {
            return Ok(None);
        };
        Ok(stream.next().await.map(event_json))
    }

    /// Waits for the operation; its reply as JSON.
    pub async fn finish(&self) -> Result<String> {
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
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}
