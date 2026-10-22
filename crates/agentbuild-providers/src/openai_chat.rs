//! The OpenAI chat-completions protocol (design 0006 §4, research
//! litellm-providers §10.5, §10.7). One protocol for OpenAI, OpenRouter,
//! Groq, Together, vLLM, LM Studio, Ollama, LiteLLM and other gateways; the
//! differences between servers are [`OpenAiQuirks`].
//!
//! Pure translation: no I/O, clock or environment.

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
use crate::think::{self, Part, ThinkSplitter};
use crate::tool::ToolKind;

/// Differences between OpenAI-compatible servers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct OpenAiQuirks {
    /// Body field for `settings.max_tokens`: `max_completion_tokens` (OpenAI)
    /// or `max_tokens` (most compatible servers).
    pub max_tokens_field: String,
    /// Send `stream_options.include_usage` when streaming.
    pub stream_usage: bool,
    /// Send `parallel_tool_calls` (some servers reject the field).
    pub parallel_tool_calls: bool,
    /// Send `reasoning_effort` for `settings.reasoning`.
    pub reasoning_effort: bool,
    /// Role used for system messages (`system`, or `developer` for some
    /// OpenAI reasoning models).
    pub system_role: String,
    /// Assistant-message field to send earlier reasoning back in (e.g.
    /// `reasoning_content`), for servers that want it. `None` drops it.
    pub reasoning_echo: Option<String>,
    /// Read `<think>…</think>` at the start of the answer text as
    /// reasoning (reasoning models on servers without a reasoning parser,
    /// e.g. DeepSeek-R1 on Ollama 0.6). Default on.
    pub think_tags: bool,
    /// Gemini (design 0006 §14): an assistant message whose tool calls
    /// carry no thought signature (history from another model) gets
    /// Google's placeholder on its first call, which Gemini 3 requires.
    /// A tool call's own `extra_content` is always sent back, on every
    /// endpoint.
    pub thought_signatures: bool,
}

impl Default for OpenAiQuirks {
    /// Settings that work with most compatible servers.
    fn default() -> Self {
        Self {
            max_tokens_field: "max_tokens".into(),
            stream_usage: true,
            parallel_tool_calls: false,
            reasoning_effort: false,
            system_role: "system".into(),
            reasoning_echo: None,
            think_tags: true,
            thought_signatures: false,
        }
    }
}

/// Google's documented placeholder for a missing thought signature.
const PLACEHOLDER_SIGNATURE: &str = "skip_thought_signature_validator";

/// A tool call's `extra_content` (Gemini's thought signature), as block
/// extras under the endpoint's name.
fn extra_content(call: &Value, provider: &str) -> Option<Extras> {
    let extra = call.get("extra_content").filter(|v| v.is_object())?;
    let mut extras = Extras::new();
    extras.insert(provider.to_string(), json!({"extra_content": extra}));
    Some(extras)
}

impl OpenAiQuirks {
    /// OpenAI's own API.
    pub fn openai() -> Self {
        Self {
            max_tokens_field: "max_completion_tokens".into(),
            parallel_tool_calls: true,
            reasoning_effort: true,
            ..Self::default()
        }
    }
}

// ---------- encode ----------

