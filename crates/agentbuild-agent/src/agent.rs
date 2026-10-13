//! The agent: a model ⇄ tools loop compiled into an ordinary workflow
//! (design 0004 §2, §4, §5).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agentbuild_core::{
    BoxFuture, BuildError, END, Node, NodeContext, NodeError, NodeOutput, Reducer, RetryPolicy,
    RunError, RunErrorKind, RunOptions, RunOutput, Snapshot, State, Update, Workflow,
    WorkflowBuilder,
};
use agentbuild_providers::{
    ContentBlock, FinishReason, Message, MessageDelta, Model, ModelError, ModelErrorKind,
    ModelRequest, OutputSpec, OutputStrategy, Role, ToolIssue, ToolResultError, ToolSpec,
    ToolStatus, Usage, transcript, validate_tools,
};
use agentbuild_storage::Storage;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::approval::{APPROVAL_INTERRUPT, Approval, ApprovalRequest, Decision, check_decisions};
use crate::hooks::{Hook, HookContext, ModelNext, Outcome, ToolCallRequest, ToolNext};
use crate::output::{FINAL_OUTPUT_TOOL, OutputConfig};
use crate::resilience::{CallEnv, ModelChain};
use crate::schema;
use crate::tool::{Tool, ToolContext};

/// State key holding the transcript (`upsert_by_id` reducer).
pub const MESSAGES: &str = "messages";
/// State key holding the run's bookkeeping (model and tool call counts,
/// usage, stop reason, output).
pub const AGENT: &str = "agent";
/// State key holding hook state, one entry per hook name.
pub const HOOKS: &str = "hooks";

const MODEL_NODE: &str = "model";
const TOOLS_NODE: &str = "tools";
const REVIEW_NODE: &str = "review";

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
    /// Invalid structured-output answers tolerated before the run fails
    /// with `output_invalid`. Default 2.
    pub output_retries: u32,
    /// Input plus output tokens across the run's model calls. Checked
    /// before each model call. Default: none.
    pub tokens: Option<u64>,
    /// What happens when a limit is reached. Default [`OnLimit::Error`].
    pub on_limit: OnLimit,
    /// How deep agents may call agents ([`Agent::as_tool`]). Default 8.
    pub nesting: u32,
    /// Tool results longer than this (in characters) are cut, unless the
    /// tool sets its own `max_result_chars`. Default 100,000.
    pub max_result_chars: usize,
}

