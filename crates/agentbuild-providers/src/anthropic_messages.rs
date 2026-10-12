//! The Anthropic Messages protocol (design 0006 §4, research
//! litellm-providers §10.6, §10.7): Anthropic's API and servers that speak
//! its format.
//!
//! Blocks map one to one and in order, so reasoning signatures, citations,
//! `cache_control` and hosted-tool blocks survive a round trip. Pure
//! translation: no I/O, clock or environment.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::error::{HttpFailure, ModelError, ModelErrorKind, classify_http, invalid_response};
use crate::message::{
    ContentBlock, Extras, FinishReason, MediaSource, Message, Role, ToolStatus, Usage,
};
use crate::request::{
    ModelRequest, ModelResponse, OutputStrategy, Reasoning, ReasoningEffort, ToolChoice,
};
use crate::sse::SseEvent;
use crate::stream::{Assembler, BlockDelta, StreamItem, Warning};
use crate::tool::ToolKind;

/// Name used for `provider` blocks and the `extras` key of this protocol.
pub(crate) const PROVIDER: &str = "anthropic";

/// Settings of the Anthropic protocol for one endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct AnthropicOptions {
    /// The `anthropic-version` header.
    pub version: String,
    /// `max_tokens` when the request sets none (the API requires one).
    /// With reasoning on, room for the reasoning is added to it.
    pub default_max_tokens: u32,
    /// `anthropic-beta` flags sent with every request.
    pub betas: Vec<String>,
}

impl Default for AnthropicOptions {
    fn default() -> Self {
        Self {
            version: "2023-06-01".into(),
            default_max_tokens: 8192,
            betas: Vec::new(),
        }
    }
}

impl AnthropicOptions {
    /// Sets the `anthropic-version` header.
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// Sets [`default_max_tokens`](Self::default_max_tokens).
    pub fn default_max_tokens(mut self, max_tokens: u32) -> Self {
        self.default_max_tokens = max_tokens;
        self
    }

    /// Adds an `anthropic-beta` flag.
    pub fn beta(mut self, flag: impl Into<String>) -> Self {
        self.betas.push(flag.into());
        self
    }
}

/// An encoded request: the body plus the beta flags it needs.
#[derive(Debug)]
pub(crate) struct WireBody {
    pub(crate) body: Value,
    pub(crate) betas: Vec<String>,
}

// ---------- encode ----------

/// Output room added to the default `max_tokens` for an effort level
/// (reasoning counts as output). Our own tiers.
fn effort_room(effort: ReasoningEffort) -> u32 {
    match effort {
        ReasoningEffort::Low => 2_048,
        ReasoningEffort::Medium => 8_192,
        ReasoningEffort::High => 24_576,
    }
}

fn effort_name(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
    }
}