/// Builds the JSON body for `request`. Anything the protocol cannot carry is
/// dropped with a warning, or rejected as `invalid_request` before sending.
pub(crate) fn encode(
    request: &ModelRequest,
    model: &str,
    provider: &str,
    quirks: &OpenAiQuirks,
    stream: bool,
    warnings: &mut Vec<Warning>,
) -> Result<Value, ModelError> {
    let mut messages = Vec::new();
    let mut pending_images: Vec<Value> = Vec::new();
    let mut reasoning_dropped = 0;

    for message in &request.messages {
        if message.role != Role::Tool && !pending_images.is_empty() {
            flush_images(&mut messages, &mut pending_images, warnings);
        }
        match message.role {
            Role::System => messages.push(json!({
                "role": quirks.system_role,
                "content": join_text(&message.content, "\n\n"),
            })),
            Role::User => messages.push(encode_user(message, warnings)?),
            Role::Assistant => {
                let (value, dropped) = encode_assistant(message, provider, quirks, warnings);
                reasoning_dropped += dropped;
                messages.push(value);
            }
            Role::Tool => messages.push(encode_tool(message, &mut pending_images, warnings)),
        }
    }
    if !pending_images.is_empty() {
        flush_images(&mut messages, &mut pending_images, warnings);
    }
    if reasoning_dropped > 0 {
        warnings.push(Warning::new(
            "reasoning_dropped",
            format!("{reasoning_dropped} reasoning block(s) are not sent on this protocol"),
        ));
    }

    let mut body = Map::new();
    body.insert("model".into(), json!(model));
    body.insert("messages".into(), Value::Array(messages));

    let tools: Vec<Value> = request
        .tools
        .iter()
        .filter(|tool| {
            let hosted = tool.kind == ToolKind::Hosted;
            if hosted {
                warnings.push(Warning::new(
                    "hosted_tool_dropped",
                    format!(
                        "hosted tool `{}` is not supported on this protocol",
                        tool.name
                    ),
                ));
            }
            !hosted
        })
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                }
            })
        })
        .collect();
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
        let choice = match &request.tool_choice {
            ToolChoice::Auto => None,
            ToolChoice::None => Some(json!("none")),
            ToolChoice::Required => Some(json!("required")),
            ToolChoice::Tool(name) => Some(json!({"type": "function", "function": {"name": name}})),
        };
        if let Some(choice) = choice {
            body.insert("tool_choice".into(), choice);
        }
        if quirks.parallel_tool_calls
            && let Some(parallel) = request.settings.parallel_tool_calls
        {
            body.insert("parallel_tool_calls".into(), json!(parallel));
        }
    }

    let settings = &request.settings;
    if let Some(max) = settings.max_tokens {
        body.insert(quirks.max_tokens_field.clone(), json!(max));
    }
    if let Some(temperature) = settings.temperature {
        body.insert("temperature".into(), json!(temperature));
    }
    if !settings.stop.is_empty() {
        body.insert("stop".into(), json!(settings.stop));
    }
    match settings.reasoning {
        Some(Reasoning::Effort { effort }) if quirks.reasoning_effort => {
            let effort = match effort {
                ReasoningEffort::Low => "low",
                ReasoningEffort::Medium => "medium",
                ReasoningEffort::High => "high",
            };
            body.insert("reasoning_effort".into(), json!(effort));
        }
        Some(_) => warnings.push(Warning::new(
            "reasoning_setting_dropped",
            "this endpoint does not take a reasoning setting",
        )),
        None => {}
    }
    if let Some(output) = &request.output
        && output.strategy != Some(OutputStrategy::Tool)
    {
        body.insert(
            "response_format".into(),
            json!({
                "type": "json_schema",
                "json_schema": {"name": "final_output", "schema": output.schema, "strict": true}
            }),
        );
    }
    if stream {
        body.insert("stream".into(), json!(true));
        if quirks.stream_usage {
            body.insert("stream_options".into(), json!({"include_usage": true}));
        }
    }

    // Provider-specific fields go last, so the caller wins.
    for key in ["openai", provider] {
        if let Some(Value::Object(extra)) = request.extras.get(key) {
            for (name, value) in extra {
                body.insert(name.clone(), value.clone());
            }
        }
    }
    Ok(Value::Object(body))
}

fn join_text(blocks: &[ContentBlock], separator: &str) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(separator)
}

fn data_url(mime: Option<&str>, data: &str, fallback: &str) -> String {
    format!("data:{};base64,{data}", mime.unwrap_or(fallback))
}