/// What happens when the model-call, token or tool-call budget runs out
/// (0004 §5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OnLimit {
    /// Fail with `limit_exceeded`.
    #[default]
    Error,
    /// End normally with stop reason `limit`; the output is the last
    /// assistant text, and the transcript stays valid.
    End,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            model_calls: 25,
            tool_calls: 200,
            tool_calls_per_turn: 32,
            parallel_tool_calls: 8,
            tool_timeout: Duration::from_secs(60),
            output_retries: 2,
            tokens: None,
            on_limit: OnLimit::Error,
            nesting: 8,
            max_result_chars: 100_000,
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

    /// Sets [`tokens`](Self::tokens).
    pub fn tokens(mut self, n: u64) -> Self {
        self.tokens = Some(n);
        self
    }

    /// Sets [`max_result_chars`](Self::max_result_chars).
    pub fn max_result_chars(mut self, n: usize) -> Self {
        self.max_result_chars = n;
        self
    }

    /// Sets [`nesting`](Self::nesting).
    pub fn nesting(mut self, depth: u32) -> Self {
        self.nesting = depth;
        self
    }

    /// Sets [`on_limit`](Self::on_limit).
    pub fn on_limit(mut self, on_limit: OnLimit) -> Self {
        self.on_limit = on_limit;
        self
    }

    /// Sets [`output_retries`](Self::output_retries).
    pub fn output_retries(mut self, n: u32) -> Self {
        self.output_retries = n;
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
        2 * self.model_calls as usize + 2 * self.output_retries as usize + 4
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
    /// Structured output was accepted; it is the output.
    Output,
    /// A hook ended the run.
    HookEnd,
    /// A limit was reached with [`OnLimit::End`]; see
    /// [`AgentResult::limit`].
    Limit,
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
    /// Reviewer decisions for the calls of the last assistant message,
    /// consumed by the next tools step.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    decisions: BTreeMap<String, Decision>,
    /// Invalid structured-output answers so far.
    #[serde(skip_serializing_if = "is_zero")]
    output_retries: u32,
    /// A call was skipped because the run's tool-call budget ran out; the
    /// next model call applies `on_limit`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    tool_budget_spent: bool,
    /// The limit that ended the run (`OnLimit::End`).
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<String>,
    /// Agents above this one (agents used as tools).
    #[serde(skip_serializing_if = "is_zero")]
    depth: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
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
    /// The output schema is not a JSON object.
    #[error("the output schema must be a JSON Schema object")]
    OutputSchema,
    /// Two hooks share a name.
    #[error("hook `{0}` is added more than once")]
    DuplicateHook(String),
    /// The approval settings are invalid.
    #[error("invalid approval settings: {0}")]
    Approval(String),
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
    approval: Option<Approval>,
    output: Option<(Value, Option<OutputStrategy>)>,
    hooks: Vec<Arc<dyn Hook>>,
    fallbacks: Vec<Arc<dyn Model>>,
    fallback_on: Vec<ModelErrorKind>,
    max_retry_after: Duration,
    tool_retry: RetryPolicy,
    app: Option<Arc<dyn std::any::Any + Send + Sync>>,
    storage: Option<Storage>,
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
    /// Default: 3 attempts, exponential backoff from 0.5 s with jitter. A
    /// provider's `Retry-After` replaces the backoff; requests that never
    /// reached the provider are retried at once.
    pub fn model_retry(mut self, retry: RetryPolicy) -> Self {
        self.model_retry = retry;
        self
    }

    /// Adds a fallback model, tried in order when the previous one fails
    /// with a transient error (after its retries; at once for rate limits
    /// and overload) or a kind listed in [`fallback_on`](Self::fallback_on).
    pub fn fallback(self, model: impl Model) -> Self {
        self.fallback_shared(Arc::new(model))
    }

    /// [`fallback`](Self::fallback) with a shared model.
    pub fn fallback_shared(mut self, model: Arc<dyn Model>) -> Self {
        self.fallbacks.push(model);
        self
    }

    /// Non-transient error kinds that also move to the next model, e.g.
    /// `context_overflow` (a larger model) or `content_filter` (another
    /// vendor).
    pub fn fallback_on(mut self, kinds: impl IntoIterator<Item = ModelErrorKind>) -> Self {
        self.fallback_on = kinds.into_iter().collect();
        self
    }

    /// Retries for tools annotated `read_only` or `idempotent` after a
    /// retryable [`ToolError`](crate::ToolError) or a timeout. Other tools
    /// are never retried (retrying `send_email` is dangerous). Default: 3
    /// attempts, backoff from 0.5 s with jitter.
    pub fn tool_retry(mut self, retry: RetryPolicy) -> Self {
        self.tool_retry = retry;
        self
    }

    /// App services for tools (a database client, settings), read with
    /// [`ToolContext::context`](crate::ToolContext::context).
    pub fn context<T: std::any::Any + Send + Sync>(mut self, services: T) -> Self {
        self.app = Some(Arc::new(services));
        self
    }

    /// The longest `Retry-After` the agent waits; longer ones fail (or fall
    /// back) at once instead of hanging. Default 60 s.
    pub fn max_retry_after(mut self, wait: Duration) -> Self {
        self.max_retry_after = wait;
        self
    }

    /// The run must end with JSON matching `schema` (design 0004 §6). The
    /// strategy follows the model's capabilities: its native JSON mode when
    /// declared, otherwise a reserved `final_output` tool. Invalid answers
    /// are sent back with the validation errors, up to
    /// [`Limits::output_retries`] times.
    pub fn output(mut self, schema: Value) -> Self {
        self.output = Some((schema, None));
        self
    }

    /// [`output`](Self::output) with a fixed strategy.
    pub fn output_with(mut self, schema: Value, strategy: OutputStrategy) -> Self {
        self.output = Some((schema, Some(strategy)));
        self
    }

    /// Adds a hook (design 0004 §3). Order matters: see [`Hook`].
    pub fn hook(mut self, hook: impl Hook) -> Self {
        self.hooks.push(Arc::new(hook));
        self
    }

    /// Requires a person to approve some tool calls before they run
    /// (design 0004 §7). The run suspends with the requests; continue it
    /// with [`Agent::resume`].
    pub fn approval(mut self, approval: Approval) -> Self {
        self.approval = Some(approval);
        self
    }

    /// Where [`Agent::thread`] keeps conversations (design 0008). Default:
    /// a new in-memory store, lost when the process exits.
    pub fn storage(mut self, storage: Storage) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Validates the tools and limits and compiles the agent's workflow.
    pub fn build(self) -> Result<Agent, AgentBuildError> {
        let specs: Vec<ToolSpec> = self.tools.iter().map(|t| t.spec().clone()).collect();
        validate_tools(&specs).map_err(AgentBuildError::Tools)?;
        if let Some(approval) = &self.approval {
            let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
            approval.check(&names).map_err(AgentBuildError::Approval)?;
        }
        let per_tool: HashMap<String, Arc<Semaphore>> = specs
            .iter()
            .filter_map(|s| {
                let n = s.max_concurrency?;
                Some((s.name.clone(), Arc::new(Semaphore::new(n as usize))))
            })
            .collect();
        let schemas: HashMap<String, Value> = specs
            .iter()
            .map(|s| (s.name.clone(), s.input_schema.clone()))
            .collect();
        let approval = self.approval.map(Arc::new);
        let mut hook_names = HashSet::new();
        for hook in &self.hooks {
            if !hook_names.insert(hook.name().to_string()) {
                return Err(AgentBuildError::DuplicateHook(hook.name().to_string()));
            }
        }
        let hooks: Arc<[Arc<dyn Hook>]> = self.hooks.into();
        let output = match self.output {
            None => None,
            Some((schema, _)) if !schema.is_object() => return Err(AgentBuildError::OutputSchema),
            Some((schema, strategy)) => {
                let strategy = strategy.unwrap_or(
                    if self.model.capabilities().native_structured_output == Some(true) {
                        OutputStrategy::Native
                    } else {
                        OutputStrategy::Tool
                    },
                );
                Some(Arc::new(OutputConfig::new(schema, strategy)))
            }
        };
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
        let mut models = vec![self.model];
        models.extend(self.fallbacks);
        let model_node = ModelNode(Arc::new(ModelInner {
            chain: ModelChain {
                models,
                retry: self.model_retry,
                fallback_on: self.fallback_on,
                max_retry_after: self.max_retry_after,
            },
            instructions: self.instructions,
            specs,
            limits: limits.clone(),
            approval: approval.clone(),
            output: output.clone(),
            hooks: hooks.clone(),
        }));
        let tools_node = ToolsNode(Arc::new(ToolsInner {
            tools,
            limits: limits.clone(),
            output,
            hooks: hooks.clone(),
            per_tool,
            retry: self.tool_retry,
            app: self.app,
        }));

        let mut builder = WorkflowBuilder::new();
        builder
            .add_node(MODEL_NODE, model_node)
            .add_node(TOOLS_NODE, tools_node)
            .set_entry(MODEL_NODE)
            .may_goto(TOOLS_NODE, [MODEL_NODE, END])
            .reducer(MESSAGES, Reducer::UpsertById);
        // Each loop is model + tools, plus review when approval is on.
        let max_steps = match &approval {
            Some(approval) => {
                builder
                    .add_node(
                        REVIEW_NODE,
                        ReviewNode(Arc::new(ReviewInner {
                            approval: approval.clone(),
                            schemas: schemas.clone(),
                        })),
                    )
                    .may_goto(MODEL_NODE, [TOOLS_NODE, REVIEW_NODE, MODEL_NODE, END])
                    .may_goto(REVIEW_NODE, [TOOLS_NODE]);
                3 * limits.model_calls as usize + 2 * limits.output_retries as usize + 4
            }
            None => {
                builder.may_goto(MODEL_NODE, [TOOLS_NODE, MODEL_NODE, END]);
                limits.max_steps()
            }
        };
        let workflow = builder.build().map_err(AgentBuildError::Workflow)?;
        Ok(Agent {
            workflow,
            limits,
            max_steps,
            schemas,
            hooks,
            storage: self.storage.unwrap_or_default(),
        })
    }
}

