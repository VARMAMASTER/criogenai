//! Hooks (middleware) around the agent loop (design 0004 §3).
//!
//! Six optional methods. For `hooks = [A, B, C]`:
//!
//! - `on_start` and `before_model` run A → B → C;
//! - `after_model` and `on_end` run C → B → A;
//! - `wrap_model` and `wrap_tool` nest with A outermost.
//!
//! Hooks never become graph nodes, so adding one changes no snapshot,
//! event shape or step budget.

use std::collections::HashMap;
use std::sync::Arc;

use std::sync::atomic::{AtomicBool, Ordering};

use criogen_core::State;
use criogen_providers::{
    BoxFuture, Message, MessageDelta, ModelError, ModelRequest, ModelResponse,
};
use serde_json::{Map, Value};

use crate::resilience::{CallEnv, ChainError, ModelChain};
use crate::tool::{Tool, ToolContext, ToolError, ToolOutput};

/// What a hook sees: the run, its transcript and the agent's state.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HookContext {
    /// The run's id.
    pub run_id: String,
    /// The transcript so far (for `after_model`, including the new message).
    pub messages: Vec<Message>,
    /// Model calls made so far in this run.
    pub model_calls: u32,
    /// This hook's saved state (see [`HookAction::state`]), if any.
    pub hook_state: Option<Value>,
    /// The whole workflow state, read-only.
    pub state: Arc<State>,
    /// The run's metadata ([`RunOptions::metadata`](criogen_core::RunOptions::metadata)),
    /// e.g. a user or session id.
    pub metadata: Option<Value>,
}

/// What a hook wants to happen. The default does nothing.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct HookAction {
    /// Saves this hook's state (under `hooks.<name>`), visible to it next
    /// time as [`HookContext::hook_state`].
    pub state: Option<Value>,
    /// `before_model`: replaces the whole transcript (e.g. a summary), both
    /// for this model call and in the state.
    pub messages: Option<Vec<Message>>,
    /// `after_model`: replaces the model's new message (e.g. redacted).
    pub message: Option<Message>,
    /// Ends the run (stop reason `hook_end`), with this output. The first
    /// hook to end wins, in hook order.
    pub end: Option<Option<Value>>,
}

impl HookAction {
    /// Nothing to do.
    pub fn proceed() -> Self {
        Self::default()
    }

    /// Saves this hook's state.
    pub fn with_state(mut self, state: Value) -> Self {
        self.state = Some(state);
        self
    }

    /// Replaces the transcript (`before_model`).
    pub fn replace_messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = Some(messages);
        self
    }

    /// Replaces the new message (`after_model`).
    pub fn replace_message(mut self, message: Message) -> Self {
        self.message = Some(message);
        self
    }

    /// Ends the run with `output`.
    pub fn end(mut self, output: Option<Value>) -> Self {
        self.end = Some(output);
        self
    }
}

/// A hook failure: the run fails with `hook_failed` (a developer error).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
#[non_exhaustive]
pub struct HookError {
    /// What went wrong.
    pub message: String,
    /// A key unique to this failure, repeated in the run error's
    /// `details.cause_key`: the SDKs keep the exception that was thrown under
    /// it, so a run's error carries its own exception and no other run's.
    pub cause_key: Option<String>,
}

impl HookError {
    /// An error with this message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cause_key: None,
        }
    }

    /// Sets [`cause_key`](Self::cause_key).
    pub fn with_cause_key(mut self, key: impl Into<String>) -> Self {
        self.cause_key = Some(key.into());
        self
    }
}

/// How a run segment ended, for [`Hook::on_end`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    /// The agent finished.
    Completed,
    /// The run waits for approval.
    Suspended,
    /// The run failed (the code is the [`AgentError`](crate::AgentError) code).
    Failed,
}

/// One tool call going through [`Hook::wrap_tool`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ToolCallRequest {
    /// The call's id.
    pub call_id: String,
    /// The tool to run (a hook may change it).
    pub name: String,
    /// Its arguments (a hook may change them).
    pub args: Map<String, Value>,
}

