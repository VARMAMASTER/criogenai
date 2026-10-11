//! The agent: a model ⇄ tools loop compiled into an ordinary workflow
//! (design 0004 §2, §4, §5).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agentbuild_core::{
    BoxFuture, BuildError, END, Node, NodeContext, NodeError, NodeOutput, NodePolicy, Reducer,
    RetryPolicy, RunError, RunErrorKind, RunOptions, RunOutput, Snapshot, State, Update, Workflow,
    WorkflowBuilder,
};
use agentbuild_providers::{
    ContentBlock, FinishReason, Message, MessageDelta, Model, ModelError, ModelRequest, Role,
    ToolIssue, ToolResultError, ToolSpec, ToolStatus, Usage, transcript, validate_tools,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::schema;
use crate::tool::{Tool, ToolContext, ToolOutput};

/// State key holding the transcript (`upsert_by_id` reducer).
pub const MESSAGES: &str = "messages";
/// State key holding the run's bookkeeping (model and tool call counts,
/// usage, stop reason, output).
pub const AGENT: &str = "agent";

const MODEL_NODE: &str = "model";
const TOOLS_NODE: &str = "tools";

/// Bounds on one agent run (0004 §5). Every agent has them; no runaway
/// loops.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// Model calls per run (retries of one call don't count). Default 25.
    pub model_calls: u32,
    /// Tool calls executed per run. Default 200.
    pub tool_calls: u32,
    /// Tool calls taken from one assistant message; the rest are
    /// `skipped`. Default 32.
    pub tool_calls_per_turn: u32,
    /// Tool calls running at the same time. Default 8.
    pub parallel_tool_calls: usize,
    /// Time limit per tool call when the tool sets none. Default 60 s.
    pub tool_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            model_calls: 25,
            tool_calls: 200,
            tool_calls_per_turn: 32,
            parallel_tool_calls: 8,
            tool_timeout: Duration::from_secs(60),
        }
    }
}

impl Limits {
    /// Sets [`model_calls`](Self::model_calls).
    pub fn model_calls(mut self, n: u32) -> Self {
        self.model_calls = n;
        self
    }

    /// Sets [`tool_calls`](Self::tool_calls).
    pub fn tool_calls(mut self, n: u32) -> Self {
        self.tool_calls = n;
        self
    }

    /// Sets [`tool_calls_per_turn`](Self::tool_calls_per_turn).
    pub fn tool_calls_per_turn(mut self, n: u32) -> Self {
        self.tool_calls_per_turn = n;
        self
    }

    /// Sets [`parallel_tool_calls`](Self::parallel_tool_calls).
    pub fn parallel_tool_calls(mut self, n: usize) -> Self {
        self.parallel_tool_calls = n;
        self
    }

    /// Sets [`tool_timeout`](Self::tool_timeout).
    pub fn tool_timeout(mut self, timeout: Duration) -> Self {
        self.tool_timeout = timeout;
        self
    }

    /// The core step limit that fits `model_calls`: each model call and each
    /// tools round is one step.
    pub fn max_steps(&self) -> usize {
        2 * self.model_calls as usize + 4
    }
}

/// Why an agent run ended normally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StopReason {
    /// The model answered without calling tools.
    FinalAnswer,
    /// A `return_direct` tool succeeded; its result is the output.
    ReturnDirect,
    /// The model declined to answer (`finish_reason: content_filter`).
    ContentFilter,
}

/// Run bookkeeping, stored under [`AGENT`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Bookkeeping {
    model_calls: u32,
    tool_calls: u32,
    usage: Usage,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_reason: Option<StopReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<Value>,
}

/// A problem found when building an agent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AgentBuildError {
    /// The tool definitions are invalid.
    #[error("invalid tools: {}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    Tools(Vec<ToolIssue>),
    /// A limit is zero.
    #[error("limit `{0}` must be at least 1")]
    ZeroLimit(&'static str),
    /// The compiled workflow is invalid (a bug in AgentBuild).
    #[error("internal: {0}")]
    Workflow(BuildError),
}

