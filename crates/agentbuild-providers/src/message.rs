//! Messages and content blocks (design 0002 §1, §2, §4).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Provider-specific fields, keyed by provider name (for example
/// `{"anthropic": {"cache_control": {"type": "ephemeral"}}}`). Kept so a
/// conversation round-trips losslessly to the same provider.
pub type Extras = BTreeMap<String, Value>;

/// Who wrote a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Instructions for the model.
    System,
    /// The person (or app) talking to the model.
    User,
    /// The model.
    Assistant,
    /// The result of a tool call.
    Tool,
}

impl Role {
    /// The role's JSON name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

/// One message of a conversation. The same JSON in every language.
///
/// Tool calls live in an assistant message's `content` as
/// [`ContentBlock::ToolCall`] blocks, in order; [`tool_calls`](Self::tool_calls)
/// reads them. A tool message (`role: tool`) also sets `call_id`, `tool` and
/// `status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Message {
    /// Unique within a conversation. Generated when absent from JSON.
    #[serde(default = "new_message_id")]
    pub id: String,
    /// Who wrote it.
    pub role: Role,
    /// Ordered content.
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    /// Optional participant or agent name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Tool messages: the `id` of the tool call this answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// Tool messages: the tool's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Tool messages: how the call ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ToolStatus>,
    /// Tool messages with `status: error`: what went wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ToolResultError>,
    /// Tool messages: app-only data, never sent to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<Value>,
    /// Never sent to the model.
    #[serde(default, skip_serializing_if = "MessageMetadata::is_empty")]
    pub metadata: MessageMetadata,
}

/// A new message id: `msg_` followed by a time-ordered UUID.
pub fn new_message_id() -> String {
    format!("msg_{}", uuid::Uuid::now_v7().simple())
}

/// A new tool call id, for providers that omit one (0002 §7.1).
pub fn new_call_id() -> String {
    format!("call_{}", uuid::Uuid::now_v7().simple())
}

impl Message {
    fn new(role: Role, content: Vec<ContentBlock>) -> Self {
        Self {
            id: new_message_id(),
            role,
            content,
            name: None,
            call_id: None,
            tool: None,
            status: None,
            error: None,
            artifact: None,
            metadata: MessageMetadata::default(),
        }
    }

    /// A system message with one text block.
    pub fn system(text: impl Into<String>) -> Self {
        Self::new(Role::System, vec![ContentBlock::text(text)])
    }

    /// A user message with one text block.
    pub fn user(text: impl Into<String>) -> Self {
        Self::new(Role::User, vec![ContentBlock::text(text)])
    }

    /// An assistant message with one text block.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::new(Role::Assistant, vec![ContentBlock::text(text)])
    }

    /// A message with any role and content.
    pub fn with_content(role: Role, content: Vec<ContentBlock>) -> Self {
        Self::new(role, content)
    }

    /// A tool message answering `call_id`.
    pub fn tool_result(
        call_id: impl Into<String>,
        tool: impl Into<String>,
        status: ToolStatus,
        content: Vec<ContentBlock>,
    ) -> Self {
        let mut message = Self::new(Role::Tool, content);
        message.call_id = Some(call_id.into());
        message.tool = Some(tool.into());
        message.status = Some(status);
        message
    }

    /// A tool message with `status: error`. The model sees `Error: <message>`.
    pub fn tool_error(
        call_id: impl Into<String>,
        tool: impl Into<String>,
        error: ToolResultError,
    ) -> Self {
        let text = format!("Error: {}", error.message);
        let mut message = Self::tool_result(
            call_id,
            tool,
            ToolStatus::Error,
            vec![ContentBlock::text(text)],
        );
        message.error = Some(error);
        message
    }

    /// Sets the participant name.
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the id (useful for deterministic tests and upserts).
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    /// The tool calls in this message, in order.
    pub fn tool_calls(&self) -> impl Iterator<Item = ToolCall<'_>> {
        self.content.iter().filter_map(|block| match block {
            ContentBlock::ToolCall { id, name, args, .. } => Some(ToolCall { id, name, args }),
            _ => None,
        })
    }

    /// Ids of every tool call and invalid tool call, in order. Each needs a
    /// tool message (0002 §6).
    pub fn call_ids(&self) -> impl Iterator<Item = &str> {
        self.content.iter().filter_map(|block| match block {
            ContentBlock::ToolCall { id, .. } | ContentBlock::InvalidToolCall { id, .. } => {
                Some(id.as_str())
            }
            _ => None,
        })
    }

    /// The text blocks joined together (no separator).
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Checks the rules a message must follow on its own (role/block table
    /// of 0002 §1, tool-message fields of §4). Transcript-level rules are in
    /// [`crate::transcript`].
    pub fn validate(&self) -> Result<(), MessageIssue> {
        let issue = |reason: String| {
            Err(MessageIssue {
                id: self.id.clone(),
                reason,
            })
        };
        if self.id.is_empty() {
            return issue("message id must not be empty".into());
        }
        for block in &self.content {
            if !block.allowed_in(self.role) {
                return issue(format!(
                    "a `{}` block is not allowed in a {} message",
                    block.type_name(),
                    self.role.as_str()
                ));
            }
            if let ContentBlock::ToolCall { id, name, .. } = block
                && (id.is_empty() || name.is_empty())
            {
                return issue("tool_call needs a non-empty `id` and `name`".into());
            }
        }
        let tool_fields = self.call_id.is_some() || self.tool.is_some() || self.status.is_some();
        match self.role {
            Role::Tool => {
                if self.call_id.as_deref().is_none_or(str::is_empty) {
                    return issue("tool message needs `call_id`".into());
                }
                if self.tool.as_deref().is_none_or(str::is_empty) {
                    return issue("tool message needs `tool`".into());
                }
                let Some(status) = self.status else {
                    return issue("tool message needs `status`".into());
                };
                if (status == ToolStatus::Error) != self.error.is_some() {
                    return issue("`error` is set exactly when `status` is `error`".into());
                }
            }
            _ if tool_fields || self.error.is_some() || self.artifact.is_some() => {
                return issue(format!(
                    "only tool messages have call_id/tool/status/error/artifact, not {}",
                    self.role.as_str()
                ));
            }
            _ => {}
        }
        Ok(())
    }
}

