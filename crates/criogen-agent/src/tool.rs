//! Tools: functions an agent's model may call (design 0002 §3–§5).

use std::any::Any;
use std::future::Future;
use std::sync::Arc;

use criogen_core::{CancelToken, EventSink};
use criogen_providers::{BoxFuture, ContentBlock, ToolSpec};
use criogen_storage::ScopedMemory;
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
    pub(crate) app: Option<Arc<dyn Any + Send + Sync>>,
    pub(crate) memory: Option<ScopedMemory>,
    pub(crate) metadata: Option<Arc<Value>>,
}

impl ToolContext {
    /// The run's [`RunOptions::metadata`](criogen_core::RunOptions::metadata),
    /// if any (who started it, for whom; SDK bridges keep their own keys
    /// there).
    pub fn metadata(&self) -> Option<&Value> {
        self.metadata.as_deref()
    }

    /// How many agents are above the agent making this call (0 for a
    /// top-level agent).
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// The app services given to [`AgentBuilder::context`](crate::AgentBuilder::context)
    /// (a database client, an HTTP client, settings), if they are a `T`.
    pub fn context<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.app.as_deref().and_then(|app| app.downcast_ref::<T>())
    }

    /// The run's long-term memory (its namespace only), if the agent has
    /// [`Memory`](crate::Memory).
    pub fn memory(&self) -> Option<&ScopedMemory> {
        self.memory.as_ref()
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
    /// Content plus app-only data and state changes. Build it with
    /// [`with_artifact`](Self::with_artifact) /
    /// [`with_state`](Self::with_state) on any output.
    #[non_exhaustive]
    Rich {
        /// What the model sees.
        content: Vec<ContentBlock>,
        /// Kept in the tool message, never sent to the model.
        artifact: Option<Value>,
        /// State changes, merged in tool-call order (0002 §4.4).
        state: Map<String, Value>,
        /// The output as a value (for `return_direct`).
        value: Value,
    },
}

impl ToolOutput {
    /// Adds app-only data (stored in the tool message's `artifact`, never
    /// shown to the model).
    pub fn with_artifact(self, artifact: Value) -> Self {
        match self.into_rich() {
            Self::Rich {
                content,
                state,
                value,
                ..
            } => Self::Rich {
                content,
                artifact: Some(artifact),
                state,
                value,
            },
            _ => unreachable!("into_rich returns Rich"),
        }
    }

    /// Adds a state change: `key` is set to `value` when the tools step
    /// commits. Lists written to the same key by parallel calls are joined
    /// in call order; any other overlap is an `update_conflict`.
    pub fn with_state(self, key: impl Into<String>, value: Value) -> Self {
        match self.into_rich() {
            Self::Rich {
                content,
                artifact,
                mut state,
                value: output,
            } => {
                state.insert(key.into(), value);
                Self::Rich {
                    content,
                    artifact,
                    state,
                    value: output,
                }
            }
            _ => unreachable!("into_rich returns Rich"),
        }
    }

    fn into_rich(self) -> Self {
        match self {
            rich @ Self::Rich { .. } => rich,
            other => {
                let value = other.to_value();
                Self::Rich {
                    content: other.into_content(),
                    artifact: None,
                    state: Map::new(),
                    value,
                }
            }
        }
    }

    /// `(content, artifact, state, value)`.
    pub(crate) fn into_parts(
        self,
    ) -> (Vec<ContentBlock>, Option<Value>, Map<String, Value>, Value) {
        match self {
            Self::Rich {
                content,
                artifact,
                state,
                value,
            } => (content, artifact, state, value),
            other => {
                let value = other.to_value();
                (other.into_content(), None, Map::new(), value)
            }
        }
    }

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
            Self::Rich { content, .. } if content.is_empty() => {
                vec![ContentBlock::text("(no output)")]
            }
            Self::Rich { content, .. } => content,
        }
    }

    /// The output as a JSON value (for `return_direct` results).
    pub(crate) fn to_value(&self) -> Value {
        match self {
            Self::Text(text) => Value::String(text.clone()),
            Self::Json(value) => value.clone(),
            Self::Blocks(blocks) => serde_json::to_value(blocks).unwrap_or(Value::Null),
            Self::Rich { value, .. } => value.clone(),
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
/// is a developer error: it fails the run (0003 §3).
/// [`ToolError::crashed`] is a failure the tool did not handle (an exception
/// in Python or JavaScript, a panic in Rust): the agent's
/// [`Limits::on_tool_exception`](crate::Limits::on_tool_exception) decides
/// (by default, the model sees it and the run goes on).
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
    /// `true`: the tool did not handle this failure (an exception or a
    /// panic); `on_tool_exception` decides what happens.
    pub crashed: bool,
    /// `true`: the arguments were wrong; the model sees an
    /// `invalid_arguments` error, as for arguments that fail the schema.
    pub invalid_arguments: bool,
}

impl ToolError {
    /// An error the model sees and can react to.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            fatal: false,
            retryable: false,
            crashed: false,
            invalid_arguments: false,
        }
    }

    /// Arguments the tool's own checks reject (e.g. a rule the JSON Schema
    /// cannot express): the model sees `invalid_arguments` with `problems`
    /// and may correct the call.
    pub fn invalid_arguments(problems: impl Into<String>) -> Self {
        Self {
            invalid_arguments: true,
            ..Self::new(problems)
        }
    }

    /// A developer error: the run fails (`tool_failed`).
    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            fatal: true,
            ..Self::new("")
        }
    }

    /// A failure the tool did not handle (an uncaught exception, a panic).
    /// `message` is the exception's type and text.
    pub fn crashed(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            crashed: true,
            ..Self::new("")
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
/// use criogen_agent::{ToolError, tool_fn};
/// use criogen_providers::ToolSpec;
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
