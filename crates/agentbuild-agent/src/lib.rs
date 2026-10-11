//! Agents for AgentBuild: a model with tools, run as a loop on the workflow
//! engine (design 0004).
//!
//! An [`Agent`] compiles into an ordinary [`agentbuild_core::Workflow`] with
//! two nodes: `model` calls the model; `tools` runs the tool calls of the
//! model's last message, concurrently, and appends the results in call order.
//! The loop ends when the model answers without calling a tool.
//!
//! - Tool arguments are validated against the tool's JSON Schema before the
//!   tool runs; problems go back to the model as `invalid_arguments`.
//! - Unknown tools, `ToolError`s and timeouts become error results the model
//!   can react to; [`ToolError::fatal`] fails the run.
//! - Every finished call is saved at once in the task memo: if the step
//!   fails, [`Agent::retry`] does not run those calls again.
//! - [`Limits`] bound model calls and tool calls; transient model errors are
//!   retried with backoff.
//!
//! - [`Approval`] gates tool calls: the run suspends before any call of that
//!   turn runs, and [`Agent::resume`] continues with each [`Decision`]
//!   (approve, edit, reject, respond).
//!
//! - [`AgentBuilder::output`] makes the run end with JSON matching a schema,
//!   through the model's JSON mode or a reserved `final_output` tool;
//!   invalid answers are sent back with the validation errors.
//!
//! - [`Hook`]s run around the loop (`on_start`, `before_model`,
//!   `wrap_model`, `after_model`, `wrap_tool`, `on_end`) without changing
//!   the graph.
//!
//! **Status:** the loop, tools, limits, human approval, structured output
//! and hooks exist (design 0004).
//!
//! ```
//! use agentbuild_agent::{Agent, ToolError, tool_fn};
//! use agentbuild_providers::{FakeModel, FakeReply, ToolSpec};
//! use serde_json::json;
//!
//! # tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap().block_on(async {
//! let model = FakeModel::new()
//!     .then(FakeReply::tool_call("add", json!({"a": 2, "b": 3})))
//!     .then(FakeReply::text("2 + 3 = 5"));
//! let add = tool_fn(
//!     ToolSpec::new("add", "Add two numbers", json!({
//!         "type": "object",
//!         "properties": {"a": {"type": "number"}, "b": {"type": "number"}},
//!         "required": ["a", "b"]
//!     })),
//!     |args, _ctx| async move {
//!         Ok::<_, ToolError>(json!(args["a"].as_f64().unwrap() + args["b"].as_f64().unwrap()))
//!     },
//! );
//! let agent = Agent::builder(model).tool(add).build().unwrap();
//! let result = agent.run("What is 2 + 3?").await.unwrap();
//! assert_eq!(result.text(), Some("2 + 3 = 5"));
//! assert_eq!((result.model_calls, result.tool_calls), (2, 1));
//! # });
//! ```

// Compiles and runs the README's example with the doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

mod agent;
mod approval;
mod hooks;
mod nested;
mod output;
mod resilience;
mod schema;
mod tool;

pub use agent::{
    AGENT, Agent, AgentBuildError, AgentBuilder, AgentError, AgentInput, AgentResult, HOOKS,
    Limits, MESSAGES, OnLimit, StopReason,
};
pub use approval::{
    APPROVAL_INTERRUPT, Approval, ApprovalRequest, ApprovalRule, Decision, DecisionKind,
};
pub use hooks::{
    Hook, HookAction, HookContext, HookError, ModelNext, Outcome, ToolCallRequest, ToolNext,
};
pub use nested::{AGENT_APPROVAL_INTERRUPT, AgentNode, AgentTool};
pub use output::FINAL_OUTPUT_TOOL;
pub use schema::validate as validate_arguments;
pub use tool::{Tool, ToolContext, ToolError, ToolOutput, tool_fn};