/// A borrowed view of a [`ContentBlock::ToolCall`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToolCall<'a> {
    /// Call id; the tool message answering it carries the same `call_id`.
    pub id: &'a str,
    /// Tool name.
    pub name: &'a str,
    /// Arguments (always a JSON object).
    pub args: &'a Map<String, Value>,
}

/// Why a message is invalid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("message `{id}`: {reason}")]
#[non_exhaustive]
pub struct MessageIssue {
    /// The message.
    pub id: String,
    /// What is wrong.
    pub reason: String,
}

/// One piece of a message's content (0002 §2).
///
/// Every block may carry [`Extras`]. Build blocks with the constructors
/// ([`text`](Self::text), [`tool_call`](Self::tool_call), ...); variants are
/// `#[non_exhaustive]` so fields can be added.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContentBlock {
    /// Plain text.
    #[non_exhaustive]
    Text {
        /// The text.
        text: String,
        /// Provider-specific fields.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        extras: Extras,
    },
    /// Model reasoning. Opaque fields must round-trip unchanged to the same
    /// provider.
    #[non_exhaustive]
    Reasoning {
        /// Readable reasoning, if the provider returns it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// Provider signature that must be sent back unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        /// Encrypted reasoning the provider did not reveal (e.g. Anthropic
        /// `redacted_thinking.data`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redacted: Option<String>,
        /// Provider-specific fields.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        extras: Extras,
    },
    /// An image.
    #[non_exhaustive]
    Image {
        /// Where the bytes are.
        source: MediaSource,
        /// For example `image/png`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        /// Provider-specific fields.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        extras: Extras,
    },
    /// Audio.
    #[non_exhaustive]
    Audio {
        /// Where the bytes are.
        source: MediaSource,
        /// For example `audio/wav`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        /// Provider-specific fields.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        extras: Extras,
    },
    /// A document or other file.
    #[non_exhaustive]
    File {
        /// Where the bytes are.
        source: MediaSource,
        /// For example `application/pdf`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        /// Provider-specific fields.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        extras: Extras,
    },
    /// Structured data, mainly in tool results.
    #[non_exhaustive]
    Json {
        /// The data.
        value: Value,
        /// Provider-specific fields.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        extras: Extras,
    },
    /// The model asks to call a tool. Assistant messages only.
    #[non_exhaustive]
    ToolCall {
        /// Call id.
        id: String,
        /// Tool name.
        name: String,
        /// Arguments: always a JSON object.
        args: Map<String, Value>,
        /// Provider-specific fields.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        extras: Extras,
    },
    /// The model tried to call a tool but its arguments could not be parsed.
    #[non_exhaustive]
    InvalidToolCall {
        /// Call id.
        id: String,
        /// Tool name, if it was readable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// The arguments exactly as received.
        raw_args: String,
        /// Why parsing failed (e.g. `"truncated"`).
        error: String,
        /// Provider-specific fields.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        extras: Extras,
    },
    /// A block AgentBuild does not model (e.g. a provider's server-side tool
    /// use). Sent back only to the same provider.
    #[non_exhaustive]
    Provider {
        /// Provider name, e.g. `anthropic`.
        provider: String,
        /// The block as the provider sent it.
        data: Value,
    },
}