/// A model with tools, run as a loop: model → tools → model … until the
/// model answers without calling a tool (design 0004).
///
/// It compiles into an ordinary [`Workflow`] with two nodes, `model` and
/// `tools`, so streaming, cancellation, deadlines, snapshots and
/// [`retry`](Self::retry) work exactly as for any workflow.
#[derive(Clone)]
pub struct Agent {
    workflow: Workflow,
    limits: Limits,
    max_steps: usize,
    /// Tool argument schemas, to check edited calls on resume.
    schemas: HashMap<String, Value>,
    hooks: Arc<[Arc<dyn Hook>]>,
    storage: Storage,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("workflow", &self.workflow)
            .field("limits", &self.limits)
            .field(
                "hooks",
                &self.hooks.iter().map(|h| h.name()).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
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
            approval: None,
            output: None,
            hooks: Vec::new(),
            fallbacks: Vec::new(),
            fallback_on: Vec::new(),
            max_retry_after: Duration::from_secs(60),
            tool_retry: RetryPolicy::new(),
            app: None,
            storage: None,
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

    /// Where [`thread`](Self::thread)s are kept.
    pub fn storage(&self) -> &Storage {
        &self.storage
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
        RunOptions::default().max_steps(self.max_steps)
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
        options: RunOptions,
    ) -> Result<AgentResult, AgentError> {
        self.run_at_depth(input, 0, options).await
    }

    /// A run `depth` levels below a top-level agent (agents as tools).
    pub(crate) async fn run_at_depth(
        &self,
        input: impl Into<AgentInput>,
        depth: u32,
        options: RunOptions,
    ) -> Result<AgentResult, AgentError> {
        if depth > self.limits.nesting {
            return Err(AgentError {
                code: "limit_exceeded".into(),
                message: format!(
                    "agents are nested {depth} deep; the limit is {}",
                    self.limits.nesting
                ),
                details: Some(json!({"limit": "nesting", "value": self.limits.nesting})),
                run_error: None,
            });
        }
        let mut state = Self::initial_state(input);
        if depth > 0 {
            let book = Bookkeeping {
                depth,
                ..Bookkeeping::default()
            };
            state.insert(AGENT, to_json(&book));
        }
        self.run_state(state, options).await
    }

    /// Runs from a prepared state (a new run, e.g. a thread's next turn).
    pub(crate) async fn run_state(
        &self,
        mut state: State,
        mut options: RunOptions,
    ) -> Result<AgentResult, AgentError> {
        options.max_steps = options.max_steps.max(self.max_steps);
        let run_id = String::new();
        let hook_state = self.on_start(&run_id, &state).await?;
        if let Some(hook_state) = hook_state {
            state.insert(HOOKS, hook_state);
        }
        let result = self.workflow.run_with(state, options).await;
        self.finish(result).await
    }

    /// Runs every `on_start` hook (in order). Returns the hooks' state to
    /// store, if any hook set some.
    async fn on_start(&self, run_id: &str, state: &State) -> Result<Option<Value>, AgentError> {
        if self.hooks.is_empty() {
            return Ok(None);
        }
        let mut saved = read_hooks(state);
        let mut changed = false;
        let state = Arc::new(state.clone());
        for hook in self.hooks.iter() {
            let ctx = hook_context(run_id, &state, hook.name(), &saved)
                .map_err(AgentError::invalid_state)?;
            let action = hook.on_start(&ctx).await.map_err(|err| AgentError {
                code: "hook_failed".into(),
                message: format!("hook `{}` failed in on_start: {err}", hook.name()),
                details: Some(json!({"hook": hook.name(), "stage": "on_start"})),
                run_error: None,
            })?;
            if let Some(value) = action.state {
                saved.insert(hook.name().to_string(), value);
                changed = true;
            }
        }
        Ok(changed.then_some(Value::Object(saved)))
    }

    /// Runs every `on_end` hook (in reverse order) and builds the result.
    async fn finish(&self, result: Result<RunOutput, RunError>) -> Result<AgentResult, AgentError> {
        let (outcome, state, run_id) = match &result {
            Ok(output) if output.is_suspended() => (
                Outcome::Suspended,
                output.state.clone(),
                output.run_id.clone(),
            ),
            Ok(output) => (
                Outcome::Completed,
                output.state.clone(),
                output.run_id.clone(),
            ),
            Err(error) => (
                Outcome::Failed,
                error.state.clone(),
                error
                    .snapshot()
                    .map(|s| s.run_id.clone())
                    .unwrap_or_default(),
            ),
        };
        if !self.hooks.is_empty() {
            let saved = read_hooks(&state);
            let state = Arc::new(state);
            for hook in self.hooks.iter().rev() {
                if let Ok(ctx) = hook_context(&run_id, &state, hook.name(), &saved) {
                    hook.on_end(&ctx, outcome).await;
                }
            }
        }
        AgentResult::from_output(result.map_err(AgentError::from_run)?)
    }

    /// Continues a run suspended for approval, with one decision per
    /// requested call (keyed by `call_id`). Decisions are checked first
    /// (one per request, allowed kind, valid edited arguments); invalid
    /// input fails with `resume_invalid` and leaves the snapshot usable.
    pub async fn resume<I>(
        &self,
        snapshot: &Snapshot,
        decisions: I,
    ) -> Result<AgentResult, AgentError>
    where
        I: IntoIterator<Item = (String, Decision)>,
    {
        self.resume_with(snapshot, decisions, self.run_options())
            .await
    }

    /// [`resume`](Self::resume) with options (events, deadline, ...).
    pub async fn resume_with<I>(
        &self,
        snapshot: &Snapshot,
        decisions: I,
        mut options: RunOptions,
    ) -> Result<AgentResult, AgentError>
    where
        I: IntoIterator<Item = (String, Decision)>,
    {
        let decisions: BTreeMap<String, Decision> = decisions.into_iter().collect();
        let requests = approval_requests(&snapshot.suspended);
        if requests.is_empty() {
            return Err(AgentError::resume_invalid(
                "the snapshot is not waiting for approval",
            ));
        }
        check_decisions(&requests, &decisions, &self.schemas)
            .map_err(AgentError::resume_invalid)?;
        options.max_steps = options.max_steps.max(self.max_steps);
        self.on_start(&snapshot.run_id, &snapshot.state).await?;
        let result = self
            .workflow
            .resume_with(snapshot, [(REVIEW_NODE, to_json(&decisions))], options)
            .await;
        self.finish(result).await
    }

    /// Continues a failed run from its snapshot. Tool calls that finished
    /// before the failure are not run again.
    pub async fn retry(&self, snapshot: &Snapshot) -> Result<AgentResult, AgentError> {
        self.retry_with(snapshot, self.run_options()).await
    }

    /// [`retry`](Self::retry) with options.
    pub async fn retry_with(
        &self,
        snapshot: &Snapshot,
        mut options: RunOptions,
    ) -> Result<AgentResult, AgentError> {
        options.max_steps = options.max_steps.max(self.max_steps);
        self.on_start(&snapshot.run_id, &snapshot.state).await?;
        let result = self.workflow.retry_with(snapshot, options).await;
        self.finish(result).await
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
    /// With stop reason `limit`: which limit (`model_calls`, `tokens`,
    /// `tool_calls`).
    pub limit: Option<String>,
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
            limit: book.limit,
            run,
        })
    }

    /// The output as text (`None` if there is no output or it is not text).
    pub fn text(&self) -> Option<&str> {
        self.output.as_ref().and_then(Value::as_str)
    }

    /// `true` if the run is waiting for decisions ([`Agent::resume`]).
    pub fn is_suspended(&self) -> bool {
        self.run.is_suspended()
    }

    /// The calls waiting for approval (empty unless suspended).
    pub fn approval_requests(&self) -> Vec<ApprovalRequest> {
        self.run
            .snapshot()
            .map(|s| approval_requests(&s.suspended))
            .unwrap_or_default()
    }

    /// The snapshot to resume from, if suspended. Plain JSON: store it
    /// anywhere.
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.run.snapshot()
    }
}