/// Configures an [`Agent`].
pub struct AgentBuilder {
    model: Arc<dyn Model>,
    instructions: Option<String>,
    tools: Vec<Arc<dyn Tool>>,
    limits: Limits,
    model_retry: RetryPolicy,
}

impl std::fmt::Debug for AgentBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentBuilder")
            .field("model", &self.model.model())
            .field(
                "tools",
                &self
                    .tools
                    .iter()
                    .map(|t| &t.spec().name)
                    .collect::<Vec<_>>(),
            )
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl AgentBuilder {
    /// System instructions, sent first in every model request (not stored
    /// in the transcript).
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Adds a tool.
    pub fn tool(mut self, tool: impl Tool) -> Self {
        self.tools.push(Arc::new(tool));
        self
    }

    /// Adds a shared tool.
    pub fn shared_tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Sets the limits.
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Retries for transient model errors (rate limits, overload, network).
    /// Default: 3 attempts, exponential backoff from 0.5 s with jitter.
    pub fn model_retry(mut self, retry: RetryPolicy) -> Self {
        self.model_retry = retry;
        self
    }

    /// Validates the tools and limits and compiles the agent's workflow.
    pub fn build(self) -> Result<Agent, AgentBuildError> {
        let specs: Vec<ToolSpec> = self.tools.iter().map(|t| t.spec().clone()).collect();
        validate_tools(&specs).map_err(AgentBuildError::Tools)?;
        let limits = self.limits;
        for (name, value) in [
            ("model_calls", limits.model_calls as usize),
            ("tool_calls", limits.tool_calls as usize),
            ("tool_calls_per_turn", limits.tool_calls_per_turn as usize),
            ("parallel_tool_calls", limits.parallel_tool_calls),
        ] {
            if value == 0 {
                return Err(AgentBuildError::ZeroLimit(name));
            }
        }

        let tools: HashMap<String, Arc<dyn Tool>> = self
            .tools
            .iter()
            .map(|t| (t.spec().name.clone(), t.clone()))
            .collect();
        let model_node = ModelNode(Arc::new(ModelInner {
            model: self.model,
            instructions: self.instructions,
            specs,
            limits: limits.clone(),
        }));
        let tools_node = ToolsNode(Arc::new(ToolsInner {
            tools,
            limits: limits.clone(),
        }));

        let mut builder = WorkflowBuilder::new();
        builder
            .add_node(MODEL_NODE, model_node)
            .add_node(TOOLS_NODE, tools_node)
            .set_entry(MODEL_NODE)
            .may_goto(MODEL_NODE, [TOOLS_NODE, END])
            .may_goto(TOOLS_NODE, [MODEL_NODE, END])
            .reducer(MESSAGES, Reducer::UpsertById)
            .policy(MODEL_NODE, NodePolicy::new().retry(self.model_retry));
        let workflow = builder.build().map_err(AgentBuildError::Workflow)?;
        Ok(Agent { workflow, limits })
    }
}

/// A model with tools, run as a loop: model → tools → model … until the
/// model answers without calling a tool (design 0004).
///
/// It compiles into an ordinary [`Workflow`] with two nodes, `model` and
/// `tools`, so streaming, cancellation, deadlines, snapshots and
/// [`retry`](Self::retry) work exactly as for any workflow.
#[derive(Debug, Clone)]
pub struct Agent {
    workflow: Workflow,
    limits: Limits,
}

/// What an agent run starts from.
#[derive(Debug, Clone)]
pub struct AgentInput(Vec<Message>);

impl From<&str> for AgentInput {
    fn from(text: &str) -> Self {
        Self(vec![Message::user(text)])
    }
}

impl From<String> for AgentInput {
    fn from(text: String) -> Self {
        Self(vec![Message::user(text)])
    }
}

impl From<Message> for AgentInput {
    fn from(message: Message) -> Self {
        Self(vec![message])
    }
}

