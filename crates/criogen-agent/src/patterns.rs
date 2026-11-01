//! Design patterns for agentic systems, ready to use (design 0019).
//!
//! Each pattern is a function that builds a graph from the engine's existing
//! pieces (nodes, edges, routers, joins, `goto`, reducers, suspend); the
//! scheduling, merging and durability stay in the Rust engine.
//!
//! | Pattern | Function | Returns |
//! |---|---|---|
//! | ReAct | [`react`] | an [`Agent`] |
//! | Sequential | [`sequential`] | a [`Pattern`] |
//! | Parallel | [`parallel`] | a [`Pattern`] |
//! | Loop | [`loop_`] | a [`Pattern`] |
//! | Review and critique | [`review_and_critique`] | a [`Pattern`] |
//! | Iterative refinement | [`iterative_refinement`] | a [`Pattern`] |
//! | Coordinator | [`coordinator`] | an [`Agent`] |
//! | Hierarchical decomposition | [`hierarchical`] | an [`Agent`] |
//! | Swarm | [`swarm`] | a [`Pattern`] |
//! | Human in the loop | [`human_review`] | a [`Pattern`] |
//! | Custom logic | [`custom`] | a [`Pattern`] |
//!
//! [`quick`] builds a sequential, parallel or review pattern from one line of
//! registered specialist names.
//!
//! A **step** is an [`Agent`], a [`Specialist`] (see [`Step::specialist`]), a
//! function `text -> text` ([`Step::function`]), or any of them with a name
//! (`("research", agent)`). Text flows between steps. The state keys are
//! `input`, `current`, `output` and `trace` (every step's name and output),
//! plus per-pattern keys (`round`, `stop_reason`).
//!
//! Every loop has a cap with a hard ceiling of [`CEILING`] (50), and a
//! pattern ends with a [`PatternStop`], never by hanging.
//!
//! ```
//! use criogen_agent::{Agent, patterns};
//! use criogen_providers::{FakeModel, FakeReply};
//!
//! # tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap().block_on(async {
//! let research = Agent::builder(FakeModel::new().then(FakeReply::text("facts about solar")))
//!     .build()
//!     .unwrap();
//! let flow = patterns::sequential([
//!     ("research", patterns::Step::from(research)),
//!     ("shout", patterns::Step::function(|text| async move { text.to_uppercase() })),
//! ])
//! .unwrap();
//! let result = flow.run("Write about solar panels").await.unwrap();
//! assert_eq!(result.output.as_deref(), Some("FACTS ABOUT SOLAR"));
//! # });
//! ```

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::sync::Arc;

use criogen_core::{
    BoxFuture, BuildError, END, NodeContext, NodeError, NodeOutput, Reducer, RunError, RunOptions,
    RunOutput, Snapshot, State, Update, Workflow, WorkflowBuilder, node_fn, node_with_context,
    router_fn,
};
use criogen_providers::Model;
use criogen_storage::{Storage, WorkflowThread};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent::{Agent, AgentBuilder, CriogenError, Limits};
use crate::specialists::{self, Specialist, SpecialistError};
use crate::tool::Tool;

/// No loop runs more often than this, whatever the caller asks for.
pub const CEILING: u32 = 50;

// -------------------------------------------------------------------- errors