/// The approval requests in a snapshot's interrupts.
pub(crate) fn approval_requests(interrupts: &[agentbuild_core::Interrupt]) -> Vec<ApprovalRequest> {
    interrupts
        .iter()
        .filter(|i| i.node == REVIEW_NODE && i.payload["kind"] == APPROVAL_INTERRUPT)
        .filter_map(|i| serde_json::from_value(i.payload["requests"].clone()).ok())
        .flat_map(|requests: Vec<ApprovalRequest>| requests)
        .collect()
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
                    Some(
                        "model_failed"
                            | "tool_failed"
                            | "limit_exceeded"
                            | "invalid_state"
                            | "resume_invalid"
                            | "output_invalid"
                            | "hook_failed"
                            | "deadline_exceeded"
                            | "update_conflict"
                            | "storage_failed"
                    )
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

    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
            run_error: None,
        }
    }

    pub(crate) fn resume_invalid(message: impl Into<String>) -> Self {
        Self {
            code: "resume_invalid".into(),
            message: message.into(),
            details: None,
            run_error: None,
        }
    }

    pub(crate) fn invalid_state(error: NodeError) -> Self {
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

pub(crate) fn read_messages(state: &State) -> Result<Vec<Message>, NodeError> {
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

/// The saved state of every hook (`hooks` key).
fn read_hooks(state: &State) -> Map<String, Value> {
    match state.get(HOOKS) {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    }
}

fn hook_context(
    run_id: &str,
    state: &Arc<State>,
    hook: &str,
    saved: &Map<String, Value>,
) -> Result<HookContext, NodeError> {
    Ok(HookContext {
        run_id: run_id.to_string(),
        messages: read_messages(state)?,
        model_calls: read_book(state)?.model_calls,
        hook_state: saved.get(hook).cloned(),
        state: state.clone(),
    })
}

fn hook_failed(hook: &str, stage: &str, error: impl std::fmt::Display) -> NodeError {
    NodeError::new(format!("hook `{hook}` failed in {stage}: {error}"))
        .with_kind("hook_failed")
        .with_details(json!({"hook": hook, "stage": stage}))
}

fn to_json<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("agent data serializes")
}

fn limit_error(limit: &str, value: u64) -> NodeError {
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
    chain: ModelChain,
    instructions: Option<String>,
    specs: Vec<ToolSpec>,
    limits: Limits,
    approval: Option<Arc<Approval>>,
    output: Option<Arc<OutputConfig>>,
    hooks: Arc<[Arc<dyn Hook>]>,
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
    // Budgets (0004 §5), checked before the call.
    let limits = &inner.limits;
    let used_tokens = book.usage.input_tokens + book.usage.output_tokens;
    let hit = if book.model_calls >= limits.model_calls {
        Some(("model_calls", u64::from(limits.model_calls)))
    } else if let Some(max) = limits.tokens.filter(|max| used_tokens >= *max) {
        Some(("tokens", max))
    } else if book.tool_budget_spent {
        Some(("tool_calls", u64::from(limits.tool_calls)))
    } else {
        None
    };
    if let Some((limit, value)) = hit {
        if limits.on_limit == OnLimit::Error {
            return Err(limit_error(limit, value));
        }
        // Graceful end: a valid transcript and the last assistant text.
        let repaired = transcript::repair(messages);
        let last_text = repaired
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            .map(Message::text)
            .unwrap_or_default();
        book.stop_reason = Some(StopReason::Limit);
        book.output = Some(Value::String(last_text));
        book.limit = Some(limit.to_string());
        let mut update = Update::new().set(AGENT, to_json(&book));
        if !repaired.changes.is_empty() {
            update = update.set(MESSAGES, json!({"$replace": repaired.messages}));
        }
        return Ok(NodeOutput::goto(update, [END]));
    }

    let repaired = transcript::repair(messages);
    let events = ctx.events().clone();
    if !repaired.changes.is_empty() {
        events.emit("transcript_repaired", json!({"changes": repaired.changes}));
    }
    // before_model hooks (in order): may save state, replace the
    // transcript, or end the run without calling the model.
    let mut transcript = repaired.messages;
    let mut replaced = !repaired.changes.is_empty();
    let mut saved_hooks = read_hooks(&state);
    let mut hooks_changed = false;
    for hook in inner.hooks.iter() {
        let hook_ctx = HookContext {
            run_id: ctx.run_id().to_string(),
            messages: transcript.clone(),
            model_calls: book.model_calls,
            hook_state: saved_hooks.get(hook.name()).cloned(),
            state: state.clone(),
        };
        let action = hook
            .before_model(&hook_ctx)
            .await
            .map_err(|err| hook_failed(hook.name(), "before_model", err))?;
        if let Some(value) = action.state {
            saved_hooks.insert(hook.name().to_string(), value);
            hooks_changed = true;
        }
        if let Some(messages) = action.messages {
            transcript = messages;
            replaced = true;
        }
        if let Some(output) = action.end {
            book.stop_reason = Some(StopReason::HookEnd);
            book.output = output;
            let mut update = Update::new().set(AGENT, to_json(&book));
            if replaced {
                update = update.set(MESSAGES, json!({"$replace": transcript}));
            }
            if hooks_changed {
                update = update.set(HOOKS, Value::Object(saved_hooks));
            }
            return Ok(NodeOutput::goto(update, [END]));
        }
    }

    let mut request_messages = Vec::with_capacity(transcript.len() + 1);
    if let Some(instructions) = &inner.instructions {
        request_messages.push(Message::system(instructions.clone()).with_id("msg_instructions"));
    }
    request_messages.extend(transcript.iter().cloned());
    let mut tools = inner.specs.clone();
    let mut request_output = None;
    match inner.output.as_deref() {
        Some(output) if output.strategy == OutputStrategy::Native => {
            request_output = Some(
                OutputSpec::new(output.schema().clone()).with_strategy(OutputStrategy::Native),
            );
        }
        Some(output) => tools.push(output.tool_spec()),
        None => {}
    }
    let mut request = ModelRequest::new(request_messages).with_tools(tools);
    request.output = request_output;
    if events.is_enabled() {
        // Enough to rebuild the call in a tracing tool (design 0009 §2).
        let tools: Vec<&str> = inner.specs.iter().map(|t| t.name.as_str()).collect();
        events.emit(
            "model_request",
            json!({
                "provider": inner.chain.models[0].provider(),
                "model": inner.chain.models[0].model(),
                "settings": request.settings,
                "tools": tools,
                "messages": request.messages,
            }),
        );
    }

    let delta_events = events.clone();
    let on_delta = Arc::new(move |delta: MessageDelta| {
        if delta_events.is_enabled() {
            delta_events.emit("message_delta", to_json(&delta));
        }
    });
    // wrap_model hooks nest around the call (the first hook outermost).
    let env = CallEnv {
        events: events.clone(),
        deadline: ctx
            .remaining_time()
            .map(|left| tokio::time::Instant::now() + left),
        seed: ctx.task_id(),
    };
    let deadline_hit = std::sync::atomic::AtomicBool::new(false);
    let chain = ModelNext {
        hooks: &inner.hooks,
        chain: &inner.chain,
        env: &env,
        on_delta,
        deadline_hit: &deadline_hit,
    };
    let response = match chain.call(request).await {
        Ok(response) => response,
        Err(error) if deadline_hit.load(std::sync::atomic::Ordering::SeqCst) => {
            return Err(NodeError::new(format!(
                "the next model retry would pass the run's deadline (last error: {error})"
            ))
            .with_kind("deadline_exceeded")
            .with_details(json!({"model_error": error})));
        }
        Err(error) => return Err(model_error(error)),
    };
    for warning in &response.warnings {
        events.emit("warning", to_json(warning));
    }
    let mut message = response.message;
    // after_model hooks (reverse order): may save state, replace the new
    // message, or end the run.
    let mut hook_end = None;
    for hook in inner.hooks.iter().rev() {
        let mut messages = transcript.clone();
        messages.push(message.clone());
        let hook_ctx = HookContext {
            run_id: ctx.run_id().to_string(),
            messages,
            model_calls: book.model_calls + 1,
            hook_state: saved_hooks.get(hook.name()).cloned(),
            state: state.clone(),
        };
        let action = hook
            .after_model(&hook_ctx)
            .await
            .map_err(|err| hook_failed(hook.name(), "after_model", err))?;
        if let Some(value) = action.state {
            saved_hooks.insert(hook.name().to_string(), value);
            hooks_changed = true;
        }
        if let Some(replacement) = action.message {
            message = replacement;
        }
        if hook_end.is_none() {
            hook_end = action.end;
        }
    }
    events.emit(
        "message_completed",
        json!({
            "message_id": message.id,
            "message": message,
            "finish_reason": response.finish_reason,
            "usage": response.usage,
            "model": message
                .metadata
                .model
                .as_deref()
                .unwrap_or(inner.chain.models[0].model()),
        }),
    );

    book.model_calls += 1;
    book.usage = book.usage.add(&response.usage);
    // Routing after the model (0004 §4.2).
    let mut reminders = Vec::new();
    let next = if let Some(output) = hook_end {
        book.stop_reason = Some(StopReason::HookEnd);
        book.output = output;
        END
    } else if response.finish_reason == FinishReason::ContentFilter {
        book.stop_reason = Some(StopReason::ContentFilter);
        book.output = Some(Value::String(message.text()));
        END
    } else if message.call_ids().next().is_some() {
        let gated = inner.approval.as_ref().is_some_and(|approval| {
            message.tool_calls().any(|call| {
                inner.specs.iter().any(|s| s.name == call.name)
                    && approval.rule_for(call.name, call.args).is_some()
            })
        });
        if gated { REVIEW_NODE } else { TOOLS_NODE }
    } else if let Some(output) = inner.output.as_deref() {
        // Rule 5: an answer is due, in the required shape.
        let problems = match output.strategy {
            OutputStrategy::Native => match output.accept_text(&message.text()) {
                Ok(value) => {
                    book.stop_reason = Some(StopReason::Output);
                    book.output = Some(value);
                    None
                }
                Err(problems) => Some(format!(
                    "Your answer does not match the required format: {}. Answer again with only the JSON.",
                    problems.join("; ")
                )),
            },
            _ => Some(format!(
                "Give your final answer by calling the `{FINAL_OUTPUT_TOOL}` tool."
            )),
        };
        match problems {
            None => END,
            Some(reminder) => {
                count_output_retry(&mut book, &inner.limits, &reminder, &message.text())?;
                reminders.push(Message::user(reminder));
                MODEL_NODE
            }
        }
    } else {
        book.stop_reason = Some(StopReason::FinalAnswer);
        book.output = Some(Value::String(message.text()));
        END
    };

    let messages_update = if !replaced {
        let mut new = vec![message];
        new.append(&mut reminders);
        json!(new)
    } else {
        // Repairs and hook rewrites change earlier messages, which an upsert
        // (append) cannot express: replace the list (both are rare).
        let mut all = transcript;
        all.push(message);
        all.append(&mut reminders);
        json!({"$replace": all})
    };
    let mut update = Update::new()
        .set(MESSAGES, messages_update)
        .set(AGENT, to_json(&book));
    if hooks_changed {
        update = update.set(HOOKS, Value::Object(saved_hooks));
    }
    Ok(NodeOutput::goto(update, [next]))
}

/// Counts an invalid structured-output answer; fails with `output_invalid`
/// once [`Limits::output_retries`] is used up.
fn count_output_retry(
    book: &mut Bookkeeping,
    limits: &Limits,
    problem: &str,
    raw: &str,
) -> Result<(), NodeError> {
    book.output_retries += 1;
    if book.output_retries > limits.output_retries {
        return Err(NodeError::new(format!(
            "no valid structured output after {} attempt(s): {problem}",
            book.output_retries
        ))
        .with_kind("output_invalid")
        .with_details(json!({"problem": problem, "raw": raw})));
    }
    Ok(())
}

// ---------- review node ----------

struct ReviewInner {
    approval: Arc<Approval>,
    schemas: HashMap<String, Value>,
}

struct ReviewNode(Arc<ReviewInner>);

impl Node for ReviewNode {
    fn run(
        &self,
        ctx: NodeContext,
        state: Arc<State>,
    ) -> BoxFuture<'static, Result<NodeOutput, NodeError>> {
        Box::pin(review_step(self.0.clone(), ctx, state))
    }
}

