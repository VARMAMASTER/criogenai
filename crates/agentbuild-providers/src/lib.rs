//! The model-provider layer of AgentBuild (design 0002, 0003 §4, 0006).
//!
//! This crate owns the language-neutral data contracts: [`Message`]s with
//! ordered [`ContentBlock`]s, [`ToolSpec`]s, the [`ModelRequest`] /
//! [`ModelResponse`] contract, and classified [`ModelError`]s. The [`Model`]
//! trait is what the agent runtime calls; [`FakeModel`] simulates one for
//! tests.
//!
//! **Status:** data types, error classification, transcript repair, the SSE
//! parser, the stream assembler and the fake model exist. Wire protocols
//! (OpenAI-compatible, Anthropic) and the HTTP transport are planned
//! (0006 §11).
//!
//! ```
//! use agentbuild_providers::{FakeModel, Message, Model, ModelRequest};
//!
//! # tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap().block_on(async {
//! let model = FakeModel::new();
//! let request = ModelRequest::new(vec![Message::user("hello")]);
//! let response = model.invoke(&request).await.unwrap();
//! assert_eq!(response.message.text(), "[fake-llm] reply to: hello");
//! # });
//! ```

// `Result<ModelResponse, ModelError>` is the common return type. The `Ok`
// side (a whole message) is larger than `ModelError`, so boxing the error
// would not make the `Result` smaller; it would only add an allocation.
#![allow(clippy::result_large_err)]

// Compiles and runs the README's example with the doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

mod error;
mod fake;
mod message;
mod model;
mod request;
mod sse;
mod stream;
mod tool;
pub mod transcript;

pub use error::{
    Dispatch, HttpFailure, MAX_MESSAGE_BYTES, ModelError, ModelErrorKind, classify_http,
    invalid_response, sanitize_message,
};
pub use fake::{FakeModel, FakeReply};
pub use message::{
    ContentBlock, Extras, FinishReason, MediaSource, Message, MessageIssue, MessageMetadata, Role,
    ToolCall, ToolResultError, ToolStatus, Usage, new_call_id, new_message_id,
};
pub use model::{BoxFuture, Model};
pub use request::{
    Capabilities, ModelRequest, ModelResponse, OutputSpec, OutputStrategy, Reasoning,
    ReasoningEffort, Settings, ToolChoice,
};
pub use sse::{SseEvent, SseParser};
pub use stream::{Assembled, Assembler, BlockDelta, MessageDelta, StreamItem, Warning};
pub use tool::{
    RESERVED_TOOL_NAMES, ToolAnnotations, ToolIssue, ToolKind, ToolSpec, validate_tools,
};