/// Why a pattern could not be built or run.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PatternError {
    /// The arguments do not make a pattern (no steps, a cap of 0, a bad
    /// one-liner ...).
    #[error("{0}")]
    Invalid(String),
    /// A specialist is unknown or invalid.
    #[error(transparent)]
    Specialist(#[from] SpecialistError),
    /// An agent could not be built.
    #[error(transparent)]
    Agent(#[from] CriogenError),
    /// The graph did not validate (for example after
    /// [`Pattern::extend`]).
    #[error(transparent)]
    Build(#[from] BuildError),
    /// The run failed. The error carries the state and a snapshot.
    #[error(transparent)]
    Run(Box<RunError>),
    /// [`Pattern::resume`] needs the result of a run that is waiting for a
    /// person.
    #[error("resume needs the result of a run that is waiting for a person")]
    NotSuspended,
}

impl From<RunError> for PatternError {
    fn from(error: RunError) -> Self {
        Self::Run(Box::new(error))
    }
}

fn invalid(message: impl Into<String>) -> PatternError {
    PatternError::Invalid(message.into())
}

fn cap(value: u32, name: &str) -> Result<u32, PatternError> {
    if value < 1 {
        return Err(invalid(format!(
            "{name} must be a whole number of at least 1"
        )));
    }
    Ok(value.min(CEILING))
}

// --------------------------------------------------------------------- steps

type StepFn =
    Arc<dyn Fn(NodeContext, String) -> BoxFuture<'static, Result<String, NodeError>> + Send + Sync>;

/// One unit of work in a pattern: an agent, a specialist or a function from
/// text to text, optionally named.
#[derive(Clone)]
pub struct Step {
    name: Option<String>,
    run: StepFn,
}

impl fmt::Debug for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Step")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Step {
    /// An agent. It runs on the text it is given; its text answer (or its
    /// JSON output as text) is the step's output. An agent that waits for
    /// approval fails the step: use [`Agent::as_node`] in your own workflow
    /// for that.
    pub fn agent(agent: Agent) -> Self {
        Self {
            name: None,
            run: Arc::new(move |ctx, text| {
                let agent = agent.clone();
                Box::pin(async move {
                    let mut options = agent.run_options().cancel_token(ctx.cancel_token().child());
                    if let Some(left) = ctx.remaining_time() {
                        options = options.deadline(left);
                    }
                    match agent.run_with(text, options).await {
                        Ok(result) if result.is_suspended() => Err(NodeError::new(
                            "the agent needs approval; use Agent::as_node in a workflow of your own to pause for a reviewer",
                        )),
                        Ok(result) => Ok(match result.output {
                            Some(Value::String(text)) => text,
                            Some(other) => other.to_string(),
                            None => String::new(),
                        }),
                        Err(error) => {
                            let mut node_error =
                                NodeError::new(error.message).with_kind(error.code);
                            if let Some(details) = error.details {
                                node_error = node_error.with_details(details);
                            }
                            Err(node_error)
                        }
                    }
                })
            }),
        }
    }

    /// A function `text -> text`.
    pub fn function<F, Fut>(f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = String> + Send + 'static,
    {
        Self::try_function(move |text| {
            let fut = f(text);
            async move { Ok(fut.await) }
        })
    }

    /// A function `text -> text` that can fail; the error fails the run.
    pub fn try_function<F, Fut>(f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, NodeError>> + Send + 'static,
    {
        Self {
            name: None,
            run: Arc::new(move |_ctx, text| Box::pin(f(text))),
        }
    }

    /// A specialist recipe built around `model`; the step is named after it.
    pub fn specialist(
        specialist: &Specialist,
        model: Arc<dyn Model>,
    ) -> Result<Self, PatternError> {
        Ok(Self::agent(specialist.build(model)?).named(specialist.name()))
    }

    /// A registered specialist ([`specialists::get`]) built around `model`.
    pub fn registered(name: &str, model: Arc<dyn Model>) -> Result<Self, PatternError> {
        Self::specialist(&specialists::get(name)?, model)
    }

    /// Names the step (the name shows in the trace and is the graph node's
    /// name). Default: `step1`, `step2` ... by position.
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

impl From<Agent> for Step {
    fn from(agent: Agent) -> Self {
        Self::agent(agent)
    }
}

impl<N: Into<String>, S: Into<Step>> From<(N, S)> for Step {
    fn from((name, step): (N, S)) -> Self {
        step.into().named(name)
    }
}

struct Resolved {
    name: String,
    run: StepFn,
}

fn resolve(step: Step, index: usize) -> Resolved {
    Resolved {
        name: step.name.unwrap_or_else(|| format!("step{}", index + 1)),
        run: step.run,
    }
}

fn resolve_all<I>(steps: I) -> Vec<Resolved>
where
    I: IntoIterator,
    I::Item: Into<Step>,
{
    steps
        .into_iter()
        .enumerate()
        .map(|(i, s)| resolve(s.into(), i))
        .collect()
}

const RESERVED: [&str; 8] = [
    "begin",
    "finish",
    "check",
    "combine",
    "generate",
    "review",
    "dispatch",
    "route_first",
];

/// Node names: distinct, and never one of the pattern's own.
fn unique(names: &[String]) -> Vec<String> {
    let mut seen: HashMap<String, u32> = HashMap::new();
    names
        .iter()
        .map(|n| {
            let n = if RESERVED.contains(&n.as_str()) {
                format!("{n}-step")
            } else {
                n.clone()
            };
            let count = seen.entry(n.clone()).or_insert(0);
            *count += 1;
            if *count == 1 {
                n
            } else {
                format!("{n}-{count}")
            }
        })
        .collect()
}

fn entry(step: &str, output: &str) -> Value {
    json!([{"step": step, "output": output}])
}

fn text_of(state: &State, key: &str) -> String {
    match state.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn number_of(state: &State, key: &str) -> u64 {
    state.get(key).and_then(Value::as_u64).unwrap_or(0)
}

// ------------------------------------------------------------------- results

/// Why a pattern ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PatternStop {
    /// Ran to the end.
    Done,
    /// A loop's condition was met.
    Condition,
    /// A critic or a person approved.
    Approved,
    /// A loop reached `max_iterations`.
    MaxIterations,
    /// A review loop reached `max_rounds` without approval.
    MaxRounds,
    /// A swarm agent said `FINAL`.
    Final,
    /// A swarm reached `max_handoffs`.
    MaxHandoffs,
    /// A swarm agent handed off to a name that is not in the team.
    UnknownHandoff,
    /// A person rejected the draft.
    Rejected,
    /// Waiting for a person ([`PatternResult::waiting_for`]).
    Suspended,
}

impl PatternStop {
    /// The stable snake_case name (`"max_rounds"`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Condition => "condition",
            Self::Approved => "approved",
            Self::MaxIterations => "max_iterations",
            Self::MaxRounds => "max_rounds",
            Self::Final => "final",
            Self::MaxHandoffs => "max_handoffs",
            Self::UnknownHandoff => "unknown_handoff",
            Self::Rejected => "rejected",
            Self::Suspended => "suspended",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "condition" => Self::Condition,
            "approved" => Self::Approved,
            "max_iterations" => Self::MaxIterations,
            "max_rounds" => Self::MaxRounds,
            "final" => Self::Final,
            "max_handoffs" => Self::MaxHandoffs,
            "unknown_handoff" => Self::UnknownHandoff,
            "rejected" => Self::Rejected,
            _ => Self::Done,
        }
    }
}

impl fmt::Display for PatternStop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One step's name and what it returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TraceEntry {
    /// The step's name (`"writer (draft 2)"` in a review loop).
    pub step: String,
    /// What it returned.
    pub output: String,
}

/// What a pattern produced. `Display` shows the output;
/// [`explain`](Self::explain) shows each step.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PatternResult {
    /// The answer (`state["output"]`); `None` while suspended.
    pub output: Option<String>,
    /// Every step's name and output, in order.
    pub trace: Vec<TraceEntry>,
    /// The final state.
    pub state: State,
    /// Why it ended.
    pub stop_reason: PatternStop,
    /// `true` if it waits for a person ([`Pattern::resume`]).
    pub suspended: bool,
    /// What a person is asked, when `suspended`.
    pub waiting_for: Option<Value>,
    /// The snapshot to resume from, when `suspended`.
    pub snapshot: Option<Snapshot>,
}

impl PatternResult {
    /// Each step on its own line: which step, what it returned (shortened).
    pub fn explain(&self) -> String {
        let mut lines = vec![format!("ended: {}", self.stop_reason)];
        for (i, entry) in self.trace.iter().enumerate() {
            let text = entry
                .output
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let shown: String = text.chars().take(140).collect();
            let more = if text.chars().count() > 140 {
                "…"
            } else {
                ""
            };
            lines.push(format!("{}. {}: {shown}{more}", i + 1, entry.step));
        }
        lines.join("\n")
    }
}