/// Suspends with the gated calls of the last assistant message; on resume,
/// records the decisions for the tools step (0004 §7.2). No side effects.
async fn review_step(
    inner: Arc<ReviewInner>,
    ctx: NodeContext,
    state: Arc<State>,
) -> Result<NodeOutput, NodeError> {
    let messages = read_messages(&state)?;
    let mut book = read_book(&state)?;
    let assistant = messages
        .iter()
        .rfind(|m| m.role == Role::Assistant)
        .ok_or_else(|| {
            NodeError::new("no assistant message to review").with_kind("invalid_state")
        })?;
    let requests: Vec<ApprovalRequest> = assistant
        .tool_calls()
        .filter(|call| inner.schemas.contains_key(call.name))
        .filter_map(|call| inner.approval.request(call.id, call.name, call.args))
        .collect();

    let Some(answer) = ctx.resume() else {
        if requests.is_empty() {
            return Ok(NodeOutput::goto(Update::new(), [TOOLS_NODE]));
        }
        return Ok(NodeOutput::suspend(
            Update::new(),
            json!({"kind": APPROVAL_INTERRUPT, "requests": requests}),
        ));
    };
    let decisions: BTreeMap<String, Decision> =
        serde_json::from_value(answer.clone()).map_err(|err| {
            NodeError::new(format!("decisions are not valid: {err}")).with_kind("resume_invalid")
        })?;
    check_decisions(&requests, &decisions, &inner.schemas)
        .map_err(|reason| NodeError::new(reason).with_kind("resume_invalid"))?;
    book.decisions = decisions;
    Ok(NodeOutput::goto(
        Update::new().set(AGENT, to_json(&book)),
        [TOOLS_NODE],
    ))
}