fn encode_user(message: &Message, warnings: &mut Vec<Warning>) -> Result<Value, ModelError> {
    let mut parts = Vec::new();
    for block in &message.content {
        let mut drop = |what: &str| {
            warnings.push(Warning::new(
                "content_dropped",
                format!("{what} in message {} is not supported here", message.id),
            ));
        };
        match block {
            ContentBlock::Text { text, .. } => parts.push(json!({"type": "text", "text": text})),
            ContentBlock::Image {
                source, mime_type, ..
            } => match source {
                MediaSource::Url { url } => {
                    parts.push(json!({"type": "image_url", "image_url": {"url": url}}));
                }
                MediaSource::Base64 { data } => parts.push(json!({
                    "type": "image_url",
                    "image_url": {"url": data_url(mime_type.as_deref(), data, "image/png")}
                })),
                _ => drop("an image by file id"),
            },
            ContentBlock::Audio {
                source: MediaSource::Base64 { data },
                mime_type,
                ..
            } => {
                let format = match mime_type.as_deref() {
                    Some("audio/mpeg" | "audio/mp3") => "mp3",
                    _ => "wav",
                };
                parts.push(json!({
                    "type": "input_audio",
                    "input_audio": {"data": data, "format": format}
                }));
            }
            ContentBlock::File {
                source, mime_type, ..
            } => match source {
                MediaSource::FileId { file_id } => {
                    parts.push(json!({"type": "file", "file": {"file_id": file_id}}));
                }
                MediaSource::Base64 { data } => parts.push(json!({
                    "type": "file",
                    "file": {"file_data": data_url(mime_type.as_deref(), data, "application/pdf")}
                })),
                _ => drop("a file by URL"),
            },
            other => drop(&format!("a `{}` block", other.type_name())),
        }
    }
    if parts.is_empty() {
        return Err(ModelError::new(
            ModelErrorKind::InvalidRequest,
            format!(
                "user message {} has no content this protocol can send",
                message.id
            ),
        ));
    }
    let content = match parts.as_slice() {
        [single] if single["type"] == "text" => single["text"].clone(),
        _ => Value::Array(parts),
    };
    let mut value = json!({"role": "user", "content": content});
    if let Some(name) = &message.name {
        value["name"] = json!(name);
    }
    Ok(value)
}

/// Returns the message and the number of reasoning blocks dropped.
fn encode_assistant(
    message: &Message,
    provider: &str,
    quirks: &OpenAiQuirks,
    warnings: &mut Vec<Warning>,
) -> (Value, usize) {
    let mut tool_calls = Vec::new();
    let mut reasoning = Vec::new();
    let mut seen_call = false;
    let mut order_lost = false;
    for block in &message.content {
        match block {
            ContentBlock::Text { .. } => order_lost |= seen_call,
            ContentBlock::ToolCall {
                id,
                name,
                args,
                extras,
                ..
            } => {
                seen_call = true;
                let mut call = json!({
                    "id": id, "type": "function",
                    "function": {"name": name, "arguments": Value::Object(args.clone()).to_string()}
                });
                // Sent back as received, on the endpoint that sent it.
                if let Some(extra) = extras.get(provider).and_then(|e| e.get("extra_content")) {
                    call["extra_content"] = extra.clone();
                }
                tool_calls.push(call);
            }
            // Sent as written, so the error result that answers it still
            // has a matching call.
            ContentBlock::InvalidToolCall {
                id, name, raw_args, ..
            } => {
                seen_call = true;
                tool_calls.push(json!({
                    "id": id, "type": "function",
                    "function": {"name": name.as_deref().unwrap_or("unknown"), "arguments": raw_args}
                }));
            }
            ContentBlock::Reasoning { text: Some(t), .. } => reasoning.push(t.as_str()),
            ContentBlock::Reasoning { .. } => {}
            other => warnings.push(Warning::new(
                "content_dropped",
                format!(
                    "a `{}` block in message {} is not supported here",
                    other.type_name(),
                    message.id
                ),
            )),
        }
    }
    if order_lost {
        warnings.push(Warning::new(
            "block_order_lost",
            format!(
                "message {}: text after a tool call is sent before it",
                message.id
            ),
        ));
    }
    let text = join_text(&message.content, "\n\n");
    let mut value = json!({"role": "assistant"});
    value["content"] = if text.is_empty() && !tool_calls.is_empty() {
        Value::Null
    } else {
        json!(text)
    };
    if quirks.thought_signatures
        && !tool_calls.is_empty()
        && tool_calls.iter().all(|c| c.get("extra_content").is_none())
    {
        tool_calls[0]["extra_content"] =
            json!({"google": {"thought_signature": PLACEHOLDER_SIGNATURE}});
    }
    if !tool_calls.is_empty() {
        value["tool_calls"] = Value::Array(tool_calls);
    }
    if let Some(name) = &message.name {
        value["name"] = json!(name);
    }
    let mut dropped = 0;
    if !reasoning.is_empty() {
        match &quirks.reasoning_echo {
            Some(field) => value[field.as_str()] = json!(reasoning.join("\n\n")),
            None => dropped = reasoning.len(),
        }
    }
    (value, dropped)
}

