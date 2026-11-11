//! The model-provider layer of Criogen (design 0002, 0003 §4, 0006).
//!
//! This crate owns the language-neutral data contracts: [`Message`]s with
//! ordered [`ContentBlock`]s, [`ToolSpec`]s, the [`ModelRequest`] /
//! [`ModelResponse`] contract, and classified [`ModelError`]s. The [`Model`]
//! trait is what the agent runtime calls; [`FakeModel`] simulates one for
//! tests.
//!
//! **Status:** data types, error classification, transcript repair, the SSE
//! parser, the stream assembler, the fake model, the HTTP transport, three
//! wire protocols (OpenAI chat completions, OpenAI Responses and Anthropic
//! Messages) and
//! embedding models ([`Embedder`], [`FakeEmbedder`]) exist.
//! The gateway is planned (0006 §11).
//!
//! ```
//! use criogen_providers::{FakeModel, Message, Model, ModelRequest};
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

#[cfg(feature = "http")]
mod anthropic_messages;
#[cfg(feature = "http")]
mod azure;
#[cfg(feature = "http")]
mod bedrock;
mod embed;
mod error;
mod fake;
#[cfg(feature = "http")]
mod kaggle;
mod message;
mod model;
#[cfg(feature = "http")]
mod openai_chat;
#[cfg(feature = "http")]
mod openai_responses;
#[cfg(feature = "http")]
mod presets;
#[cfg(feature = "http")]
mod provider;
mod request;
mod sse;
mod stream;
#[cfg(feature = "http")]
mod think;
mod tool;
pub mod transcript;
#[cfg(feature = "http")]
mod transport;
#[cfg(feature = "http")]
mod vertex;

#[cfg(feature = "http")]
pub use anthropic_messages::AnthropicOptions;
#[cfg(feature = "http")]
pub use azure::{AzureApi, AzureCredential, AzureModel};
#[cfg(feature = "http")]
pub use bedrock::{BedrockApi, bedrock_endpoint, bedrock_from_env};
pub use embed::{Embedder, FAKE_DIMENSIONS, FakeEmbedder};
pub use error::{
    Dispatch, HttpFailure, MAX_MESSAGE_BYTES, ModelError, ModelErrorKind, classify_http,
    invalid_response, sanitize_message,
};
pub use fake::{FakeModel, FakeReply};
#[cfg(feature = "http")]
pub use kaggle::{KAGGLE_API_BASE, KaggleModel};
pub use message::{
    ContentBlock, Extras, FinishReason, MediaSource, Message, MessageIssue, MessageMetadata, Role,
    ToolCall, ToolResultError, ToolStatus, Usage, new_call_id, new_message_id,
};
pub use model::{BoxFuture, DeltaSink, Model};
#[cfg(feature = "http")]
pub use openai_chat::OpenAiQuirks;
#[cfg(feature = "http")]
pub use openai_responses::ResponsesOptions;
#[cfg(feature = "http")]
pub use presets::{PRESETS, Preset, preset};
#[cfg(feature = "http")]
pub use provider::{
    Endpoint, Protocol, ProviderEmbedder, ProviderModel, azure_base_url, preview_request,
};
pub use request::{
    Capabilities, ModelRequest, ModelResponse, OutputSpec, OutputStrategy, Reasoning,
    ReasoningEffort, Settings, ToolChoice,
};
pub use sse::{SseEvent, SseParser};
pub use stream::{Assembler, BlockDelta, MessageDelta, StreamItem, Warning};
pub use tool::{
    RESERVED_TOOL_NAMES, ToolAnnotations, ToolIssue, ToolKind, ToolSpec, validate_tools,
};
#[cfg(feature = "http")]
pub use transport::{Body, Exchange, HttpConfig, Transport, WireRequest};
#[cfg(feature = "http")]
pub use vertex::{VertexApi, VertexCredential, VertexModel};