/// Builds the JSON body for `request`. Anything the protocol cannot carry is
/// dropped with a warning, or rejected as `invalid_request` before sending.
pub(crate) fn encode(
    request: &ModelRequest,
    model: &str,
    provider: &str,
    options: &AnthropicOptions,
    stream: bool,
    warnings: &mut Vec<Warning>,
) -> Result<WireBody, ModelError> {
    let mut betas = options.betas.clone();
    let mut system = Vec::new();
    let mut turns: Vec<(Role, Vec<Value>)> = Vec::new();
    let mut reasoning_dropped = 0;

    for message in &request.messages {
        match message.role {
            Role::System => {
                if !turns.is_empty() {
                    return Err(invalid_request(format!(
                        "system message {} comes after the conversation started; \
                         this protocol only takes system messages at the start",
                        message.id
                    )));
                }
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text, extras } => {
                            system.push(with_extras(json!({"type": "text", "text": text}), extras));
                        }
                        other => drop_block(warnings, other, &message.id),
                    }
                }
            }
            Role::User => {
                let blocks: Vec<Value> = message
                    .content
                    .iter()
                    .filter_map(|block| user_block(block, &message.id, warnings))
                    .collect();
                if blocks.is_empty() {
                    return Err(invalid_request(format!(
                        "user message {} has no content this protocol can send",
                        message.id
                    )));
                }
                push_turn(&mut turns, Role::User, blocks);
            }
            Role::Assistant => {
                let mut blocks = Vec::new();
                for block in &message.content {
                    match assistant_block(block) {
                        Encoded::Send(value) => blocks.push(value),
                        Encoded::Skip => {}
                        Encoded::UnsignedReasoning => reasoning_dropped += 1,
                        Encoded::Unsupported => drop_block(warnings, block, &message.id),
                    }
                }
                if blocks.is_empty() {
                    warnings.push(Warning::new(
                        "message_dropped",
                        format!("assistant message {} has nothing to send", message.id),
                    ));
                } else {
                    push_turn(&mut turns, Role::Assistant, blocks);
                }
            }
            // Tool results travel in a user turn.
            Role::Tool => {
                let result = tool_result(message, warnings);
                push_turn(&mut turns, Role::User, vec![result]);
            }
        }
    }
    if reasoning_dropped > 0 {
        warnings.push(Warning::new(
            "reasoning_dropped",
            format!(
                "{reasoning_dropped} reasoning block(s) without a signature are not sent \
                 (only this protocol's own signed reasoning can be sent back)"
            ),
        ));
    }

    let mut body = Map::new();
    body.insert("model".into(), json!(model));
    if !system.is_empty() {
        body.insert("system".into(), Value::Array(system));
    }
    let messages: Vec<Value> = turns
        .into_iter()
        .map(|(role, content)| json!({"role": role.as_str(), "content": content}))
        .collect();
    body.insert("messages".into(), Value::Array(messages));

    let mut tools: Vec<Value> = Vec::new();
    for tool in &request.tools {
        if tool.kind == ToolKind::Hosted {
            warnings.push(Warning::new(
                "hosted_tool_dropped",
                format!(
                    "hosted tool `{}` is dropped; pass Anthropic's tool definition in \
                     `extras.anthropic.tools`",
                    tool.name
                ),
            ));
            continue;
        }
        let mut value = json!({"name": tool.name, "input_schema": tool.input_schema});
        if !tool.description.is_empty() {
            value["description"] = json!(tool.description);
        }
        tools.push(value);
    }

    // Reasoning: a token budget is the older form (`thinking.enabled`);
    // an effort level is the current one (`thinking.adaptive` plus
    // `output_config.effort`), which newer models require.
    let settings = &request.settings;
    let mut output_config = Map::new();
    let (thinking, room) = match settings.reasoning {
        Some(Reasoning::Budget { budget_tokens }) => (
            Some(json!({"type": "enabled", "budget_tokens": budget_tokens})),
            budget_tokens,
        ),
        Some(Reasoning::Effort { effort }) => {
            output_config.insert("effort".into(), json!(effort_name(effort)));
            (Some(json!({"type": "adaptive"})), effort_room(effort))
        }
        None => (None, 0),
    };
    let max_tokens = match (settings.max_tokens, settings.reasoning) {
        (Some(max), Some(Reasoning::Budget { budget_tokens })) if max <= budget_tokens => {
            return Err(invalid_request(format!(
                "max_tokens ({max}) must be larger than the reasoning budget ({budget_tokens})"
            )));
        }
        (Some(max), _) => max,
        (None, _) => options.default_max_tokens.saturating_add(room),
    };
    body.insert("max_tokens".into(), json!(max_tokens));
    if let Some(thinking) = thinking {
        body.insert("thinking".into(), thinking);
    }
    if let Some(temperature) = settings.temperature {
        body.insert("temperature".into(), json!(temperature));
    }
    if !settings.stop.is_empty() {
        body.insert("stop_sequences".into(), json!(settings.stop));
    }
    if let Some(output) = &request.output
        && output.strategy != Some(OutputStrategy::Tool)
    {
        output_config.insert(
            "format".into(),
            json!({"type": "json_schema", "schema": output.schema}),
        );
    }
    if !output_config.is_empty() {
        body.insert("output_config".into(), Value::Object(output_config));
    }

    // Provider-specific fields go last, so the caller wins. `betas` become a
    // header; `tools` (hosted tool definitions) are added to the list.
    let mut keys = vec![PROVIDER];
    if provider != PROVIDER {
        keys.push(provider);
    }
    for key in keys {
        let Some(Value::Object(extra)) = request.extras.get(key) else {
            continue;
        };
        for (name, value) in extra {
            match (name.as_str(), value) {
                ("betas", Value::Array(flags)) => {
                    betas.extend(flags.iter().filter_map(Value::as_str).map(str::to_string));
                }
                ("tools", Value::Array(hosted)) => tools.extend(hosted.iter().cloned()),
                _ => {
                    body.insert(name.clone(), value.clone());
                }
            }
        }
    }
    if !tools.is_empty() {
        let no_parallel = settings.parallel_tool_calls == Some(false);
        let choice = match &request.tool_choice {
            ToolChoice::Auto => no_parallel.then(|| json!({"type": "auto"})),
            ToolChoice::None => Some(json!({"type": "none"})),
            ToolChoice::Required => Some(json!({"type": "any"})),
            ToolChoice::Tool(name) => Some(json!({"type": "tool", "name": name})),
        };
        if let Some(mut choice) = choice {
            if no_parallel && choice["type"] != "none" {
                choice["disable_parallel_tool_use"] = json!(true);
            }
            body.entry("tool_choice").or_insert(choice);
        }
        body.entry("tools").or_insert(Value::Array(tools));
    }
    if stream {
        body.insert("stream".into(), json!(true));
    }

    let mut seen = std::collections::HashSet::new();
    betas.retain(|flag| !flag.is_empty() && seen.insert(flag.clone()));
    Ok(WireBody {
        body: Value::Object(body),
        betas,
    })
}