fn encode_tool(message: &Message, images: &mut Vec<Value>, warnings: &mut Vec<Warning>) -> Value {
    let mut parts = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text, .. } => parts.push(text.clone()),
            ContentBlock::Json { value, .. } => parts.push(value.to_string()),
            ContentBlock::Image {
                source, mime_type, ..
            } => match source {
                MediaSource::Url { url } => {
                    images.push(json!({"type": "image_url", "image_url": {"url": url}}));
                }
                MediaSource::Base64 { data } => images.push(json!({
                    "type": "image_url",
                    "image_url": {"url": data_url(mime_type.as_deref(), data, "image/png")}
                })),
                _ => warnings.push(Warning::new(
                    "content_dropped",
                    format!("an image by file id in tool result {}", message.id),
                )),
            },
            other => warnings.push(Warning::new(
                "content_dropped",
                format!(
                    "a `{}` block in tool result {} is not supported here",
                    other.type_name(),
                    message.id
                ),
            )),
        }
    }
    let mut content = parts.join("\n");
    if message.status == Some(ToolStatus::Error)
        && let Some(error) = &message.error
        && !content.starts_with("Error:")
    {
        content = if content.is_empty() {
            format!("Error: {}", error.message)
        } else {
            format!("Error: {}\n{content}", error.message)
        };
    }
    if content.is_empty() {
        content = "(no output)".into();
    }
    json!({
        "role": "tool",
        "tool_call_id": message.call_id.as_deref().unwrap_or_default(),
        "content": content,
    })
}

/// Tool results cannot carry images on this protocol: they follow the tool
/// messages in a user message.
fn flush_images(messages: &mut Vec<Value>, images: &mut Vec<Value>, warnings: &mut Vec<Warning>) {
    warnings.push(Warning::new(
        "tool_result_images_moved",
        format!(
            "{} image(s) from tool results were sent in a following user message",
            images.len()
        ),
    ));
    let mut content = vec![json!({"type": "text", "text": "Images returned by the tools above:"})];
    content.append(images);
    messages.push(json!({"role": "user", "content": content}));
}

// ---------- decode ----------

/// Decodes a complete (non-streamed) response body.
pub(crate) fn decode(
    body: &[u8],
    provider: &str,
    think_tags: bool,
) -> Result<ModelResponse, ModelError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|err| invalid_response(format!("body is not JSON: {err}")))?;
    if value.get("choices").is_none()
        && let Some(error) = in_band_error(&value)
    {
        return Err(error);
    }
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| invalid_response("the response has no choices"))?;
    let message = choice.get("message").unwrap_or(&Value::Null);

    let mut assembler = Assembler::new();
    assembler.push(StreamItem::Start {
        provider_message_id: value.get("id").and_then(Value::as_str).map(str::to_string),
    });
    let mut index = 0;
    let mut next = || {
        index += 1;
        index - 1
    };
    if let Some(reasoning) = reasoning_text(message) {
        assembler.push(StreamItem::Delta {
            index: next(),
            delta: BlockDelta::Reasoning {
                text: Some(reasoning.to_string()),
                signature: None,
            },
        });
    }
    if let Some(text) = message.get("content").and_then(Value::as_str)
        && !text.is_empty()
    {
        let (thought, text) = if think_tags {
            think::split(text)
        } else {
            (None, text.to_string())
        };
        if let Some(thought) = thought {
            assembler.push(StreamItem::Delta {
                index: next(),
                delta: BlockDelta::Reasoning {
                    text: Some(thought),
                    signature: None,
                },
            });
        }
        if !text.is_empty() {
            assembler.push(StreamItem::Delta {
                index: next(),
                delta: BlockDelta::Text { text },
            });
        }
    }
    let refused = message.get("refusal").and_then(Value::as_str);
    if let Some(refusal) = refused {
        assembler.push(StreamItem::Delta {
            index: next(),
            delta: BlockDelta::Block {
                block: refusal_block(refusal),
            },
        });
    }
    for call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let at = next();
        let function = call.get("function").unwrap_or(&Value::Null);
        assembler.push(StreamItem::Delta {
            index: at,
            delta: BlockDelta::ToolCall {
                id: call.get("id").and_then(Value::as_str).map(str::to_string),
                name: function
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                args_fragment: arguments(function.get("arguments")),
            },
        });
        if let Some(extras) = extra_content(call, provider) {
            assembler.push(StreamItem::Extras { index: at, extras });
        }
        assembler.push(StreamItem::BlockEnd { index: at });
    }
    if let Some(usage) = value.get("usage") {
        assembler.push(StreamItem::Usage(usage_from(usage)));
    }
    let (reason, raw) = finish_reason(choice.get("finish_reason"));
    assembler.push(StreamItem::Finish {
        reason: if refused.is_some() {
            FinishReason::ContentFilter
        } else {
            reason
        },
        raw,
    });
    let mut response = assembler.finish()?;
    let metadata = &mut response.message.metadata;
    metadata.provider = Some(provider.to_string());
    metadata.model = value
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(response)
}