impl From<Vec<Message>> for AgentInput {
    fn from(messages: Vec<Message>) -> Self {
        Self(messages)
    }
}

impl Agent {
    /// Starts configuring an agent around `model`.
    pub fn builder(model: impl Model) -> AgentBuilder {
        Self::builder_shared(Arc::new(model))
    }

    /// [`builder`](Self::builder) with a shared model.
    pub fn builder_shared(model: Arc<dyn Model>) -> AgentBuilder {
        AgentBuilder {
            model,
            instructions: None,
            tools: Vec::new(),
            limits: Limits::default(),
            model_retry: RetryPolicy::new(),
        }
    }

    /// The compiled workflow (nodes `model` and `tools`).
    pub fn workflow(&self) -> &Workflow {
        &self.workflow
    }

    /// The limits.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// The initial state for a run starting from `input`.
    pub fn initial_state(input: impl Into<AgentInput>) -> State {
        let messages = input.into().0;
        State::new().with(
            MESSAGES,
            serde_json::to_value(messages).expect("messages serialize"),
        )
    }

    /// Run options with the step limit derived from [`Limits`].
    pub fn run_options(&self) -> RunOptions {
        RunOptions::default().max_steps(self.limits.max_steps())
    }

    /// Runs the agent on `input` (text, a message or a transcript).
    pub async fn run(&self, input: impl Into<AgentInput>) -> Result<AgentResult, AgentError> {
        self.run_with(input, self.run_options()).await
    }

    /// [`run`](Self::run) with options (events, deadline, cancellation).
    /// `max_steps` is raised to fit the model-call limit if needed.
    pub async fn run_with(
        &self,
        input: impl Into<AgentInput>,
        mut options: RunOptions,
    ) -> Result<AgentResult, AgentError> {
        options.max_steps = options.max_steps.max(self.limits.max_steps());
        let output = self
            .workflow
            .run_with(Self::initial_state(input), options)
            .await
            .map_err(AgentError::from_run)?;
        AgentResult::from_output(output)
    }

    /// Continues a failed run from its snapshot. Tool calls that finished
    /// before the failure are not run again.
    pub async fn retry(&self, snapshot: &Snapshot) -> Result<AgentResult, AgentError> {
        let output = self
            .workflow
            .retry_with(snapshot, self.run_options())
            .await
            .map_err(AgentError::from_run)?;
        AgentResult::from_output(output)
    }
}

/// The result of an agent run that did not fail.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct AgentResult {
    /// The full transcript.
    pub messages: Vec<Message>,
    /// The final text, or the `return_direct` tool's result.
    pub output: Option<Value>,
    /// Why the run ended.
    pub stop_reason: Option<StopReason>,
    /// Tokens across all model calls.
    pub usage: Usage,
    /// Model calls made.
    pub model_calls: u32,
    /// Tool calls executed.
    pub tool_calls: u32,
    /// The underlying workflow result (status, steps, run id).
    pub run: RunOutput,
}

impl AgentResult {
    /// Reads an agent's result from a workflow run's output.
    pub fn from_output(run: RunOutput) -> Result<Self, AgentError> {
        let messages = read_messages(&run.state).map_err(AgentError::invalid_state)?;
        let book = read_book(&run.state).map_err(AgentError::invalid_state)?;
        Ok(Self {
            messages,
            output: book.output,
            stop_reason: book.stop_reason,
            usage: book.usage,
            model_calls: book.model_calls,
            tool_calls: book.tool_calls,
            run,
        })
    }

    /// The output as text (`None` if there is no output or it is not text).
    pub fn text(&self) -> Option<&str> {
        self.output.as_ref().and_then(Value::as_str)
    }
}

/// A failed agent run.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{code}: {message}")]
#[non_exhaustive]
pub struct AgentError {
    /// Stable code: `model_failed`, `tool_failed`, `limit_exceeded`, or a
    /// workflow code (`cancelled`, `deadline_exceeded`, …).
    pub code: String,
    /// What went wrong.
    pub message: String,
    /// Structured context (e.g. the classified model error).
    pub details: Option<Value>,
    /// The workflow error, with the snapshot for [`Agent::retry`]. Boxed:
    /// it carries the run's state.
    pub run_error: Option<Box<RunError>>,
}