fn invalid_request(message: String) -> ModelError {
    ModelError::new(ModelErrorKind::InvalidRequest, message)
}

fn drop_block(warnings: &mut Vec<Warning>, block: &ContentBlock, message_id: &str) {
    warnings.push(Warning::new(
        "content_dropped",
        format!(
            "a `{}` block in message {message_id} is not supported here",
            block.type_name()
        ),
    ));
}

/// Consecutive turns of the same role become one turn: the API wants
/// user and assistant turns to alternate.
fn push_turn(turns: &mut Vec<(Role, Vec<Value>)>, role: Role, mut blocks: Vec<Value>) {
    match turns.last_mut() {
        Some((last, content)) if *last == role => content.append(&mut blocks),
        _ => turns.push((role, blocks)),
    }
}

/// Copies `extras.anthropic` fields (e.g. `cache_control`, `citations`)
/// into a wire block.
fn with_extras(mut value: Value, extras: &Extras) -> Value {
    if let Some(Value::Object(fields)) = extras.get(PROVIDER) {
        for (name, field) in fields {
            value[name.as_str()] = field.clone();
        }
    }
    value
}

fn source(source: &MediaSource, mime_type: Option<&str>, fallback: &str) -> Value {
    match source {
        MediaSource::Base64 { data } => json!({
            "type": "base64",
            "media_type": mime_type.unwrap_or(fallback),
            "data": data,
        }),
        MediaSource::Url { url } => json!({"type": "url", "url": url}),
        MediaSource::FileId { file_id } => json!({"type": "file", "file_id": file_id}),
    }
}

/// Text, images and documents: shared by user turns and tool results.
fn media_block(block: &ContentBlock) -> Option<Value> {
    Some(match block {
        ContentBlock::Text { text, extras } => {
            if text.is_empty() {
                return None;
            }
            with_extras(json!({"type": "text", "text": text}), extras)
        }
        ContentBlock::Image {
            source: src,
            mime_type,
            extras,
        } => with_extras(
            json!({"type": "image", "source": source(src, mime_type.as_deref(), "image/png")}),
            extras,
        ),
        ContentBlock::File {
            source: src,
            mime_type,
            extras,
        } => with_extras(
            json!({
                "type": "document",
                "source": source(src, mime_type.as_deref(), "application/pdf")
            }),
            extras,
        ),
        _ => return None,
    })
}

fn user_block(
    block: &ContentBlock,
    message_id: &str,
    warnings: &mut Vec<Warning>,
) -> Option<Value> {
    if let ContentBlock::Text { text, .. } = block
        && text.is_empty()
    {
        return None;
    }
    let value = media_block(block);
    if value.is_none() {
        drop_block(warnings, block, message_id);
    }
    value
}

/// What became of an assistant block.
enum Encoded {
    Send(Value),
    /// Empty text: nothing to send.
    Skip,
    /// Reasoning without a signature (from another provider): the API
    /// would reject it.
    UnsignedReasoning,
    Unsupported,
}

