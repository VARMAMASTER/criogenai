//! Tools: functions an agent's model may call (design 0002 §3–§5).

use std::future::Future;
use std::sync::Arc;

use agentbuild_core::{CancelToken, EventSink};
use agentbuild_providers::{BoxFuture, ContentBlock, ToolSpec};
use serde_json::{Map, Value};

/// A tool an agent can call. Most code uses [`tool_fn`].
///
/// The SDKs implement it by calling into Python or JavaScript.
pub trait Tool: Send + Sync + 'static {
    /// Name, description and argument schema.
    fn spec(&self) -> &ToolSpec;

    /// Runs one call. `args` were already validated against the schema.
    fn call(
        &self,
        args: Map<String, Value>,
        ctx: ToolContext,
    ) -> BoxFuture<'_, Result<ToolOutput, ToolError>>;
}

/// What a tool receives besides its arguments. Never part of the schema,
/// so the model cannot forge it (0002 §3.1).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ToolContext {
    /// The tool call's id.
    pub call_id: String,
    /// The run's id.
    pub run_id: String,
    pub(crate) cancel: CancelToken,
    pub(crate) events: EventSink,
    /// How deep this call is in agents calling agents (0 = top level).
    pub(crate) depth: u32,
}

impl ToolContext {
    /// How many agents are above the agent making this call (0 for a
    /// top-level agent).
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// Fires when the run is cancelled or the call times out.
    pub fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }

    /// Emits progress events (`custom` events with the tool's payload).
    pub fn events(&self) -> &EventSink {
        &self.events
    }
}

/// What a tool returns. Converted to the tool message's content
/// (0002 §4.2).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ToolOutput {
    /// One text block.
    Text(String),
    /// One JSON block.
    Json(Value),
    /// Content blocks as given (e.g. an image).
    Blocks(Vec<ContentBlock>),
}

impl ToolOutput {
    pub(crate) fn into_content(self) -> Vec<ContentBlock> {
        match self {
            Self::Text(text) if text.is_empty() => vec![ContentBlock::text("(no output)")],
            Self::Text(text) => vec![ContentBlock::text(text)],
            Self::Json(Value::Null) => vec![ContentBlock::text("(no output)")],
            Self::Json(value) => vec![ContentBlock::json(value)],
            Self::Blocks(blocks) if blocks.is_empty() => {
                vec![ContentBlock::text("(no output)")]
            }
            Self::Blocks(blocks) => blocks,
        }
    }

    /// The output as a JSON value (for `return_direct` results).
    pub(crate) fn to_value(&self) -> Value {
        match self {
            Self::Text(text) => Value::String(text.clone()),
            Self::Json(value) => value.clone(),
            Self::Blocks(blocks) => serde_json::to_value(blocks).unwrap_or(Value::Null),
        }
    }
}

impl From<String> for ToolOutput {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl From<&str> for ToolOutput {
    fn from(text: &str) -> Self {
        Self::Text(text.to_string())
    }
}

impl From<Value> for ToolOutput {
    fn from(value: Value) -> Self {
        Self::Json(value)
    }
}

impl From<Vec<ContentBlock>> for ToolOutput {
    fn from(blocks: Vec<ContentBlock>) -> Self {
        Self::Blocks(blocks)
    }
}

/// A tool failure.
///
/// [`ToolError::new`] is a message **for the model** (it becomes a tool
/// result with `status: error`, and the loop continues). [`ToolError::fatal`]
/// is a developer error: it fails the run, like an uncaught exception
/// (0003 §3).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
#[non_exhaustive]
pub struct ToolError {
    /// What went wrong.
    pub message: String,
    /// `true`: fail the run instead of telling the model.
    pub fatal: bool,
    /// Calling again may succeed (shown to the model).
    pub retryable: bool,
}

impl ToolError {
    /// An error the model sees and can react to.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            fatal: false,
            retryable: false,
        }
    }

    /// A developer error: the run fails (`tool_failed`).
    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            fatal: true,
            retryable: false,
        }
    }

    /// Marks the error as worth retrying.
    pub fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }
}

/// A tool from an async closure.
///
/// ```
/// use agentbuild_agent::{ToolError, tool_fn};
/// use agentbuild_providers::ToolSpec;
/// use serde_json::json;
///
/// let add = tool_fn(
///     ToolSpec::new("add", "Add two numbers", json!({
///         "type": "object",
///         "properties": {"a": {"type": "number"}, "b": {"type": "number"}},
///         "required": ["a", "b"]
///     })),
///     |args, _ctx| async move {
///         let a = args["a"].as_f64().unwrap_or(0.0);
///         let b = args["b"].as_f64().unwrap_or(0.0);
///         Ok::<_, ToolError>(json!(a + b))
///     },
/// );
/// ```
pub fn tool_fn<F, Fut, O>(spec: ToolSpec, f: F) -> impl Tool
where
    F: Fn(Map<String, Value>, ToolContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<O, ToolError>> + Send + 'static,
    O: Into<ToolOutput>,
{
    FnTool { spec, f }
}

struct FnTool<F> {
    spec: ToolSpec,
    f: F,
}

impl<F, Fut, O> Tool for FnTool<F>
where
    F: Fn(Map<String, Value>, ToolContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<O, ToolError>> + Send + 'static,
    O: Into<ToolOutput>,
{
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn call(
        &self,
        args: Map<String, Value>,
        ctx: ToolContext,
    ) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        let fut = (self.f)(args, ctx);
        Box::pin(async move { fut.await.map(Into::into) })
    }
}

impl<T: Tool + ?Sized> Tool for Arc<T> {
    fn spec(&self) -> &ToolSpec {
        (**self).spec()
    }

    fn call(
        &self,
        args: Map<String, Value>,
        ctx: ToolContext,
    ) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        (**self).call(args, ctx)
    }
}