impl AgentError {
    fn from_run(error: RunError) -> Self {
        let (code, message, details) = match &error.kind {
            RunErrorKind::NodeFailed { source, .. }
                if matches!(
                    source.kind.as_deref(),
                    Some("model_failed" | "tool_failed" | "limit_exceeded" | "invalid_state")
                ) =>
            {
                (
                    source.kind.clone().unwrap_or_default(),
                    source.message.clone(),
                    source.details.clone(),
                )
            }
            other => (other.code().to_string(), other.to_string(), None),
        };
        Self {
            code,
            message,
            details,
            run_error: Some(Box::new(error)),
        }
    }

    fn invalid_state(error: NodeError) -> Self {
        Self {
            code: "invalid_state".into(),
            message: error.message,
            details: None,
            run_error: None,
        }
    }

    /// The snapshot to [`Agent::retry`] from, if the run got that far.
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.run_error.as_deref().and_then(RunError::snapshot)
    }
}

// ---------- shared helpers ----------

fn read_messages(state: &State) -> Result<Vec<Message>, NodeError> {
    match state.get(MESSAGES) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(value) => serde_json::from_value(value.clone()).map_err(|err| {
            NodeError::new(format!("`{MESSAGES}` is not a list of messages: {err}"))
                .with_kind("invalid_state")
        }),
    }
}

fn read_book(state: &State) -> Result<Bookkeeping, NodeError> {
    match state.get(AGENT) {
        None | Some(Value::Null) => Ok(Bookkeeping::default()),
        Some(value) => serde_json::from_value(value.clone()).map_err(|err| {
            NodeError::new(format!("`{AGENT}` has an unexpected shape: {err}"))
                .with_kind("invalid_state")
        }),
    }
}

fn to_json<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("agent data serializes")
}

fn limit_error(limit: &str, value: u32) -> NodeError {
    NodeError::new(format!("the agent reached its {limit} limit ({value})"))
        .with_kind("limit_exceeded")
        .with_details(json!({"limit": limit, "value": value}))
}

fn model_error(error: ModelError) -> NodeError {
    let transient = error.kind.is_transient();
    let node_error = NodeError::new(error.to_string())
        .with_kind("model_failed")
        .with_details(json!({"model_error": error}));
    if transient {
        node_error.retryable()
    } else {
        node_error
    }
}

// ---------- model node ----------

struct ModelInner {
    model: Arc<dyn Model>,
    instructions: Option<String>,
    specs: Vec<ToolSpec>,
    limits: Limits,
}

struct ModelNode(Arc<ModelInner>);

impl Node for ModelNode {
    fn run(
        &self,
        ctx: NodeContext,
        state: Arc<State>,
    ) -> BoxFuture<'static, Result<NodeOutput, NodeError>> {
        Box::pin(model_step(self.0.clone(), ctx, state))
    }
}