/// Reasoning blocks are sent back exactly as received, in order (the API
/// rejects changed ones).
fn assistant_block(block: &ContentBlock) -> Encoded {
    Encoded::Send(match block {
        ContentBlock::Text { text, extras } => {
            if text.is_empty() {
                return Encoded::Skip;
            }
            with_extras(json!({"type": "text", "text": text}), extras)
        }
        ContentBlock::Reasoning {
            text,
            signature,
            redacted,
            extras,
        } => match (redacted, signature) {
            (Some(data), _) => {
                with_extras(json!({"type": "redacted_thinking", "data": data}), extras)
            }
            (None, Some(signature)) => with_extras(
                json!({
                    "type": "thinking",
                    "thinking": text.as_deref().unwrap_or_default(),
                    "signature": signature,
                }),
                extras,
            ),
            (None, None) => return Encoded::UnsignedReasoning,
        },
        ContentBlock::ToolCall {
            id,
            name,
            args,
            extras,
        } => with_extras(
            json!({"type": "tool_use", "id": id, "name": name, "input": args}),
            extras,
        ),
        // Sent as a call (arguments that parse, else none), so the error
        // result that answers it still has a matching call.
        ContentBlock::InvalidToolCall {
            id,
            name,
            raw_args,
            extras,
            ..
        } => {
            let input = match serde_json::from_str::<Value>(raw_args) {
                Ok(Value::Object(map)) => Value::Object(map),
                _ => json!({}),
            };
            with_extras(
                json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name.as_deref().unwrap_or("unknown"),
                    "input": input,
                }),
                extras,
            )
        }
        ContentBlock::Provider { provider, data } if provider == PROVIDER => data.clone(),
        _ => return Encoded::Unsupported,
    })
}

fn tool_result(message: &Message, warnings: &mut Vec<Warning>) -> Value {
    let mut content = Vec::new();
    for block in &message.content {
        let value = match block {
            ContentBlock::Json { value, .. } => {
                Some(json!({"type": "text", "text": value.to_string()}))
            }
            other => media_block(other),
        };
        match value {
            Some(value) => content.push(value),
            None if matches!(block, ContentBlock::Text { text, .. } if text.is_empty()) => {}
            None => warnings.push(Warning::new(
                "content_dropped",
                format!(
                    "a `{}` block in tool result {} is not supported here",
                    block.type_name(),
                    message.id
                ),
            )),
        }
    }
    let is_error = message.status.is_some_and(|s| s != ToolStatus::Success);
    if message.status == Some(ToolStatus::Error)
        && let Some(error) = &message.error
    {
        let shown = content
            .first()
            .and_then(|b| b["text"].as_str())
            .is_some_and(|t| t.starts_with("Error:"));
        if !shown {
            content.insert(
                0,
                json!({"type": "text", "text": format!("Error: {}", error.message)}),
            );
        }
    }
    if content.is_empty() {
        content.push(json!({"type": "text", "text": "(no output)"}));
    }
    let mut value = json!({
        "type": "tool_result",
        "tool_use_id": message.call_id.as_deref().unwrap_or_default(),
        "content": content,
    });
    if is_error {
        value["is_error"] = json!(true);
    }
    value
}

// ---------- decode ----------

/// Decodes a complete (non-streamed) response body.
pub(crate) fn decode(body: &[u8], provider: &str) -> Result<ModelResponse, ModelError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|err| invalid_response(format!("body is not JSON: {err}")))?;
    if value.get("type").and_then(Value::as_str) == Some("error") {
        return Err(in_band_error(&value));
    }
    let content = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_response("the response has no content"))?;
    let (reason, raw) = stop_reason(value.get("stop_reason"));

    let mut assembler = Assembler::new();
    assembler.push(StreamItem::Start {
        provider_message_id: value.get("id").and_then(Value::as_str).map(str::to_string),
    });
    for (i, block) in content.iter().enumerate() {
        let index = i as u32;
        // A tool call cut by the token limit is the last block.
        let complete = !(reason == FinishReason::Length && i + 1 == content.len());
        for item in whole_block(index, block, complete) {
            assembler.push(item);
        }
    }
    let mut usage = UsageParts::default();
    if let Some(wire) = value.get("usage") {
        usage.update(wire);
    }
    assembler.push(StreamItem::Usage(usage.total()));
    assembler.push(StreamItem::Finish { reason, raw });

    let mut response = assembler.finish()?;
    let metadata = &mut response.message.metadata;
    metadata.provider = Some(provider.to_string());
    metadata.model = value
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(sequence) = value.get("stop_sequence").and_then(Value::as_str) {
        metadata
            .other
            .insert("stop_sequence".into(), json!(sequence));
    }
    Ok(response)
}