impl fmt::Display for PatternResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.output.as_deref().unwrap_or(""))
    }
}

/// A person's answer to a [`human_review`] pause. Pass it to
/// [`Pattern::resume`] (or send the same JSON through a durable thread).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReviewDecision {
    /// Use the draft as it is.
    Approve,
    /// Use this text instead.
    Edit(String),
    /// Throw the draft away; the output is empty.
    Reject,
}

impl From<ReviewDecision> for Value {
    fn from(decision: ReviewDecision) -> Self {
        match decision {
            ReviewDecision::Approve => json!("approve"),
            ReviewDecision::Edit(text) => json!({"decision": "edit", "text": text}),
            ReviewDecision::Reject => json!({"decision": "reject"}),
        }
    }
}

// ------------------------------------------------------------------- pattern

/// A workflow pattern: run it, thread it for durability, or extend it.
///
/// It owns the plain graph ([`builder`](Self::builder),
/// [`workflow`](Self::workflow)), so you can add nodes and edges with
/// [`extend`](Self::extend).
pub struct Pattern {
    name: String,
    builder: WorkflowBuilder,
    workflow: Workflow,
    budget: usize,
}

impl fmt::Debug for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pattern")
            .field("name", &self.name)
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}

impl Pattern {
    fn new(name: &str, builder: WorkflowBuilder, budget: usize) -> Result<Self, PatternError> {
        let workflow = builder.build()?;
        Ok(Self {
            name: name.to_string(),
            builder,
            workflow,
            budget,
        })
    }

    /// The pattern's name (`"sequential"`, `"swarm"` ...).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The compiled graph.
    pub fn workflow(&self) -> &Workflow {
        &self.workflow
    }

    /// The compiled graph, by value (for a [`Storage`] thread or your own
    /// runs).
    pub fn into_workflow(self) -> Workflow {
        self.workflow
    }

    /// The graph's declarations, for reading.
    pub fn builder(&self) -> &WorkflowBuilder {
        &self.builder
    }

    /// Adds nodes, edges or policies to the graph, then checks it again.
    pub fn extend(mut self, f: impl FnOnce(&mut WorkflowBuilder)) -> Result<Self, PatternError> {
        f(&mut self.builder);
        self.workflow = self.builder.build()?;
        Ok(self)
    }

    /// The step limit that fits this pattern's graph.
    pub fn max_steps(&self) -> usize {
        self.budget
    }

    /// The start state of a run on `text`: `{"input": text}`.
    pub fn input(text: impl Into<String>) -> State {
        State::new().with("input", text.into())
    }

    /// The durable form: a thread of this pattern in `storage`, with
    /// approvals, resume and an audit trail:
    /// `pattern.thread(&storage, "t-1").run(Pattern::input("go")).await`.
    pub fn thread(&self, storage: &Storage, id: impl Into<String>) -> WorkflowThread {
        storage
            .workflow_thread(&self.workflow, id)
            .with_options(RunOptions::default().max_steps(self.budget))
    }

    /// Runs once and returns the result. A [`human_review`] pattern returns
    /// `suspended: true`; continue with [`resume`](Self::resume).
    pub async fn run(&self, text: impl Into<String>) -> Result<PatternResult, PatternError> {
        self.run_with(text, RunOptions::default().max_steps(self.budget))
            .await
    }

    /// [`run`](Self::run) with options (events, deadline, cancellation). The
    /// options are used as given, including `max_steps`.
    pub async fn run_with(
        &self,
        text: impl Into<String>,
        options: RunOptions,
    ) -> Result<PatternResult, PatternError> {
        let output = self.workflow.run_with(Self::input(text), options).await?;
        Ok(Self::result(output))
    }

    /// Continues a [`human_review`] run with the person's decision
    /// ([`ReviewDecision`], or the JSON `"approve"`,
    /// `{"decision": "edit", "text": ...}`, `{"decision": "reject"}`).
    pub async fn resume(
        &self,
        paused: &PatternResult,
        decision: impl Into<Value>,
    ) -> Result<PatternResult, PatternError> {
        let snapshot = paused
            .snapshot
            .as_ref()
            .filter(|_| paused.suspended)
            .ok_or(PatternError::NotSuspended)?;
        let node = snapshot
            .suspended
            .first()
            .map(|i| i.node.clone())
            .ok_or(PatternError::NotSuspended)?;
        let output = self
            .workflow
            .resume_with(
                snapshot,
                [(node, decision.into())],
                RunOptions::default().max_steps(self.budget),
            )
            .await?;
        Ok(Self::result(output))
    }