impl ContentBlock {
    /// A text block.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            extras: Extras::new(),
        }
    }

    /// A reasoning block with readable text.
    pub fn reasoning(text: impl Into<String>) -> Self {
        Self::Reasoning {
            text: Some(text.into()),
            signature: None,
            redacted: None,
            extras: Extras::new(),
        }
    }

    /// A JSON block.
    pub fn json(value: impl Into<Value>) -> Self {
        Self::Json {
            value: value.into(),
            extras: Extras::new(),
        }
    }

    /// An image block.
    pub fn image(source: MediaSource, mime_type: Option<String>) -> Self {
        Self::Image {
            source,
            mime_type,
            extras: Extras::new(),
        }
    }

    /// A tool call block.
    pub fn tool_call(
        id: impl Into<String>,
        name: impl Into<String>,
        args: Map<String, Value>,
    ) -> Self {
        Self::ToolCall {
            id: id.into(),
            name: name.into(),
            args,
            extras: Extras::new(),
        }
    }

    /// An invalid tool call block.
    pub fn invalid_tool_call(
        id: impl Into<String>,
        name: Option<String>,
        raw_args: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self::InvalidToolCall {
            id: id.into(),
            name,
            raw_args: raw_args.into(),
            error: error.into(),
            extras: Extras::new(),
        }
    }

    /// A provider-specific block.
    pub fn provider(provider: impl Into<String>, data: Value) -> Self {
        Self::Provider {
            provider: provider.into(),
            data,
        }
    }

    /// The block's JSON `type`.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Reasoning { .. } => "reasoning",
            Self::Image { .. } => "image",
            Self::Audio { .. } => "audio",
            Self::File { .. } => "file",
            Self::Json { .. } => "json",
            Self::ToolCall { .. } => "tool_call",
            Self::InvalidToolCall { .. } => "invalid_tool_call",
            Self::Provider { .. } => "provider",
        }
    }

    /// The role/block table of 0002 §1.
    pub fn allowed_in(&self, role: Role) -> bool {
        match role {
            Role::System => matches!(self, Self::Text { .. }),
            Role::User => matches!(
                self,
                Self::Text { .. } | Self::Image { .. } | Self::Audio { .. } | Self::File { .. }
            ),
            Role::Assistant => matches!(
                self,
                Self::Text { .. }
                    | Self::Reasoning { .. }
                    | Self::ToolCall { .. }
                    | Self::InvalidToolCall { .. }
                    | Self::Provider { .. }
            ),
            Role::Tool => matches!(
                self,
                Self::Text { .. } | Self::Image { .. } | Self::File { .. } | Self::Json { .. }
            ),
        }
    }
}

/// Where a media block's bytes are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MediaSource {
    /// A URL the provider fetches.
    Url {
        /// The URL.
        url: String,
    },
    /// Inline bytes.
    Base64 {
        /// Base64-encoded bytes.
        data: String,
    },
    /// A file previously uploaded to the provider.
    FileId {
        /// The provider's file id.
        file_id: String,
    },
}

/// How a tool call ended (0002 §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolStatus {
    /// The tool returned normally.
    Success,
    /// A failure the model can react to (bad arguments, unknown tool,
    /// `ToolError`, timeout after retries).
    Error,
    /// A human rejected the call.
    Rejected,
    /// Not executed because of a policy.
    Skipped,
    /// The run was cancelled while the call was in flight.
    Cancelled,
}

/// The error in a tool message with `status: error`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ToolResultError {
    /// `unknown_tool`, `invalid_arguments`, `tool_error`, `timeout` or
    /// `transient`.
    pub code: String,
    /// What the model is told.
    pub message: String,
    /// Whether calling again may succeed.
    #[serde(default)]
    pub retryable: bool,
}

impl ToolResultError {
    /// A non-retryable error.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }
}

/// Token counts. `input_tokens` counts all input processed, including cache
/// reads and writes; the `cache_*` fields are subsets of it (0002 §1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Usage {
    /// All input tokens.
    pub input_tokens: u64,
    /// Output tokens, including reasoning.
    pub output_tokens: u64,
    /// Input tokens read from the provider's cache.
    pub cache_read_tokens: u64,
    /// Input tokens written to the provider's cache.
    pub cache_write_tokens: u64,
    /// Output tokens spent on reasoning.
    pub reasoning_tokens: u64,
}