/// The items for one complete wire block.
fn whole_block(index: u32, block: &Value, complete: bool) -> Vec<StreamItem> {
    let text = |field: &str| block.get(field).and_then(Value::as_str).unwrap_or_default();
    let delta = |delta| StreamItem::Delta { index, delta };
    let mut items = Vec::new();
    match block.get("type").and_then(Value::as_str) {
        Some("text") => {
            if !text("text").is_empty() {
                items.push(delta(BlockDelta::Text {
                    text: text("text").into(),
                }));
            }
            if let Some(citations) = block.get("citations").filter(|c| has_items(c)) {
                items.push(citations_item(index, citations.clone()));
            }
        }
        Some("thinking") => items.push(delta(BlockDelta::Reasoning {
            text: Some(text("thinking").into()),
            signature: Some(text("signature").into()).filter(|s: &String| !s.is_empty()),
        })),
        Some("redacted_thinking") => items.push(delta(BlockDelta::Block {
            block: redacted(text("data")),
        })),
        Some("tool_use") => {
            items.push(delta(BlockDelta::ToolCall {
                id: block.get("id").and_then(Value::as_str).map(str::to_string),
                name: block
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                args_fragment: block.get("input").map(Value::to_string).unwrap_or_default(),
            }));
            if complete {
                items.push(StreamItem::BlockEnd { index });
            }
        }
        // Hosted tools (`server_tool_use`, `web_search_tool_result`, ...)
        // and block types we do not know: kept as they are, never run.
        _ => items.push(delta(BlockDelta::Block {
            block: ContentBlock::provider(PROVIDER, block.clone()),
        })),
    }
    items
}

fn has_items(value: &Value) -> bool {
    value.as_array().is_some_and(|items| !items.is_empty())
}

fn redacted(data: &str) -> ContentBlock {
    ContentBlock::Reasoning {
        text: None,
        signature: None,
        redacted: Some(data.to_string()),
        extras: Extras::new(),
    }
}

fn citations_item(index: u32, citations: Value) -> StreamItem {
    let mut extras = Extras::new();
    extras.insert(PROVIDER.into(), json!({"citations": citations}));
    StreamItem::Extras { index, extras }
}

fn stop_reason(value: Option<&Value>) -> (FinishReason, Option<String>) {
    let raw = value.and_then(Value::as_str);
    let reason = match raw {
        Some("end_turn" | "stop_sequence") => FinishReason::Stop,
        Some("max_tokens" | "model_context_window_exceeded") => FinishReason::Length,
        Some("tool_use") => FinishReason::ToolCalls,
        Some("refusal") => FinishReason::ContentFilter,
        _ => FinishReason::Other,
    };
    (reason, raw.map(str::to_string))
}

/// Anthropic counts cache reads and writes outside `input_tokens`; ours
/// includes them (0002 §1). Fields are kept as last reported.
#[derive(Debug, Default)]
struct UsageParts {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    reasoning: u64,
}

impl UsageParts {
    fn update(&mut self, wire: &Value) {
        let fields = [
            ("input_tokens", &mut self.input),
            ("output_tokens", &mut self.output),
            ("cache_read_input_tokens", &mut self.cache_read),
            ("cache_creation_input_tokens", &mut self.cache_write),
        ];
        for (name, field) in fields {
            if let Some(value) = wire.get(name).and_then(Value::as_u64) {
                *field = value;
            }
        }
        if let Some(value) = wire["output_tokens_details"]["thinking_tokens"].as_u64() {
            self.reasoning = value;
        }
    }

    fn total(&self) -> Usage {
        let mut usage = Usage::new(self.input + self.cache_read + self.cache_write, self.output);
        usage.cache_read_tokens = self.cache_read;
        usage.cache_write_tokens = self.cache_write;
        usage.reasoning_tokens = self.reasoning;
        usage
    }
}