    fn result(output: RunOutput) -> PatternResult {
        let suspended = output.is_suspended();
        let snapshot = output.snapshot().cloned();
        let waiting_for = output.interrupts().first().map(|i| i.payload.clone());
        let state = output.state;
        let stop_reason = if suspended {
            PatternStop::Suspended
        } else {
            state
                .get("stop_reason")
                .and_then(Value::as_str)
                .map_or(PatternStop::Done, PatternStop::parse)
        };
        let trace = state
            .get("trace")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| serde_json::from_value::<TraceEntry>(v.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();
        PatternResult {
            output: state
                .get("output")
                .and_then(Value::as_str)
                .map(String::from),
            trace,
            state,
            stop_reason,
            suspended,
            waiting_for,
            snapshot,
        }
    }
}

/// Custom logic: any graph, with the same `run` / `thread` as the
/// ready-made patterns. Its start state is `{"input": text}`; the answer is
/// `state["output"]`. `max_steps` bounds a run.
pub fn custom(builder: WorkflowBuilder, max_steps: usize) -> Result<Pattern, PatternError> {
    Pattern::new("custom", builder, max_steps)
}

fn begin() -> impl criogen_core::Node {
    node_fn(|state: Arc<State>| async move {
        Ok(Update::new()
            .set("current", text_of(&state, "input"))
            .set("trace", json!([]))
            .set("round", 0))
    })
}

fn finish(default: PatternStop) -> impl criogen_core::Node {
    node_fn(move |state: Arc<State>| async move {
        let reason = state
            .get("stop_reason")
            .and_then(Value::as_str)
            .unwrap_or(default.as_str())
            .to_string();
        Ok(Update::new()
            .set("output", text_of(&state, "current"))
            .set("stop_reason", reason))
    })
}

fn step_node(name: String, run: StepFn) -> impl criogen_core::Node {
    node_with_context(move |ctx, state: Arc<State>| {
        let run = run.clone();
        let name = name.clone();
        async move {
            let out = run(ctx, text_of(&state, "current")).await?;
            Ok(Update::new()
                .set("trace", entry(&name, &out))
                .set("current", out)
                .into())
        }
    })
}

fn chain(builder: &mut WorkflowBuilder, steps: &[Resolved], names: &[String]) -> String {
    let mut previous = "begin".to_string();
    for (step, name) in steps.iter().zip(names) {
        builder.add_node(name.clone(), step_node(name.clone(), step.run.clone()));
        builder.add_edge(previous, name.clone());
        previous = name.clone();
    }
    previous
}

fn base_builder() -> WorkflowBuilder {
    let mut builder = WorkflowBuilder::new();
    builder
        .reducer("trace", Reducer::Append)
        .add_node("begin", begin())
        .set_entry("begin");
    builder
}

// ----------------------------------------------------------- workflow patterns

/// Runs the steps in a fixed order; each one's output is the next one's input.
pub fn sequential<I>(steps: I) -> Result<Pattern, PatternError>
where
    I: IntoIterator,
    I::Item: Into<Step>,
{
    let resolved = resolve_all(steps);
    if resolved.is_empty() {
        return Err(invalid("sequential needs at least one step"));
    }
    let names = unique(&resolved.iter().map(|r| r.name.clone()).collect::<Vec<_>>());
    let mut builder = base_builder();
    let last = chain(&mut builder, &resolved, &names);
    builder
        .add_node("finish", finish(PatternStop::Done))
        .add_edge(last, "finish")
        .add_edge("finish", END);
    Pattern::new("sequential", builder, 3 * resolved.len() + 8)
}

/// Runs the branches at the same time on the same input, then combines them:
/// with a `synthesizer` step (it receives the input and every branch's
/// answer), or as a titled list (`## name`, then the answer) in branch order.
pub fn parallel<I>(branches: I, synthesizer: Option<Step>) -> Result<Pattern, PatternError>
where
    I: IntoIterator,
    I::Item: Into<Step>,
{
    let resolved = resolve_all(branches);
    if resolved.is_empty() {
        return Err(invalid("parallel needs at least one branch"));
    }
    let names = unique(&resolved.iter().map(|r| r.name.clone()).collect::<Vec<_>>());
    let synth = synthesizer.map(|s| resolve(s, 0));
    let mut builder = base_builder();
    builder.reducer("results", Reducer::Append);
    for (i, (step, name)) in resolved.iter().zip(&names).enumerate() {
        let (run, node_name) = (step.run.clone(), name.clone());
        builder.add_node(
            name.clone(),
            node_with_context(move |ctx, state: Arc<State>| {
                let (run, name) = (run.clone(), node_name.clone());
                async move {
                    let out = run(ctx, text_of(&state, "current")).await?;
                    Ok(Update::new()
                        .set("results", json!([{"i": i, "name": name, "output": out}]))
                        .set("trace", entry(&name, &out))
                        .into())
                }
            }),
        );
        builder.add_edge("begin", name.clone());
    }
    let synth = synth.map(|s| (s.name, s.run));
    builder.add_node(
        "combine",
        node_with_context(move |ctx, state: Arc<State>| {
            let synth = synth.clone();
            async move {
                let mut found: Vec<Value> = state
                    .get("results")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                found.sort_by_key(|r| r["i"].as_u64().unwrap_or(0));
                let listing = found
                    .iter()
                    .map(|r| {
                        format!(
                            "## {}\n{}",
                            r["name"].as_str().unwrap_or_default(),
                            r["output"].as_str().unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let Some((name, run)) = synth else {
                    return Ok(Update::new()
                        .set("current", listing)
                        .set("stop_reason", "done")
                        .into());
                };
                let prompt = format!(
                    "Original request:\n{}\n\nResults from {} specialists:\n\n{listing}",
                    text_of(&state, "input"),
                    found.len()
                );
                let out = run(ctx, prompt).await?;
                Ok(Update::new()
                    .set("trace", entry(&format!("{name} (synthesis)"), &out))
                    .set("current", out)
                    .into())
            }
        }),
    );
    builder
        .add_join(names.clone(), "combine")
        .add_node("finish", finish(PatternStop::Done))
        .add_edge("combine", "finish")
        .add_edge("finish", END);
    Pattern::new("parallel", builder, 2 * resolved.len() + 10)
}

fn again_or_done() -> impl criogen_core::Router {
    router_fn(|state: Arc<State>| async move {
        Ok::<_, NodeError>(if state.get("again") == Some(&Value::Bool(true)) {
            "again"
        } else {
            "done"
        })
    })
}

/// Repeats the steps (a chain, each output feeding the next) until
/// `until(text, round)` is true or `max_iterations` is reached (at most 50).
/// The result's [`stop_reason`](PatternResult::stop_reason) says which
/// (`Condition` or `MaxIterations`).
pub fn loop_<I>(
    steps: I,
    until: impl Fn(&str, u32) -> bool + Send + Sync + 'static,
    max_iterations: u32,
) -> Result<Pattern, PatternError>
where
    I: IntoIterator,
    I::Item: Into<Step>,
{
    let resolved = resolve_all(steps);
    if resolved.is_empty() {
        return Err(invalid("loop needs at least one step"));
    }
    let cap = cap(max_iterations, "max_iterations")?;
    let names = unique(&resolved.iter().map(|r| r.name.clone()).collect::<Vec<_>>());
    let mut builder = base_builder();
    let last = chain(&mut builder, &resolved, &names);
    let until = Arc::new(until);
    builder.add_node(
        "check",
        node_fn(move |state: Arc<State>| {
            let until = until.clone();
            async move {
                let round = number_of(&state, "round") as u32 + 1;
                let mut update = Update::new().set("round", round);
                if until(&text_of(&state, "current"), round) {
                    update = update.set("again", false).set("stop_reason", "condition");
                } else if round >= cap {
                    update = update
                        .set("again", false)
                        .set("stop_reason", "max_iterations");
                } else {
                    update = update.set("again", true);
                }
                Ok(update)
            }
        }),
    );
    builder
        .add_edge(last, "check")
        .add_router(
            "check",
            again_or_done(),
            [("again", names[0].clone()), ("done", "finish".to_string())],
        )
        .add_node("finish", finish(PatternStop::Done))
        .add_edge("finish", END);
    Pattern::new("loop", builder, cap as usize * (resolved.len() + 2) + 8)
}

/// Improves a result step by step: `refine` gets the current text and
/// returns a better one; stops when `score(text) >= threshold` or after
/// `max_iterations`.
pub fn iterative_refinement(
    refine: impl Into<Step>,
    score: impl Fn(&str) -> f64 + Send + Sync + 'static,
    threshold: f64,
    max_iterations: u32,
) -> Result<Pattern, PatternError> {
    loop_(
        [refine.into()],
        move |text, _round| score(text) >= threshold,
        max_iterations,
    )
}

/// A generator writes, a critic reviews. The critic approves (its reply
/// starts with `APPROVED`) or returns feedback; the generator revises. Ends
/// on approval or after `max_rounds` (at most 50) with the last draft. The
/// stop reason is `Approved` or `MaxRounds`.
pub fn review_and_critique(
    generator: impl Into<Step>,
    critic: impl Into<Step>,
    max_rounds: u32,
) -> Result<Pattern, PatternError> {
    review_and_critique_with(generator, critic, max_rounds, |reply| {
        reply.trim().to_uppercase().starts_with("APPROVED")
    })
}

/// [`review_and_critique`] with your own test for "the critic approved".
pub fn review_and_critique_with(
    generator: impl Into<Step>,
    critic: impl Into<Step>,
    max_rounds: u32,
    approved: impl Fn(&str) -> bool + Send + Sync + 'static,
) -> Result<Pattern, PatternError> {
    let cap = cap(max_rounds, "max_rounds")?;
    let generator = resolve(generator.into(), 0);
    let mut critic = resolve(critic.into(), 1);
    if generator.name == critic.name {
        critic.name.push_str("-critic");
    }
    let (gen_name, gen_run) = (generator.name, generator.run);
    let (crit_name, crit_run) = (critic.name, critic.run);
    let approved = Arc::new(approved);

    let mut builder = base_builder();
    builder.add_node(
        "generate",
        node_with_context(move |ctx, state: Arc<State>| {
            let (run, name) = (gen_run.clone(), gen_name.clone());
            async move {
                let feedback = text_of(&state, "feedback");
                let prompt = if feedback.is_empty() {
                    text_of(&state, "input")
                } else {
                    format!(
                        "{}\n\nYour previous draft:\n{}\n\nReviewer feedback to address:\n{feedback}\n\nWrite the improved version.",
                        text_of(&state, "input"),
                        text_of(&state, "current")
                    )
                };
                let out = run(ctx, prompt).await?;
                let round = number_of(&state, "round") + 1;
                Ok(Update::new()
                    .set("trace", entry(&format!("{name} (draft {round})"), &out))
                    .set("current", out)
                    .into())
            }
        }),
    );
    builder.add_node(
        "review",
        node_with_context(move |ctx, state: Arc<State>| {
            let (run, name, approved) = (crit_run.clone(), crit_name.clone(), approved.clone());
            async move {
                let prompt = format!(
                    "Request:\n{}\n\nDraft:\n{}",
                    text_of(&state, "input"),
                    text_of(&state, "current")
                );
                let reply = run(ctx, prompt).await?;
                let round = number_of(&state, "round") as u32 + 1;
                let update = Update::new()
                    .set("round", round)
                    .set("trace", entry(&name, &reply));
                let update = if approved(&reply) {
                    update
                        .set("again", false)
                        .set("stop_reason", "approved")
                        .set("feedback", Value::Null)
                } else if round >= cap {
                    update
                        .set("again", false)
                        .set("stop_reason", "max_rounds")
                        .set("feedback", reply)
                } else {
                    update.set("again", true).set("feedback", reply)
                };
                Ok(update.into())
            }
        }),
    );
    builder
        .add_edge("begin", "generate")
        .add_edge("generate", "review")
        .add_router(
            "review",
            again_or_done(),
            [("again", "generate"), ("done", "finish")],
        )
        .add_node("finish", finish(PatternStop::Done))
        .add_edge("finish", END);
    Pattern::new("review_and_critique", builder, cap as usize * 4 + 8)
}

/// Where a [`swarm`] starts.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SwarmStart {
    /// The first agent given.
    First,
    /// This agent.
    Agent(String),
    /// A dispatcher step: it reads the request and replies with an agent's
    /// name (an unknown reply starts with the first agent).
    Dispatcher(Step),
}

/// Agents hand the work to each other with no central coordinator. Each
/// agent is told who it can hand off to and ends its reply with
/// `HANDOFF: <name>` to pass the work on or `FINAL` when the answer is
/// ready. Ends on `FINAL` or after `max_handoffs` (at most 50); a hand-off
/// to an unknown name ends the run with [`PatternStop::UnknownHandoff`].
///
/// `agents` are `(name, step)` pairs; at least two, with distinct names that
/// are not one of the pattern's own node names (`begin`, `finish`, ...).
pub fn swarm<I, N, S>(
    agents: I,
    start: SwarmStart,
    max_handoffs: u32,
) -> Result<Pattern, PatternError>
where
    I: IntoIterator<Item = (N, S)>,
    N: Into<String>,
    S: Into<Step>,
{
    let agents: Vec<(String, Step)> = agents
        .into_iter()
        .map(|(n, s)| (n.into(), s.into()))
        .collect();
    if agents.len() < 2 {
        return Err(invalid("a swarm needs at least two agents"));
    }
    let cap = cap(max_handoffs, "max_handoffs")?;
    let mut seen = HashSet::new();
    for (name, _) in &agents {
        if RESERVED.contains(&name.as_str()) || !seen.insert(name.clone()) {
            return Err(invalid(format!(
                "swarm agent name {name:?} is reserved or used twice"
            )));
        }
    }
    let names: Arc<Vec<String>> = Arc::new(agents.iter().map(|(n, _)| n.clone()).collect());
    let runs: Arc<HashMap<String, StepFn>> = Arc::new(
        agents
            .into_iter()
            .map(|(n, s)| (n, s.run))
            .collect::<HashMap<_, _>>(),
    );
    let mut dispatcher = None;
    let mut first = names[0].clone();
    match start {
        SwarmStart::First => {}
        SwarmStart::Agent(name) => {
            if !runs.contains_key(&name) {
                return Err(invalid(format!(
                    "swarm start {name:?} is not one of the agents: {names:?}"
                )));
            }
            first = name;
        }
        SwarmStart::Dispatcher(step) => dispatcher = Some(step.run),
    }

    let mut builder = WorkflowBuilder::new();
    builder
        .reducer("trace", Reducer::Append)
        .add_node(
            "begin",
            node_fn(|state: Arc<State>| async move {
                Ok(Update::new()
                    .set("current", text_of(&state, "input"))
                    .set("trace", json!([]))
                    .set("round", 0)
                    .set("last", Value::Null))
            }),
        )
        .set_entry("begin")
        .add_node("finish", finish(PatternStop::Final))
        .add_edge("finish", END);

    for me in names.iter().cloned() {
        let (node_names, node_runs, node_me) = (names.clone(), runs.clone(), me.clone());
        builder.add_node(
            me.clone(),
            node_with_context(move |ctx, state: Arc<State>| {
                let (names, runs, me) = (node_names.clone(), node_runs.clone(), node_me.clone());
                async move {
                    let others = names
                        .iter()
                        .filter(|n| **n != me)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ");
                    let note = format!(
                        "You are '{me}' in a team. You can hand the work to: {others}. \
                         Do your part, then end your reply with exactly one line: 'HANDOFF: <name>' to pass it on, \
                         or 'FINAL' if the answer is ready for the user."
                    );
                    let prompt = format!(
                        "{note}\n\nRequest:\n{}\n\nWork so far:\n{}",
                        text_of(&state, "input"),
                        text_of(&state, "current")
                    );
                    let run = runs.get(&me).expect("every agent has a step");
                    let out = run(ctx, prompt).await?;
                    let round = number_of(&state, "round") + 1;
                    let update = Update::new()
                        .set("current", out.clone())
                        .set("round", round)
                        .set("trace", entry(&me, &out));
                    let tail = out
                        .lines()
                        .map(str::trim)
                        .rfind(|l| !l.is_empty())
                        .unwrap_or("FINAL");
                    let is_handoff = tail
                        .get(..8)
                        .is_some_and(|p| p.eq_ignore_ascii_case("HANDOFF:"));
                    if !is_handoff {
                        return Ok(NodeOutput::goto(
                            update.set("stop_reason", "final"),
                            ["finish"],
                        ));
                    }
                    let target = tail[8..].trim();
                    if !runs.contains_key(target) {
                        return Ok(NodeOutput::goto(
                            update.set("stop_reason", "unknown_handoff"),
                            ["finish"],
                        ));
                    }
                    if round > u64::from(cap) {
                        return Ok(NodeOutput::goto(
                            update.set("stop_reason", "max_handoffs"),
                            ["finish"],
                        ));
                    }
                    Ok(NodeOutput::goto(update, [target]))
                }
            }),
        );
        builder.may_goto(
            me,
            names
                .iter()
                .cloned()
                .chain(std::iter::once("finish".to_string())),
        );
    }

    if let Some(run) = dispatcher {
        let names_for_node = names.clone();
        builder.add_node(
            "dispatch",
            node_with_context(move |ctx, state: Arc<State>| {
                let (run, names) = (run.clone(), names_for_node.clone());
                async move {
                    let prompt = format!(
                        "Choose who should start. Options: {}.\nRequest: {}\nReply with one name.",
                        names.join(", "),
                        text_of(&state, "input")
                    );
                    let choice = run(ctx, prompt).await?.trim().to_string();
                    let target = if names.contains(&choice) {
                        choice.clone()
                    } else {
                        names[0].clone()
                    };
                    Ok(NodeOutput::goto(
                        Update::new().set("trace", entry("dispatcher", &choice)),
                        [target],
                    ))
                }
            }),
        );
        builder.may_goto("dispatch", names.iter().cloned());
        builder.add_edge("begin", "dispatch");
    } else {
        let target = first.clone();
        builder.add_node(
            "route_first",
            node_with_context(move |_ctx, _state: Arc<State>| {
                let target = target.clone();
                async move { Ok(NodeOutput::goto(Update::new(), [target])) }
            }),
        );
        builder.may_goto("route_first", [first]);
        builder.add_edge("begin", "route_first");
    }
    Pattern::new("swarm", builder, (cap as usize + 3) * 2 + 8)
}

/// Runs a step, then pauses for a person to look at its draft. Resume with
/// [`ReviewDecision::Approve`], [`ReviewDecision::Edit`] or
/// [`ReviewDecision::Reject`]. The pause is the suspend outcome of the
/// engine: through [`Pattern::thread`] it is durable, and another process
/// can resume it days later (`thread.resume([("review", decision)])`).
pub fn human_review(step: impl Into<Step>) -> Result<Pattern, PatternError> {
    let step = resolve(step.into(), 0);
    let name = unique(&[step.name])[0].clone();
    let mut builder = base_builder();
    builder.add_node(name.clone(), step_node(name.clone(), step.run));
    builder.add_node(
        "review",
        node_with_context(|ctx, state: Arc<State>| async move {
            let Some(decision) = ctx.resume() else {
                return Ok(NodeOutput::suspend(
                    Update::new(),
                    json!({
                        "draft": text_of(&state, "current"),
                        "question": "Approve, edit or reject this draft.",
                    }),
                ));
            };
            let kind = match decision {
                Value::String(kind) => Some(kind.as_str()),
                Value::Object(map) => map.get("decision").and_then(Value::as_str),
                _ => None,
            };
            let edited = decision.get("text").and_then(Value::as_str);
            let update = match (kind, edited) {
                (Some("approve"), _) => Update::new()
                    .set("stop_reason", "approved")
                    .set("trace", entry("person", "approved")),
                (Some("edit"), Some(text)) => Update::new()
                    .set("current", text)
                    .set("stop_reason", "approved")
                    .set("trace", entry("person", "edited")),
                (Some("reject"), _) => Update::new()
                    .set("current", "")
                    .set("stop_reason", "rejected")
                    .set("trace", entry("person", "rejected")),
                _ => {
                    return Err(NodeError::new(
                        "decision must be 'approve', {'decision': 'edit', 'text': ...} or {'decision': 'reject'}",
                    )
                    .with_kind("resume_invalid"));
                }
            };
            Ok(update.into())
        }),
    );
    builder
        .add_node("finish", finish(PatternStop::Done))
        .add_edge("begin", name.clone())
        .add_edge(name, "review")
        .add_edge("review", "finish")
        .add_edge("finish", END);
    Pattern::new("human_review", builder, 16)
}

/// A pattern from one line of registered specialist names (plug and play):
///
/// ```text
/// researcher > writer        one after the other
/// sentiment | keywords       at the same time
/// writer <-> critic          write, review, revise until approved
/// summarizer                 a single specialist
/// ```
///
/// One kind of connector per line (combine patterns with the functions
/// above). Every name must be registered ([`specialists::register`]); the
/// specialists are built around `model`.
pub fn quick(spec: &str, model: Arc<dyn Model>) -> Result<Pattern, PatternError> {
    if spec.trim().is_empty() {
        return Err(invalid("quick needs a line like 'researcher > writer'"));
    }
    let (mut parts, mut connectors) = (Vec::new(), Vec::new());
    let (mut start, mut i) = (0, 0);
    while i < spec.len() {
        let rest = &spec[i..];
        let connector = if rest.starts_with("<->") {
            Some("<->")
        } else if rest.starts_with('>') {
            Some(">")
        } else if rest.starts_with('|') {
            Some("|")
        } else {
            None
        };
        match connector {
            Some(c) => {
                parts.push(spec[start..i].trim());
                connectors.push(c);
                i += c.len();
                start = i;
            }
            None => i += rest.chars().next().map_or(1, char::len_utf8),
        }
    }
    parts.push(spec[start..].trim());
    let kinds: HashSet<&str> = connectors.iter().copied().collect();
    if kinds.len() > 1 {
        return Err(invalid(format!(
            "{spec:?}: use one kind of connector per line (>, | or <->)"
        )));
    }
    if parts.iter().any(|p| p.is_empty()) {
        return Err(invalid(format!("{spec:?}: a name is empty")));
    }
    let steps = parts
        .iter()
        .map(|name| Step::registered(name, model.clone()))
        .collect::<Result<Vec<_>, _>>()?;
    match kinds.into_iter().next().unwrap_or(">") {
        "<->" => {
            if steps.len() != 2 {
                return Err(invalid(format!(
                    "{spec:?}: <-> connects exactly two names (a writer and a reviewer)"
                )));
            }
            let mut steps = steps.into_iter();
            let (generator, critic) = (steps.next().expect("two"), steps.next().expect("two"));
            review_and_critique(generator, critic, 3)
        }
        "|" => parallel(steps, None),
        _ => sequential(steps),
    }
}

// ------------------------------------------------------------- agent patterns

const REACT: &str = "Work in a loop: think about what you need next, call a tool, read the result, and repeat. \
Stop and answer as soon as you can; if a tool result looks wrong, say so instead of building on it.";

/// A builder for a single agent in a reason-act loop with a hard cap on
/// model calls (`max_steps`, at most 50). The agent loop already thinks,
/// acts and observes; this sets the prompt and the limit. Add tools and
/// settings, then `build()`. (Setting [`AgentBuilder::limits`] afterwards
/// replaces the cap.)
pub fn react_builder(
    model: Arc<dyn Model>,
    max_steps: u32,
    instructions: Option<&str>,
) -> Result<AgentBuilder, PatternError> {
    let steps = cap(max_steps, "max_steps")?;
    let text = match instructions {
        Some(own) => format!("{}\n\n{REACT}", own.trim_end()),
        None => REACT.to_string(),
    };
    Ok(Agent::builder_shared(model)
        .instructions(text)
        .limits(Limits::default().model_calls(steps)))
}

/// [`react_builder`] with `tools`, built.
pub fn react(
    model: Arc<dyn Model>,
    tools: impl IntoIterator<Item = Arc<dyn Tool>>,
    max_steps: u32,
) -> Result<Agent, PatternError> {
    let mut builder = react_builder(model, max_steps, None)?;
    for tool in tools {
        builder = builder.shared_tool(tool);
    }
    Ok(builder.build()?)
}

/// What a [`coordinator`] can hand work to.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Member {
    /// An agent, with an optional description of when to use it.
    Agent {
        /// The agent.
        agent: Agent,
        /// When to use it (default: `Handles <name> requests`).
        description: Option<String>,
    },
    /// A recipe, built around the coordinator's model.
    Specialist(Specialist),
    /// A registered specialist, by name.
    Registered(String),
}

impl Member {
    /// An agent with a description of when to use it.
    pub fn described(agent: Agent, description: impl Into<String>) -> Self {
        Self::Agent {
            agent,
            description: Some(description.into()),
        }
    }
}

impl From<Agent> for Member {
    fn from(agent: Agent) -> Self {
        Self::Agent {
            agent,
            description: None,
        }
    }
}

impl From<Specialist> for Member {
    fn from(specialist: Specialist) -> Self {
        Self::Specialist(specialist)
    }
}

impl From<&str> for Member {
    fn from(name: &str) -> Self {
        Self::Registered(name.to_string())
    }
}

fn member_tool(
    name: &str,
    member: Member,
    model: &Arc<dyn Model>,
) -> Result<Arc<dyn Tool>, PatternError> {
    let (agent, description) = match member {
        Member::Agent { agent, description } => (agent, description),
        Member::Specialist(spec) => (
            spec.build(model.clone())?,
            Some(spec.description().to_string()),
        ),
        Member::Registered(registered) => {
            let spec = specialists::get(&registered)?;
            (
                spec.build(model.clone())?,
                Some(spec.description().to_string()),
            )
        }
    };
    let description = description.unwrap_or_else(|| format!("Handles {name} requests"));
    Ok(Arc::new(agent.as_tool(name, description)))
}

/// A builder for an agent that reads each request and hands it to the right
/// specialist, which it calls as a tool. `members` are `(name, member)`
/// pairs. A model decides the routing (more flexible than a fixed workflow,
/// and more model calls). Add settings, then `build()`.
pub fn coordinator_builder<I, N, M>(
    model: Arc<dyn Model>,
    members: I,
    instructions: Option<&str>,
) -> Result<AgentBuilder, PatternError>
where
    I: IntoIterator<Item = (N, M)>,
    N: Into<String>,
    M: Into<Member>,
{
    let mut tools = Vec::new();
    for (name, member) in members {
        tools.push(member_tool(&name.into(), member.into(), &model)?);
    }
    if tools.is_empty() {
        return Err(invalid("a coordinator needs at least one specialist"));
    }
    let roster = tools
        .iter()
        .map(|t| format!("- {}: {}", t.spec().name, t.spec().description))
        .collect::<Vec<_>>()
        .join("\n");
    let base = instructions
        .unwrap_or("You are a coordinator. Decide which specialist should handle the request.");
    let mut builder = Agent::builder_shared(model).instructions(format!(
        "{}\n\nSpecialists you can call (pass the request as input):\n{roster}\nCall the best one; if none fits, answer yourself briefly.",
        base.trim_end()
    ));
    for tool in tools {
        builder = builder.shared_tool(tool);
    }
    Ok(builder)
}

/// [`coordinator_builder`], built.
pub fn coordinator<I, N, M>(
    model: Arc<dyn Model>,
    members: I,
    instructions: Option<&str>,
) -> Result<Agent, PatternError>
where
    I: IntoIterator<Item = (N, M)>,
    N: Into<String>,
    M: Into<Member>,
{
    Ok(coordinator_builder(model, members, instructions)?.build()?)
}

/// A node of a [`hierarchical`] tree: a worker, or a team led by a
/// sub-coordinator.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Tree {
    /// A worker, as for [`coordinator`].
    Worker(Box<Member>),
    /// A sub-coordinator over its own members.
    Team {
        /// What the team does, for the level above (default: `The <name> team`).
        description: Option<String>,
        /// The team's members.
        members: Vec<(String, Tree)>,
    },
}

impl Tree {
    /// A team with a description.
    pub fn team<I, N, T>(description: impl Into<String>, members: I) -> Self
    where
        I: IntoIterator<Item = (N, T)>,
        N: Into<String>,
        T: Into<Tree>,
    {
        Self::Team {
            description: Some(description.into()),
            members: members
                .into_iter()
                .map(|(n, t)| (n.into(), t.into()))
                .collect(),
        }
    }
}

impl From<Member> for Tree {
    fn from(member: Member) -> Self {
        Self::Worker(Box::new(member))
    }
}

impl From<Agent> for Tree {
    fn from(agent: Agent) -> Self {
        Self::Worker(Box::new(agent.into()))
    }
}

impl From<Specialist> for Tree {
    fn from(specialist: Specialist) -> Self {
        Self::Worker(Box::new(specialist.into()))
    }
}

fn tree_members(
    model: &Arc<dyn Model>,
    level: Vec<(String, Tree)>,
) -> Result<Vec<(String, Member)>, PatternError> {
    level
        .into_iter()
        .map(|(name, node)| match node {
            Tree::Worker(member) => Ok((name, *member)),
            Tree::Team {
                description,
                members,
            } => {
                let inner = tree_members(model, members)?;
                let lead = format!("You lead the {name} team.");
                let agent = coordinator(model.clone(), inner, Some(&lead))?;
                let described = description.unwrap_or_else(|| format!("The {name} team"));
                Ok((name, Member::described(agent, described)))
            }
        })
        .collect()
}

/// A coordinator of coordinators: the root splits the work, each level hands
/// it further down, workers do it. `tree` is the root's members; a
/// [`Tree::Team`] becomes a sub-coordinator. Depth is bounded by
/// [`Limits::nesting`].
pub fn hierarchical<I, N, T>(
    model: Arc<dyn Model>,
    tree: I,
    instructions: Option<&str>,
) -> Result<Agent, PatternError>
where
    I: IntoIterator<Item = (N, T)>,
    N: Into<String>,
    T: Into<Tree>,
{
    let level = tree
        .into_iter()
        .map(|(n, t)| (n.into(), t.into()))
        .collect();
    let members = tree_members(&model, level)?;
    coordinator(model, members, instructions)
}