impl Usage {
    /// Input and output tokens.
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            ..Self::default()
        }
    }

    /// Field-wise sum, saturating.
    pub fn add(&self, other: &Usage) -> Usage {
        Usage {
            input_tokens: self.input_tokens.saturating_add(other.input_tokens),
            output_tokens: self.output_tokens.saturating_add(other.output_tokens),
            cache_read_tokens: self
                .cache_read_tokens
                .saturating_add(other.cache_read_tokens),
            cache_write_tokens: self
                .cache_write_tokens
                .saturating_add(other.cache_write_tokens),
            reasoning_tokens: self.reasoning_tokens.saturating_add(other.reasoning_tokens),
        }
    }

    fn is_zero(&self) -> bool {
        *self == Usage::default()
    }
}

/// Why the model stopped (normalized; the vendor's value is kept in
/// [`MessageMetadata::raw_finish_reason`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FinishReason {
    /// Natural end of the answer.
    Stop,
    /// The model wants tools to run.
    ToolCalls,
    /// The output token limit was reached.
    Length,
    /// The model declined in its answer (not a request error).
    ContentFilter,
    /// The provider reported an error in the response.
    Error,
    /// Anything else.
    Other,
}

/// Message information that is never sent to the model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MessageMetadata {
    /// Model that produced the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Provider that produced the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Tokens used for the message.
    #[serde(default, skip_serializing_if = "Usage::is_zero")]
    pub usage: Usage,
    /// Normalized finish reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
    /// The provider's own finish reason, e.g. `end_turn`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_finish_reason: Option<String>,
    /// RFC 3339 time the message was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Tool messages: execution time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Tool messages: attempts made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<u32>,
    /// Tool messages: whether the content was cut (0002 §4.3).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Any other fields, kept as they are.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

impl MessageMetadata {
    fn is_empty(&self) -> bool {
        *self == MessageMetadata::default()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn json_shape_matches_the_design() {
        let mut args = Map::new();
        args.insert("query".into(), json!("rust"));
        let message = Message::with_content(
            Role::Assistant,
            vec![
                ContentBlock::text("Searching."),
                ContentBlock::tool_call("call_1", "search", args),
            ],
        )
        .with_id("msg_1");
        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            json!({
                "id": "msg_1",
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "Searching."},
                    {"type": "tool_call", "id": "call_1", "name": "search", "args": {"query": "rust"}}
                ]
            })
        );
        let calls: Vec<_> = message.tool_calls().map(|c| (c.id, c.name)).collect();
        assert_eq!(calls, [("call_1", "search")]);
    }

    #[test]
    fn missing_id_is_generated_and_unknown_metadata_is_kept() {
        let message: Message = serde_json::from_value(json!({
            "role": "user",
            "content": [{"type": "text", "text": "hi"}],
            "metadata": {"trace": "abc", "usage": {"input_tokens": 3}}
        }))
        .unwrap();
        assert!(message.id.starts_with("msg_"));
        assert_eq!(message.metadata.other["trace"], "abc");
        assert_eq!(message.metadata.usage.input_tokens, 3);
        let back = serde_json::to_value(&message).unwrap();
        assert_eq!(back["metadata"]["trace"], "abc");
    }

    #[test]
    fn tool_call_args_must_be_an_object() {
        let err = serde_json::from_value::<ContentBlock>(json!({
            "type": "tool_call", "id": "c", "name": "t", "args": [1, 2]
        }));
        assert!(err.is_err());
    }

    #[test]
    fn extras_round_trip() {
        let value = json!({
            "type": "text", "text": "cached",
            "extras": {"anthropic": {"cache_control": {"type": "ephemeral"}}}
        });
        let block: ContentBlock = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&block).unwrap(), value);
    }

    #[test]
    fn validation_enforces_the_role_table_and_tool_fields() {
        assert!(Message::user("hi").validate().is_ok());
        let bad = Message::with_content(Role::User, vec![ContentBlock::reasoning("hmm")]);
        assert!(bad.validate().unwrap_err().reason.contains("reasoning"));

        let ok = Message::tool_result("c1", "search", ToolStatus::Success, vec![]);
        assert!(ok.validate().is_ok());
        let err = Message::tool_error("c1", "search", ToolResultError::new("tool_error", "x"));
        assert!(err.validate().is_ok());
        assert_eq!(err.text(), "Error: x");

        let mut missing_error = err.clone();
        missing_error.error = None;
        assert!(missing_error.validate().is_err());
        let mut no_call = ok.clone();
        no_call.call_id = None;
        assert!(no_call.validate().is_err());
        let mut user_with_status = Message::user("hi");
        user_with_status.status = Some(ToolStatus::Success);
        assert!(user_with_status.validate().is_err());
    }

    #[test]
    fn usage_adds_saturating() {
        let a = Usage::new(u64::MAX, 1);
        let b = Usage::new(5, 2);
        assert_eq!(a.add(&b), Usage::new(u64::MAX, 3));
    }
}