/// An `{"type": "error", "error": {...}}` body or stream event. Without an
/// HTTP status, the error type stands in for one, so the usual
/// classification (including the text checks) applies.
fn in_band_error(value: &Value) -> ModelError {
    let status = match value["error"]["type"].as_str() {
        Some("invalid_request_error") => 400,
        Some("authentication_error") => 401,
        Some("billing_error") => 402,
        Some("permission_error") => 403,
        Some("not_found_error") => 404,
        Some("conflict_error") => 409,
        Some("request_too_large") => 413,
        Some("rate_limit_error") => 429,
        Some("api_error") => 500,
        Some("timeout_error") => 504,
        Some("overloaded_error") => 529,
        _ => 0,
    };
    let text = value.to_string();
    let mut error = classify_http(&HttpFailure::new(status, &[], &text), std::time::UNIX_EPOCH);
    error.status = None;
    error
}

/// What a streamed block is, for routing its deltas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Reasoning,
    ToolCall,
    /// Kept whole: the start block plus any `input_json_delta` pieces.
    Provider,
}

/// Decodes streamed events into [`StreamItem`]s. The wire block `index` is
/// our block index.
#[derive(Debug, Default)]
pub(crate) struct EventDecoder {
    done: bool,
    model: Option<String>,
    stop_sequence: Option<String>,
    usage: UsageParts,
    kinds: HashMap<u32, Kind>,
    provider_blocks: HashMap<u32, (Value, String)>,
    citations: HashMap<u32, Vec<Value>>,
    /// The last finished tool call. Its end is held back until the next
    /// block stops or the stop reason arrives: a call cut by the token limit is
    /// `truncated`, not a parse error.
    held_end: Option<u32>,
}

impl EventDecoder {
    /// `message_stop` was received.
    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    /// The model name the server reported.
    pub(crate) fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// The stop sequence that ended the answer, if any.
    pub(crate) fn stop_sequence(&self) -> Option<&str> {
        self.stop_sequence.as_deref()
    }

    fn release(&mut self, items: &mut Vec<StreamItem>) {
        if let Some(index) = self.held_end.take() {
            items.push(StreamItem::BlockEnd { index });
        }
    }