/// Decodes streamed chunks (`data:` events) into [`StreamItem`]s.
#[derive(Debug, Default)]
pub(crate) struct ChunkDecoder {
    started: bool,
    done: bool,
    model: Option<String>,
    next_index: u32,
    text: Option<u32>,
    reasoning: Option<u32>,
    /// Wire tool-call index (or id) → our block index.
    calls: HashMap<String, u32>,
    last_call: Option<u32>,
    finished: Option<FinishReason>,
    /// Splits `<think>` reasoning out of the answer text, if enabled.
    think: Option<ThinkSplitter>,
    /// The endpoint's name, for the extras of tool calls.
    provider: String,
}

impl ChunkDecoder {
    /// A decoder; `think_tags` as in [`OpenAiQuirks::think_tags`],
    /// `provider` the endpoint's name.
    pub(crate) fn new(think_tags: bool, provider: &str) -> Self {
        Self {
            think: think_tags.then(ThinkSplitter::default),
            provider: provider.to_string(),
            ..Self::default()
        }
    }

    /// Adds answer text, or reasoning split out of it.
    fn push_part(&mut self, items: &mut Vec<StreamItem>, part: Part, text: String) {
        let slot = match part {
            Part::Reasoning => self.reasoning,
            Part::Text => self.text,
        };
        let index = match slot {
            Some(index) => index,
            None => {
                let index = self.allocate();
                match part {
                    Part::Reasoning => self.reasoning = Some(index),
                    Part::Text => self.text = Some(index),
                }
                index
            }
        };
        items.push(StreamItem::Delta {
            index,
            delta: match part {
                Part::Reasoning => BlockDelta::Reasoning {
                    text: Some(text),
                    signature: None,
                },
                Part::Text => BlockDelta::Text { text },
            },
        });
    }

    /// Decides text the `<think>` splitter still holds (end of the answer).
    fn flush_think(&mut self, items: &mut Vec<StreamItem>) {
        let parts = self
            .think
            .as_mut()
            .map(ThinkSplitter::finish)
            .unwrap_or_default();
        for (part, text) in parts {
            self.push_part(items, part, text);
        }
    }
    /// `[DONE]` was received.
    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    /// The model name the server reported.
    pub(crate) fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    fn allocate(&mut self) -> u32 {
        self.next_index += 1;
        self.next_index - 1
    }