/// One model turn (0004 §4.1): repair, check limits, call, route.
async fn model_step(
    inner: Arc<ModelInner>,
    ctx: NodeContext,
    state: Arc<State>,
) -> Result<NodeOutput, NodeError> {
    let messages = read_messages(&state)?;
    let mut book = read_book(&state)?;
    if book.model_calls >= inner.limits.model_calls {
        return Err(limit_error("model_calls", inner.limits.model_calls));
    }

    let repaired = transcript::repair(messages);
    let events = ctx.events().clone();
    if !repaired.changes.is_empty() {
        events.emit("transcript_repaired", json!({"changes": repaired.changes}));
    }
    let mut request_messages = Vec::with_capacity(repaired.messages.len() + 1);
    if let Some(instructions) = &inner.instructions {
        request_messages.push(Message::system(instructions.clone()).with_id("msg_instructions"));
    }
    request_messages.extend(repaired.messages.iter().cloned());
    let request = ModelRequest::new(request_messages).with_tools(inner.specs.clone());

    let delta_events = events.clone();
    let mut on_delta = move |delta: MessageDelta| {
        if delta_events.is_enabled() {
            delta_events.emit("message_delta", to_json(&delta));
        }
    };
    let response = match inner.model.stream(&request, &mut on_delta).await {
        Ok(response) => response,
        Err(error) => {
            if error.emitted_deltas {
                // A retry starts a new message; UIs drop the partial one.
                events.emit("message_aborted", json!({"reason": error.kind}));
            }
            return Err(model_error(error));
        }
    };
    for warning in &response.warnings {
        events.emit("warning", to_json(warning));
    }
    let message = response.message;
    events.emit(
        "message_completed",
        json!({
            "message_id": message.id,
            "finish_reason": response.finish_reason,
            "usage": response.usage,
        }),
    );

    book.model_calls += 1;
    book.usage = book.usage.add(&response.usage);
    // Routing after the model (0004 §4.2).
    let next = if response.finish_reason == FinishReason::ContentFilter {
        book.stop_reason = Some(StopReason::ContentFilter);
        book.output = Some(Value::String(message.text()));
        END
    } else if message.call_ids().next().is_some() {
        TOOLS_NODE
    } else {
        book.stop_reason = Some(StopReason::FinalAnswer);
        book.output = Some(Value::String(message.text()));
        END
    };

    let messages_update = if repaired.changes.is_empty() {
        json!([message])
    } else {
        // Repairs can insert results before later messages, which an
        // upsert (append) cannot express: replace the list (repairs are rare).
        let mut all = repaired.messages;
        all.push(message);
        json!({"$replace": all})
    };
    Ok(NodeOutput::goto(
        Update::new()
            .set(MESSAGES, messages_update)
            .set(AGENT, to_json(&book)),
        [next],
    ))
}

// ---------- tools node ----------

struct ToolsInner {
    tools: HashMap<String, Arc<dyn Tool>>,
    limits: Limits,
}

struct ToolsNode(Arc<ToolsInner>);

impl Node for ToolsNode {
    fn run(
        &self,
        ctx: NodeContext,
        state: Arc<State>,
    ) -> BoxFuture<'static, Result<NodeOutput, NodeError>> {
        Box::pin(tools_step(self.0.clone(), ctx, state))
    }
}

/// A call that will run.
struct Pending {
    index: usize,
    call_id: String,
    name: String,
    args: Map<String, Value>,
    tool: Arc<dyn Tool>,
}

/// A finished call.
struct Finished {
    index: usize,
    message: Message,
    /// The tool's output, when it succeeded with `return_direct`.
    direct: Option<Value>,
}

fn result_id(call_id: &str) -> String {
    format!("msg_result_{call_id}")
}

fn error_result(call_id: &str, tool: &str, code: &str, message: String) -> Message {
    Message::tool_error(call_id, tool, ToolResultError::new(code, message))
        .with_id(result_id(call_id))
}

fn skipped_result(call_id: &str, tool: &str, reason: String) -> Message {
    Message::tool_result(
        call_id,
        tool,
        ToolStatus::Skipped,
        vec![ContentBlock::text(format!("Skipped: {reason}"))],
    )
    .with_id(result_id(call_id))
}