/// Middleware around the agent loop. Every method is optional.
pub trait Hook: Send + Sync + 'static {
    /// Identifies the hook (its state key, error messages).
    fn name(&self) -> &str;

    /// Once per run segment (each `run`, `resume` or `retry`), before the
    /// workflow starts. Only `state` is used from the action.
    fn on_start<'a>(
        &'a self,
        ctx: &'a HookContext,
    ) -> BoxFuture<'a, Result<HookAction, HookError>> {
        let _ = ctx;
        Box::pin(async { Ok(HookAction::proceed()) })
    }

    /// Before each model call.
    fn before_model<'a>(
        &'a self,
        ctx: &'a HookContext,
    ) -> BoxFuture<'a, Result<HookAction, HookError>> {
        let _ = ctx;
        Box::pin(async { Ok(HookAction::proceed()) })
    }

    /// Around each model call. Call `next.call(request)` zero or more
    /// times; return the response to use. A `ModelError` from `next`
    /// passes through unchanged.
    fn wrap_model<'a>(
        &'a self,
        request: ModelRequest,
        next: ModelNext<'a>,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        Box::pin(async move { next.call(request).await })
    }

    /// After each model response, before routing. `ctx.messages` ends with
    /// the new message.
    fn after_model<'a>(
        &'a self,
        ctx: &'a HookContext,
    ) -> BoxFuture<'a, Result<HookAction, HookError>> {
        let _ = ctx;
        Box::pin(async { Ok(HookAction::proceed()) })
    }

    /// Around each tool call. Call `next.call(call)` zero or more times, or
    /// return a result without running the tool. A `ToolError` here is the
    /// tool's own error.
    fn wrap_tool<'a>(
        &'a self,
        call: ToolCallRequest,
        next: ToolNext<'a>,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move { next.call(call).await })
    }

    /// Once per run segment, always: completed, suspended or failed. It
    /// cannot change the outcome.
    fn on_end<'a>(&'a self, ctx: &'a HookContext, outcome: Outcome) -> BoxFuture<'a, ()> {
        let _ = (ctx, outcome);
        Box::pin(async {})
    }
}

/// The rest of the `wrap_model` chain.
#[derive(Clone)]
pub struct ModelNext<'a> {
    pub(crate) hooks: &'a [Arc<dyn Hook>],
    pub(crate) chain: &'a ModelChain,
    pub(crate) env: &'a CallEnv,
    pub(crate) on_delta: Arc<dyn Fn(MessageDelta) + Send + Sync>,
    /// Set when the chain stopped because of the run's deadline.
    pub(crate) deadline_hit: &'a AtomicBool,
}

impl std::fmt::Debug for ModelNext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelNext")
            .field("remaining_hooks", &self.hooks.len())
            .finish_non_exhaustive()
    }
}

impl<'a> ModelNext<'a> {
    /// Runs the remaining hooks and then the model.
    pub fn call(self, request: ModelRequest) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        match self.hooks.split_first() {
            Some((hook, rest)) => {
                let next = ModelNext {
                    hooks: rest,
                    chain: self.chain,
                    env: self.env,
                    on_delta: self.on_delta.clone(),
                    deadline_hit: self.deadline_hit,
                };
                hook.wrap_model(request, next)
            }
            // The fallback chain and retry loop sit inside the hooks
            // (0003 §5.3).
            None => Box::pin(async move {
                match self.chain.call(&request, self.env, &*self.on_delta).await {
                    Ok(response) => Ok(response),
                    Err(ChainError::Model(error)) => Err(*error),
                    Err(ChainError::Deadline(error)) => {
                        self.deadline_hit.store(true, Ordering::SeqCst);
                        Err(*error)
                    }
                }
            }),
        }
    }
}

/// The rest of the `wrap_tool` chain.
#[derive(Clone)]
pub struct ToolNext<'a> {
    pub(crate) hooks: &'a [Arc<dyn Hook>],
    pub(crate) tools: &'a HashMap<String, Arc<dyn Tool>>,
    pub(crate) ctx: ToolContext,
}

impl std::fmt::Debug for ToolNext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolNext")
            .field("remaining_hooks", &self.hooks.len())
            .finish_non_exhaustive()
    }
}

impl<'a> ToolNext<'a> {
    /// The context of the tool call going through the chain (its run, call
    /// id and the run's metadata).
    pub fn context(&self) -> &ToolContext {
        &self.ctx
    }

    /// Runs the remaining hooks and then the tool named in `call`.
    pub fn call(self, call: ToolCallRequest) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        match self.hooks.split_first() {
            Some((hook, rest)) => {
                let next = ToolNext {
                    hooks: rest,
                    tools: self.tools,
                    ctx: self.ctx.clone(),
                };
                hook.wrap_tool(call, next)
            }
            None => match self.tools.get(&call.name) {
                Some(tool) => tool.call(call.args, self.ctx),
                None => {
                    let name = call.name;
                    Box::pin(async move { Err(ToolError::new(format!("Unknown tool `{name}`"))) })
                }
            },
        }
    }
}