    pub(crate) fn on_event(&mut self, event: &SseEvent) -> Result<Vec<StreamItem>, ModelError> {
        let mut items = Vec::new();
        let data = event.data.trim();
        if data == "[DONE]" {
            self.done = true;
            self.flush_think(&mut items);
            return Ok(items);
        }
        if data.is_empty() {
            return Ok(items);
        }
        let chunk: Value = serde_json::from_str(data)
            .map_err(|err| invalid_response(format!("stream chunk is not JSON: {err}")))?;
        if let Some(error) = in_band_error(&chunk) {
            return Err(error);
        }
        if !self.started {
            self.started = true;
            items.push(StreamItem::Start {
                provider_message_id: chunk.get("id").and_then(Value::as_str).map(str::to_string),
            });
        }
        if self.model.is_none() {
            self.model = chunk
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if let Some(usage) = chunk.get("usage").filter(|u| u.is_object()) {
            items.push(StreamItem::Usage(usage_from(usage)));
        }
        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        else {
            return Ok(items);
        };
        let delta = choice.get("delta").unwrap_or(&Value::Null);
        if let Some(text) = reasoning_text(delta) {
            self.push_part(&mut items, Part::Reasoning, text.to_string());
        }
        if let Some(text) = delta.get("content").and_then(Value::as_str)
            && !text.is_empty()
        {
            let parts = match &mut self.think {
                Some(splitter) => splitter.push(text),
                None => vec![(Part::Text, text.to_string())],
            };
            for (part, text) in parts {
                self.push_part(&mut items, part, text);
            }
        }
        if let Some(refusal) = delta.get("refusal").and_then(Value::as_str) {
            let index = self.allocate();
            items.push(StreamItem::Delta {
                index,
                delta: BlockDelta::Block {
                    block: refusal_block(refusal),
                },
            });
            self.finished = Some(FinishReason::ContentFilter);
        }
        for call in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .filter(|i| !i.is_empty());
            // Key by wire index; servers without one start a call per id.
            let key = match (call.get("index").and_then(Value::as_u64), id) {
                (Some(i), _) => Some(format!("#{i}")),
                (None, Some(id)) => Some(format!("id:{id}")),
                (None, None) => None,
            };
            let index = match key {
                Some(key) => match self.calls.get(&key) {
                    Some(&index) => index,
                    None => {
                        let index = self.allocate();
                        self.calls.insert(key, index);
                        index
                    }
                },
                None => match self.last_call {
                    Some(index) => index,
                    None => self.allocate(),
                },
            };
            self.last_call = Some(index);
            let function = call.get("function").unwrap_or(&Value::Null);
            items.push(StreamItem::Delta {
                index,
                delta: BlockDelta::ToolCall {
                    id: id.map(str::to_string),
                    name: function
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|n| !n.is_empty())
                        .map(str::to_string),
                    args_fragment: arguments(function.get("arguments")),
                },
            });
            if let Some(extras) = extra_content(call, &self.provider) {
                items.push(StreamItem::Extras { index, extras });
            }
        }
        if let Some(raw) = choice.get("finish_reason").filter(|r| !r.is_null()) {
            self.flush_think(&mut items);
            let (reason, raw) = finish_reason(Some(raw));
            let reason = self.finished.unwrap_or(reason);
            self.finished = Some(reason);
            if reason != FinishReason::Length {
                // Calls are complete; an open call at `length` was cut.
                let mut ends: Vec<u32> = self.calls.values().copied().collect();
                ends.extend(self.last_call);
                ends.sort_unstable();
                ends.dedup();
                items.extend(ends.into_iter().map(|index| StreamItem::BlockEnd { index }));
            }
            items.push(StreamItem::Finish { reason, raw });
        }
        Ok(items)
    }
}

fn refusal_block(refusal: &str) -> ContentBlock {
    let mut extras = Extras::new();
    extras.insert("openai".into(), json!({"refusal": true}));
    ContentBlock::Text {
        text: refusal.to_string(),
        extras,
    }
}

/// `reasoning_content` (DeepSeek, vLLM) or `reasoning` (OpenRouter, Ollama).
fn reasoning_text(message: &Value) -> Option<&str> {
    ["reasoning_content", "reasoning"]
        .iter()
        .find_map(|field| message.get(*field).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
}

/// Arguments arrive as a JSON string; some servers send an object.
fn arguments(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn finish_reason(value: Option<&Value>) -> (FinishReason, Option<String>) {
    let raw = value.and_then(Value::as_str);
    let reason = match raw {
        Some("stop") => FinishReason::Stop,
        Some("length") => FinishReason::Length,
        Some("tool_calls" | "function_call") => FinishReason::ToolCalls,
        Some("content_filter") => FinishReason::ContentFilter,
        _ => FinishReason::Other,
    };
    (reason, raw.map(str::to_string))
}

fn usage_from(value: &Value) -> Usage {
    let number = |path: &[&str]| {
        path.iter()
            .try_fold(value, |v, key| v.get(*key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let mut usage = Usage::new(number(&["prompt_tokens"]), number(&["completion_tokens"]));
    usage.cache_read_tokens = number(&["prompt_tokens_details", "cached_tokens"]);
    usage.reasoning_tokens = number(&["completion_tokens_details", "reasoning_tokens"]);
    usage
}

/// An `{"error": …}` object in a 2xx body or a stream chunk.
fn in_band_error(value: &Value) -> Option<ModelError> {
    value.get("error").filter(|e| !e.is_null())?;
    let text = value.to_string();
    // Status 0: the kind comes from the in-band code, else `unknown`.
    let mut error = classify_http(&HttpFailure::new(0, &[], &text), std::time::UNIX_EPOCH);
    error.status = None;
    Some(error)
}