// ---------- tools node ----------

struct ToolsInner {
    tools: HashMap<String, Arc<dyn Tool>>,
    limits: Limits,
    output: Option<Arc<OutputConfig>>,
    hooks: Arc<[Arc<dyn Hook>]>,
    /// Tools with `max_concurrency`.
    per_tool: HashMap<String, Arc<Semaphore>>,
    retry: RetryPolicy,
    app: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

/// A finished call as saved in the tools task's memo, so a retried step
/// reuses everything the call produced (not just its message).
#[derive(Serialize, Deserialize)]
struct SavedCall {
    message: Message,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    state: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    direct: Option<Value>,
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
    /// Tells the model that a reviewer changed the call.
    notice: Option<String>,
}

impl SavedCall {
    /// Reads a saved call; entries from before state updates existed hold
    /// only the message.
    fn read(value: Value) -> Option<Self> {
        serde_json::from_value::<SavedCall>(value.clone())
            .or_else(|_| {
                serde_json::from_value::<Message>(value).map(|message| SavedCall {
                    message,
                    state: Map::new(),
                    direct: None,
                })
            })
            .ok()
    }
}

/// A finished call.
struct Finished {
    index: usize,
    message: Message,
    /// The tool's output, when it succeeded with `return_direct`.
    direct: Option<Value>,
    /// State changes the tool returned.
    state: Map<String, Value>,
}

/// Cuts oversized content (0002 §4.3). Returns the content to send, the
/// full content (for the artifact) if it was cut.
fn truncate_content(content: Vec<ContentBlock>, max: usize) -> (Vec<ContentBlock>, Option<Value>) {
    let rendered: Vec<String> = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            ContentBlock::Json { value, .. } => Some(value.to_string()),
            _ => None,
        })
        .collect();
    let text = rendered.join("\n");
    let total = text.chars().count();
    if total <= max {
        return (content, None);
    }
    let cut: String = text.chars().take(max).collect();
    let full = json!(content);
    let mut kept = vec![ContentBlock::text(format!(
        "{cut}\n[truncated: {} more characters]",
        total - max
    ))];
    kept.extend(
        content.into_iter().filter(|block| {
            !matches!(block, ContentBlock::Text { .. } | ContentBlock::Json { .. })
        }),
    );
    (kept, Some(full))
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
    let mut direct_outputs: Vec<(usize, Value)> = Vec::new();
    let mut state_writes: Vec<(usize, Map<String, Value>)> = Vec::new();
    let mut results: Vec<Option<Message>> = Vec::new();
    let mut pending = Vec::new();
    let mut executed = 0u32;
    let mut available: Vec<&str> = inner.tools.keys().map(String::as_str).collect();
    available.sort_unstable();
    // Tool strategy: when the model calls `final_output`, that call decides
    // the turn and every other call is skipped (0004 §4.3, rule 4).
    let final_call = inner
        .output
        .as_deref()
        .filter(|o| o.strategy != OutputStrategy::Native)
        .and_then(|_| {
            assistant
                .tool_calls()
                .find(|c| c.name == FINAL_OUTPUT_TOOL)
                .map(|c| c.id.to_string())
        });
    let mut final_output: Option<Result<Value, String>> = None;

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
        if let Some(final_id) = &final_call {
            let output = inner.output.as_deref().expect("final call implies output");
            let message = if call_id == final_id {
                match output.accept_tool_args(&Value::Object(args.clone())) {
                    Ok(value) => {
                        final_output = Some(Ok(value));
                        Message::tool_result(
                            call_id,
                            name,
                            ToolStatus::Success,
                            vec![ContentBlock::text("Final output accepted.")],
                        )
                        .with_id(result_id(call_id))
                    }
                    Err(problems) => {
                        let problem = format!("Invalid final output: {}", problems.join("; "));
                        final_output = Some(Err(problem.clone()));
                        error_result(call_id, name, "invalid_arguments", problem)
                    }
                }
            } else {
                skipped_result(
                    call_id,
                    name,
                    "the final output was provided in this turn".to_string(),
                )
            };
            results.push(Some(message));
            continue;
        }
        // A reviewer's decision (0004 §7.2) comes before anything else.
        let mut notice = None;
        let (name, args) = match book.decisions.get(call_id) {
            Some(Decision::Reject { message }) => {
                let text = message.clone().unwrap_or_else(|| {
                    "The reviewer rejected this call. Do not retry it.".to_string()
                });
                results.push(Some(
                    Message::tool_result(
                        call_id,
                        name,
                        ToolStatus::Rejected,
                        vec![ContentBlock::text(text)],
                    )
                    .with_id(result_id(call_id)),
                ));
                continue;
            }
            Some(Decision::Respond { message }) => {
                results.push(Some(
                    Message::tool_result(
                        call_id,
                        name,
                        ToolStatus::Success,
                        vec![ContentBlock::text(message.clone())],
                    )
                    .with_id(result_id(call_id)),
                ));
                continue;
            }
            Some(Decision::Edit {
                name: new_name,
                args: new_args,
            }) => {
                let tool = new_name.as_ref().unwrap_or(name);
                notice = Some(format!(
                    "Note: a human reviewer approved this call with changes, so it ran as \
                     {tool}({}). This is the intended action, not a mistake: describe the \
                     result in terms of these arguments.",
                    Value::Object(new_args.clone())
                ));
                (tool, new_args)
            }
            _ => (name, args),
        };
        // Saved by an earlier attempt of this step: never run twice.
        if let Some(saved) = memo.get(&format!("call:{call_id}"))
            && let Some(saved) = SavedCall::read(saved)
        {
            executed += 1;
            if let Some(direct) = saved.direct {
                direct_outputs.push((index, direct));
            }
            if !saved.state.is_empty() {
                state_writes.push((index, saved.state));
            }
            results.push(Some(saved.message));
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
            book.tool_budget_spent = true;
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
                    notice: notice.clone(),
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
    // The first finished call whose result storage could not take.
    let unsaved: Arc<std::sync::Mutex<Option<NodeError>>> = Arc::default();
    for call in pending {
        let semaphore = semaphore.clone();
        let memo = memo.clone();
        let unsaved = unsaved.clone();
        let events = ctx.events().clone();
        let tool_ctx = ToolContext {
            call_id: call.call_id.clone(),
            run_id: ctx.run_id().to_string(),
            cancel: ctx.cancel_token().child(),
            events: events.clone(),
            depth: book.depth,
            app: inner.app.clone(),
        };
        let timeout = call
            .tool
            .spec()
            .timeout_ms
            .map_or(limits.tool_timeout, Duration::from_millis);
        let return_direct = call.tool.spec().return_direct;
        let retry_safe = call.tool.spec().annotations.retry_safe();
        let max_chars = call
            .tool
            .spec()
            .max_result_chars
            .map_or(limits.max_result_chars, |n| n as usize);
        let tool_permits = inner.per_tool.get(&call.name).cloned();
        let (call_id, name) = (call.call_id.clone(), call.name.clone());
        let inner = inner.clone();
        let handle = tasks.spawn(async move {
            let _permit = semaphore.acquire_owned().await.expect("semaphore open");
            // Calls to a tool with `max_concurrency` wait their turn (FIFO).
            let _tool_permit = match tool_permits {
                Some(permits) => Some(permits.acquire_owned().await.expect("semaphore open")),
                None => None,
            };
            events.emit(
                "tool_call_started",
                json!({"call_id": call.call_id, "tool": call.name, "args": call.args}),
            );
            let started = Instant::now();
            // Only retry-safe tools (read-only or idempotent) are retried,
            // after a retryable error or a timeout (0002 §5.2).
            let max_attempts = if retry_safe {
                inner.retry.max_attempts.max(1)
            } else {
                1
            };
            let mut attempt = 1;
            let outcome = loop {
                // wrap_tool hooks nest around the call (the first hook outermost).
                let chain = ToolNext {
                    hooks: &inner.hooks,
                    tools: &inner.tools,
                    ctx: tool_ctx.clone(),
                };
                let request = ToolCallRequest {
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    args: call.args.clone(),
                };
                let outcome = tokio::time::timeout(timeout, chain.call(request)).await;
                let retryable = match &outcome {
                    Ok(Ok(_)) => false,
                    Ok(Err(error)) => !error.fatal && error.retryable,
                    Err(_) => true,
                };
                if !retryable || attempt >= max_attempts {
                    break outcome;
                }
                let delay = inner.retry.delay(attempt, &call.call_id);
                events.emit(
                    "tool_retry",
                    json!({
                        "call_id": call.call_id,
                        "tool": call.name,
                        "attempt": attempt,
                        "retry_in_ms": delay.as_millis() as u64,
                    }),
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
            };
            let mut state = Map::new();
            let (message, direct) = match outcome {
                Ok(Ok(output)) => {
                    let (content, artifact, tool_state, value) = output.into_parts();
                    state = tool_state;
                    let direct = return_direct.then_some(value);
                    let content = if content.is_empty() {
                        vec![ContentBlock::text("(no output)")]
                    } else {
                        content
                    };
                    let (content, full) = truncate_content(content, max_chars);
                    let mut message = Message::tool_result(
                        &call.call_id,
                        &call.name,
                        ToolStatus::Success,
                        content,
                    )
                    .with_id(result_id(&call.call_id));
                    if let Some(full) = full {
                        message.metadata.truncated = true;
                        events.emit(
                            "warning",
                            json!({
                                "code": "tool_result_truncated",
                                "message": format!(
                                    "the result of `{}` ({}) was cut to {max_chars} characters",
                                    call.name, call.call_id
                                ),
                            }),
                        );
                        message.artifact = Some(artifact.map_or(
                            full.clone(),
                            |a| json!({"artifact": a, "full_content": full}),
                        ));
                    } else {
                        message.artifact = artifact;
                    }
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
            if let Some(notice) = call.notice {
                message.content.insert(0, ContentBlock::text(notice));
            }
            message.metadata.duration_ms = Some(started.elapsed().as_millis() as u64);
            message.metadata.attempts = Some(attempt);
            // Saved at once, so a failure elsewhere in this step never makes
            // this call run again (0005 §5.1).
            let saved = SavedCall {
                message,
                state,
                direct,
            };
            // With durable storage this also waits until the result is
            // stored, so not even a crash makes the call run again.
            let key = format!("call:{}", call.call_id);
            if let Err(error) = memo.save(key, to_json(&saved)).await {
                let mut unsaved = unsaved.lock().unwrap_or_else(|e| e.into_inner());
                unsaved.get_or_insert(error);
            }
            let SavedCall {
                message,
                state,
                direct,
            } = saved;
            events.emit(
                "tool_call_finished",
                json!({
                    "call_id": call.call_id,
                    "tool": call.name,
                    "status": message.status,
                    "duration_ms": message.metadata.duration_ms,
                    "content": message.content,
                    "error": message.error,
                }),
            );
            Ok(Finished {
                index: call.index,
                message,
                direct,
                state,
            })
        });
        task_calls.insert(handle.id(), (call_id, name));
    }

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
        if !finished.state.is_empty() {
            state_writes.push((finished.index, finished.state));
        }
        results[finished.index] = Some(finished.message);
    }
    // Storage is failing: stop before the model sees results that would
    // be lost. The results stay in memory, so a retry does not repeat the
    // calls.
    let unsaved = unsaved.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(error) = unsaved {
        return Err(error);
    }
    // Tool state changes, merged in call order (0002 §4.4).
    state_writes.sort_by_key(|(index, _)| *index);
    let mut tool_state: Map<String, Value> = Map::new();
    for (_, writes) in state_writes {
        for (key, value) in writes {
            if [MESSAGES, AGENT, HOOKS].contains(&key.as_str()) {
                return Err(NodeError::new(format!(
                    "a tool may not write the agent's own state key `{key}`"
                ))
                .with_kind("tool_failed"));
            }
            match (tool_state.get_mut(&key), value) {
                (None, value) => {
                    tool_state.insert(key, value);
                }
                (Some(Value::Array(existing)), Value::Array(more)) => existing.extend(more),
                (Some(_), _) => {
                    return Err(NodeError::new(format!(
                        "two tool calls in one turn wrote `{key}`; return lists to join them"
                    ))
                    .with_kind("update_conflict"));
                }
            }
        }
    }

    let results: Vec<Message> = results.into_iter().flatten().collect();
    book.tool_calls += executed;
    book.decisions.clear();
    let next = if let Some(final_output) = final_output {
        match final_output {
            Ok(value) => {
                book.stop_reason = Some(StopReason::Output);
                book.output = Some(value);
                END
            }
            Err(problem) => {
                let raw = assistant
                    .tool_calls()
                    .find(|c| c.name == FINAL_OUTPUT_TOOL)
                    .map(|c| Value::Object(c.args.clone()).to_string())
                    .unwrap_or_default();
                count_output_retry(&mut book, &inner.limits, &problem, &raw)?;
                MODEL_NODE
            }
        }
    } else if direct_outputs.is_empty() {
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
    let mut update = Update::new()
        .set(MESSAGES, json!(results))
        .set(AGENT, to_json(&book));
    for (key, value) in tool_state {
        update = update.set(key, value);
    }
    Ok(NodeOutput::goto(update, [next]))
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