    pub(crate) fn on_event(&mut self, event: &SseEvent) -> Result<Vec<StreamItem>, ModelError> {
        let mut items = Vec::new();
        let data = event.data.trim();
        if data.is_empty() {
            return Ok(items);
        }
        let value: Value = serde_json::from_str(data)
            .map_err(|err| invalid_response(format!("stream event is not JSON: {err}")))?;
        let index = value.get("index").and_then(Value::as_u64).map(|i| i as u32);
        match value.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                let message = value.get("message").unwrap_or(&Value::Null);
                items.push(StreamItem::Start {
                    provider_message_id: message
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                });
                self.model = message
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if let Some(usage) = message.get("usage") {
                    self.usage.update(usage);
                    items.push(StreamItem::Usage(self.usage.total()));
                }
            }
            Some("content_block_start") => {
                let (Some(index), Some(block)) = (index, value.get("content_block")) else {
                    return Err(invalid_response("content_block_start without a block"));
                };
                self.start_block(index, block, &mut items);
            }
            Some("content_block_delta") => {
                let (Some(index), Some(delta)) = (index, value.get("delta")) else {
                    return Err(invalid_response("content_block_delta without a delta"));
                };
                self.block_delta(index, delta, &mut items);
            }
            Some("content_block_stop") => {
                let Some(index) = index else {
                    return Err(invalid_response("content_block_stop without an index"));
                };
                self.release(&mut items);
                if let Some((mut block, input)) = self.provider_blocks.remove(&index) {
                    if !input.is_empty() {
                        block["input"] =
                            serde_json::from_str(&input).unwrap_or(Value::String(input));
                    }
                    items.push(StreamItem::Delta {
                        index,
                        delta: BlockDelta::Block {
                            block: ContentBlock::provider(PROVIDER, block),
                        },
                    });
                }
                if let Some(citations) = self.citations.remove(&index) {
                    items.push(citations_item(index, Value::Array(citations)));
                }
                if self.kinds.get(&index) == Some(&Kind::ToolCall) {
                    self.held_end = Some(index);
                }
            }
            Some("message_delta") => {
                if let Some(usage) = value.get("usage") {
                    self.usage.update(usage);
                    items.push(StreamItem::Usage(self.usage.total()));
                }
                let delta = value.get("delta").unwrap_or(&Value::Null);
                if let Some(sequence) = delta.get("stop_sequence").and_then(Value::as_str) {
                    self.stop_sequence = Some(sequence.to_string());
                }
                if let Some(raw) = delta.get("stop_reason").filter(|r| !r.is_null()) {
                    let (reason, raw) = stop_reason(Some(raw));
                    if reason == FinishReason::Length {
                        self.held_end = None;
                    } else {
                        self.release(&mut items);
                    }
                    items.push(StreamItem::Finish { reason, raw });
                }
            }
            Some("message_stop") => self.done = true,
            Some("error") => return Err(in_band_error(&value)),
            // `ping` and event types added later.
            _ => {}
        }
        Ok(items)
    }

    fn start_block(&mut self, index: u32, block: &Value, items: &mut Vec<StreamItem>) {
        let text = |field: &str| block.get(field).and_then(Value::as_str).unwrap_or_default();
        let kind = match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if !text("text").is_empty() {
                    items.push(StreamItem::Delta {
                        index,
                        delta: BlockDelta::Text {
                            text: text("text").into(),
                        },
                    });
                }
                if let Some(Value::Array(citations)) = block.get("citations") {
                    self.citations
                        .entry(index)
                        .or_default()
                        .extend(citations.iter().cloned());
                }
                Kind::Text
            }
            Some("thinking") => {
                let (thinking, signature) = (text("thinking"), text("signature"));
                if !thinking.is_empty() || !signature.is_empty() {
                    items.push(StreamItem::Delta {
                        index,
                        delta: BlockDelta::Reasoning {
                            text: Some(thinking.to_string()).filter(|t| !t.is_empty()),
                            signature: Some(signature.to_string()).filter(|s| !s.is_empty()),
                        },
                    });
                }
                Kind::Reasoning
            }
            Some("redacted_thinking") => {
                items.push(StreamItem::Delta {
                    index,
                    delta: BlockDelta::Block {
                        block: redacted(text("data")),
                    },
                });
                Kind::Reasoning
            }
            Some("tool_use") => {
                // The start carries an empty `input`; the arguments follow
                // as `input_json_delta` pieces.
                items.push(StreamItem::Delta {
                    index,
                    delta: BlockDelta::ToolCall {
                        id: block.get("id").and_then(Value::as_str).map(str::to_string),
                        name: block
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        args_fragment: String::new(),
                    },
                });
                Kind::ToolCall
            }
            _ => {
                let mut block = block.clone();
                // Streamed hosted-tool inputs start empty and arrive as
                // pieces; results arrive whole.
                let streamed_input = block.get("input") == Some(&json!({}));
                if streamed_input && let Some(fields) = block.as_object_mut() {
                    fields.remove("input");
                }
                self.provider_blocks.insert(index, (block, String::new()));
                Kind::Provider
            }
        };
        self.kinds.insert(index, kind);
    }

    fn block_delta(&mut self, index: u32, delta: &Value, items: &mut Vec<StreamItem>) {
        let text = |field: &str| delta.get(field).and_then(Value::as_str).unwrap_or_default();
        let item = |delta| StreamItem::Delta { index, delta };
        match delta.get("type").and_then(Value::as_str) {
            Some("text_delta") => items.push(item(BlockDelta::Text {
                text: text("text").into(),
            })),
            Some("thinking_delta") => items.push(item(BlockDelta::Reasoning {
                text: Some(text("thinking").into()),
                signature: None,
            })),
            Some("signature_delta") => items.push(item(BlockDelta::Reasoning {
                text: None,
                signature: Some(text("signature").into()),
            })),
            Some("input_json_delta") => {
                if let Some((_, input)) = self.provider_blocks.get_mut(&index) {
                    input.push_str(text("partial_json"));
                } else {
                    items.push(item(BlockDelta::ToolCall {
                        id: None,
                        name: None,
                        args_fragment: text("partial_json").into(),
                    }));
                }
            }
            Some("citations_delta") => {
                if let Some(citation) = delta.get("citation") {
                    self.citations
                        .entry(index)
                        .or_default()
                        .push(citation.clone());
                }
            }
            _ => {}
        }
    }
}