/// One tools round (0004 §4.3): answer every call of the last assistant
/// message, in call order.
async fn tools_step(
    inner: Arc<ToolsInner>,
    ctx: NodeContext,
    state: Arc<State>,
) -> Result<NodeOutput, NodeError> {
    let messages = read_messages(&state)?;
    let mut book = read_book(&state)?;
    let position = messages
        .iter()
        .rposition(|m| m.role == Role::Assistant)
        .ok_or_else(|| {
            NodeError::new("no assistant message with tool calls").with_kind("invalid_state")
        })?;
    let assistant = &messages[position];
    let answered: HashSet<&str> = messages[position + 1..]
        .iter()
        .filter_map(|m| m.call_id.as_deref())
        .collect();

    let memo = ctx.memo().clone();
    let limits = &inner.limits;
    let mut results: Vec<Option<Message>> = Vec::new();
    let mut pending = Vec::new();
    let mut executed = 0u32;
    let mut available: Vec<&str> = inner.tools.keys().map(String::as_str).collect();
    available.sort_unstable();

    for block in &assistant.content {
        let index = results.len();
        let (call_id, name, args) = match block {
            ContentBlock::ToolCall { id, name, args, .. } => (id, name, Some(args)),
            ContentBlock::InvalidToolCall {
                id, name, error, ..
            } => {
                if !answered.contains(id.as_str()) {
                    let tool = name.as_deref().unwrap_or("unknown");
                    results.push(Some(error_result(
                        id,
                        tool,
                        "invalid_arguments",
                        format!("Arguments could not be parsed: {error}"),
                    )));
                } else {
                    results.push(None);
                }
                continue;
            }
            _ => continue,
        };
        let args = args.expect("tool call");
        if answered.contains(call_id.as_str()) {
            results.push(None);
            continue;
        }
        // Saved by an earlier attempt of this step: never run twice.
        if let Some(saved) = memo.get(&format!("call:{call_id}"))
            && let Ok(message) = serde_json::from_value::<Message>(saved)
        {
            executed += 1;
            results.push(Some(message));
            continue;
        }
        let call_number = index as u32;
        let result = if call_number >= limits.tool_calls_per_turn {
            Some(skipped_result(
                call_id,
                name,
                format!(
                    "at most {} tool calls are run per turn",
                    limits.tool_calls_per_turn
                ),
            ))
        } else if book.tool_calls + executed + pending.len() as u32 >= limits.tool_calls {
            Some(skipped_result(
                call_id,
                name,
                format!(
                    "the run's tool call limit ({}) was reached",
                    limits.tool_calls
                ),
            ))
        } else if let Some(tool) = inner.tools.get(name) {
            let problems =
                schema::validate(&tool.spec().input_schema, &Value::Object(args.clone()));
            if problems.is_empty() {
                pending.push(Pending {
                    index,
                    call_id: call_id.clone(),
                    name: name.clone(),
                    args: args.clone(),
                    tool: tool.clone(),
                });
                None
            } else {
                Some(error_result(
                    call_id,
                    name,
                    "invalid_arguments",
                    format!("Invalid arguments: {}", problems.join("; ")),
                ))
            }
        } else {
            Some(error_result(
                call_id,
                name,
                "unknown_tool",
                format!(
                    "Unknown tool `{name}`. Available tools: {}",
                    if available.is_empty() {
                        "none".to_string()
                    } else {
                        available.join(", ")
                    }
                ),
            ))
        };
        results.push(result);
    }

    // Run the pending calls concurrently, bounded by the semaphore.
    let semaphore = Arc::new(Semaphore::new(limits.parallel_tool_calls));
    let mut tasks = JoinSet::new();
    let mut task_calls = HashMap::new();
    for call in pending {
        let semaphore = semaphore.clone();
        let memo = memo.clone();
        let events = ctx.events().clone();
        let tool_ctx = ToolContext {
            call_id: call.call_id.clone(),
            run_id: ctx.run_id().to_string(),
            cancel: ctx.cancel_token().child(),
            events: events.clone(),
        };
        let timeout = call
            .tool
            .spec()
            .timeout_ms
            .map_or(limits.tool_timeout, Duration::from_millis);
        let return_direct = call.tool.spec().return_direct;
        let (call_id, name) = (call.call_id.clone(), call.name.clone());
        let handle = tasks.spawn(async move {
            let _permit = semaphore.acquire_owned().await.expect("semaphore open");
            events.emit(
                "tool_call_started",
                json!({"call_id": call.call_id, "tool": call.name}),
            );
            let started = Instant::now();
            let outcome = tokio::time::timeout(timeout, call.tool.call(call.args, tool_ctx)).await;
            let (message, direct) = match outcome {
                Ok(Ok(output)) => {
                    let direct = return_direct.then(|| output.to_value());
                    let message = Message::tool_result(
                        &call.call_id,
                        &call.name,
                        ToolStatus::Success,
                        ToolOutput::into_content(output),
                    )
                    .with_id(result_id(&call.call_id));
                    (message, direct)
                }
                Ok(Err(error)) if error.fatal => return Err((call.call_id, call.name, error)),
                Ok(Err(error)) => {
                    let mut message =
                        error_result(&call.call_id, &call.name, "tool_error", error.message);
                    if let Some(e) = message.error.as_mut() {
                        e.retryable = error.retryable;
                    }
                    (message, None)
                }
                Err(_) => {
                    let mut message = error_result(
                        &call.call_id,
                        &call.name,
                        "timeout",
                        format!("The tool did not finish within {} ms", timeout.as_millis()),
                    );
                    if let Some(e) = message.error.as_mut() {
                        e.retryable = true;
                    }
                    (message, None)
                }
            };
            let mut message = message;
            message.metadata.duration_ms = Some(started.elapsed().as_millis() as u64);
            // Saved at once, so a failure elsewhere in this step never makes
            // this call run again (0005 §5.1).
            memo.put(format!("call:{}", call.call_id), to_json(&message));
            events.emit(
                "tool_call_finished",
                json!({
                    "call_id": call.call_id,
                    "tool": call.name,
                    "status": message.status,
                    "duration_ms": message.metadata.duration_ms,
                }),
            );
            Ok(Finished {
                index: call.index,
                message,
                direct,
            })
        });
        task_calls.insert(handle.id(), (call_id, name));
    }

    let mut direct_outputs: Vec<(usize, Value)> = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        let finished = match joined {
            Ok(Ok(finished)) => finished,
            Ok(Err((call_id, tool, error))) => {
                return Err(tool_failed(&memo, &call_id, &tool, &error.message));
            }
            Err(join_error) => {
                let (call_id, tool) = task_calls
                    .get(&join_error.id())
                    .cloned()
                    .unwrap_or_default();
                let message = if join_error.is_panic() {
                    "the tool panicked".to_string()
                } else {
                    "the tool task was cancelled".to_string()
                };
                return Err(tool_failed(&memo, &call_id, &tool, &message));
            }
        };
        executed += 1;
        if let Some(direct) = finished.direct {
            direct_outputs.push((finished.index, direct));
        }
        results[finished.index] = Some(finished.message);
    }

    let results: Vec<Message> = results.into_iter().flatten().collect();
    book.tool_calls += executed;
    let next = if direct_outputs.is_empty() {
        MODEL_NODE
    } else {
        direct_outputs.sort_by_key(|(index, _)| *index);
        let mut values: Vec<Value> = direct_outputs.into_iter().map(|(_, v)| v).collect();
        book.stop_reason = Some(StopReason::ReturnDirect);
        book.output = Some(if values.len() == 1 {
            values.remove(0)
        } else {
            Value::Array(values)
        });
        END
    };
    Ok(NodeOutput::goto(
        Update::new()
            .set(MESSAGES, json!(results))
            .set(AGENT, to_json(&book)),
        [next],
    ))
}

/// A developer error in a tool: the step fails. Calls that already finished
/// are listed, and their results are saved in the memo (0003 E2).
fn tool_failed(
    memo: &agentbuild_core::TaskMemo,
    call_id: &str,
    tool: &str,
    message: &str,
) -> NodeError {
    let completed: Vec<String> = memo
        .keys()
        .into_iter()
        .filter_map(|key| key.strip_prefix("call:").map(str::to_string))
        .collect();
    NodeError::new(format!("tool `{tool}` failed: {message}"))
        .with_kind("tool_failed")
        .with_details(json!({"tool": tool, "call_id": call_id, "completed_calls": completed}))
}
