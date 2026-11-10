//! The OpenAI Responses protocol (design 0006 §16): `POST {base_url}/responses`.
//! OpenAI's current API, the only one that allows tool calls together with a
//! reasoning effort on newer models. Sent with `store: false`: the provider
//! keeps nothing, so reasoning travels with the transcript as encrypted
//! items.
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
use crate::tool::ToolKind;

/// Settings of a Responses endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct ResponsesOptions {
    /// Let the provider keep the conversation (`store`). Off by default:
    /// nothing is kept, and the full transcript is sent each turn.
    pub store: bool,
    /// Ask for reasoning as encrypted items
    /// (`include: ["reasoning.encrypted_content"]`), so a reasoning model
    /// keeps its reasoning across tool calls without `store`. Default on.
    pub encrypted_reasoning: bool,
    /// `reasoning.summary` sent with a reasoning effort (`auto`, `concise`,
    /// `detailed`); `None` asks for no summaries.
    pub reasoning_summary: Option<String>,
}

impl Default for ResponsesOptions {
    fn default() -> Self {
        Self {
            store: false,
            encrypted_reasoning: true,
            reasoning_summary: Some("auto".into()),
        }
    }
}

impl ResponsesOptions {
    /// Sets [`ResponsesOptions::store`].
    pub fn store(mut self, store: bool) -> Self {
        self.store = store;
        self
    }

    /// Sets [`ResponsesOptions::encrypted_reasoning`].
    pub fn encrypted_reasoning(mut self, on: bool) -> Self {
        self.encrypted_reasoning = on;
        self
    }

    /// Sets [`ResponsesOptions::reasoning_summary`].
    pub fn reasoning_summary(mut self, summary: Option<String>) -> Self {
        self.reasoning_summary = summary;
        self
    }
}

// ---------- encode ----------

/// Builds the JSON body for `request`. Anything the protocol cannot carry is
/// dropped with a warning, or rejected as `invalid_request` before sending.
pub(crate) fn encode(
    request: &ModelRequest,
    model: &str,
    provider: &str,
    options: &ResponsesOptions,
    stream: bool,
    warnings: &mut Vec<Warning>,
) -> Result<Value, ModelError> {
    let mut body = Map::new();
    body.insert("model".into(), json!(model));

    let mut messages = request.messages.as_slice();
    if let Some((first, rest)) = messages.split_first()
        && first.role == Role::System
    {
        body.insert("instructions".into(), json!(join_text(&first.content)));
        messages = rest;
    }

    let mut input = Vec::new();
    let mut reasoning_dropped = 0;
    for message in messages {
        match message.role {
            Role::System => input.push(json!({
                "role": "developer",
                "content": join_text(&message.content),
            })),
            Role::User => input.push(encode_user(message, warnings)?),
            Role::Assistant => {
                reasoning_dropped += encode_assistant(message, provider, &mut input, warnings);
            }
            Role::Tool => input.push(encode_tool(message, warnings)),
        }
    }
    if reasoning_dropped > 0 {
        warnings.push(Warning::new(
            "reasoning_dropped",
            format!(
                "{reasoning_dropped} reasoning block(s) from another provider are not sent on \
                 this protocol"
            ),
        ));
    }
    body.insert("input".into(), Value::Array(input));

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
            // `strict` defaults to true on this API, which rejects ordinary
            // schemas (optional fields, no `additionalProperties: false`).
            json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.input_schema,
                "strict": false,
            })
        })
        .collect();
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
        let choice = match &request.tool_choice {
            ToolChoice::Auto => None,
            ToolChoice::None => Some(json!("none")),
            ToolChoice::Required => Some(json!("required")),
            ToolChoice::Tool(name) => Some(json!({"type": "function", "name": name})),
        };
        if let Some(choice) = choice {
            body.insert("tool_choice".into(), choice);
        }
        if let Some(parallel) = request.settings.parallel_tool_calls {
            body.insert("parallel_tool_calls".into(), json!(parallel));
        }
    }

    let settings = &request.settings;
    if let Some(max) = settings.max_tokens {
        body.insert("max_output_tokens".into(), json!(max));
    }
    if let Some(temperature) = settings.temperature {
        body.insert("temperature".into(), json!(temperature));
    }
    if !settings.stop.is_empty() {
        warnings.push(Warning::new(
            "stop_dropped",
            "the Responses API has no stop sequences; they were not sent",
        ));
    }
    match settings.reasoning {
        Some(Reasoning::Effort { effort }) => {
            let effort = match effort {
                ReasoningEffort::Low => "low",
                ReasoningEffort::Medium => "medium",
                ReasoningEffort::High => "high",
            };
            let mut reasoning = json!({"effort": effort});
            if let Some(summary) = &options.reasoning_summary {
                reasoning["summary"] = json!(summary);
            }
            body.insert("reasoning".into(), reasoning);
        }
        Some(_) => warnings.push(Warning::new(
            "reasoning_setting_dropped",
            "this endpoint takes a reasoning effort, not a token budget",
        )),
        None => {}
    }
    if let Some(output) = &request.output
        && output.strategy != Some(OutputStrategy::Tool)
    {
        body.insert(
            "text".into(),
            json!({"format": {
                "type": "json_schema",
                "name": "final_output",
                "schema": output.schema,
                "strict": true,
            }}),
        );
    }
    body.insert("store".into(), json!(options.store));
    if options.encrypted_reasoning && !options.store {
        body.insert("include".into(), json!(["reasoning.encrypted_content"]));
    }
    if stream {
        body.insert("stream".into(), json!(true));
    }

    // Provider-specific fields go last, so the caller wins, except for the
    // fields that make the request what it is.
    let keys: &[&str] = if provider == "openai" {
        &["openai"]
    } else {
        &["openai", provider]
    };
    for key in keys {
        if let Some(Value::Object(extra)) = request.extras.get(*key) {
            for (name, value) in extra {
                if matches!(name.as_str(), "model" | "stream" | "input") {
                    warnings.push(Warning::new(
                        "extras_ignored",
                        format!("the `{name}` field cannot be set through extras"),
                    ));
                    continue;
                }
                body.insert(name.clone(), value.clone());
            }
        }
    }
    Ok(Value::Object(body))
}

fn join_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn data_url(mime: Option<&str>, data: &str, fallback: &str) -> String {
    format!("data:{};base64,{data}", mime.unwrap_or(fallback))
}

/// An image as an `input_image` part.
fn image_part(source: &MediaSource, mime_type: Option<&str>) -> Value {
    match source {
        MediaSource::Url { url } => json!({"type": "input_image", "image_url": url}),
        MediaSource::Base64 { data } => json!({
            "type": "input_image",
            "image_url": data_url(mime_type, data, "image/png"),
        }),
        MediaSource::FileId { file_id } => json!({"type": "input_image", "file_id": file_id}),
    }
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
            ContentBlock::Text { text, .. } => {
                parts.push(json!({"type": "input_text", "text": text}));
            }
            ContentBlock::Image {
                source, mime_type, ..
            } => parts.push(image_part(source, mime_type.as_deref())),
            ContentBlock::File {
                source, mime_type, ..
            } => match source {
                MediaSource::FileId { file_id } => {
                    parts.push(json!({"type": "input_file", "file_id": file_id}));
                }
                MediaSource::Base64 { data } => parts.push(json!({
                    "type": "input_file",
                    "filename": "file",
                    "file_data": data_url(mime_type.as_deref(), data, "application/pdf"),
                })),
                MediaSource::Url { url } => {
                    parts.push(json!({"type": "input_file", "file_url": url}));
                }
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
        [single] if single["type"] == "input_text" => single["text"].clone(),
        _ => Value::Array(parts),
    };
    Ok(json!({"role": "user", "content": content}))
}

/// Adds the items of an assistant message, in block order. Returns the
/// number of reasoning blocks dropped (reasoning from another provider).
fn encode_assistant(
    message: &Message,
    provider: &str,
    input: &mut Vec<Value>,
    warnings: &mut Vec<Warning>,
) -> usize {
    let mut text: Vec<&str> = Vec::new();
    let flush = |text: &mut Vec<&str>, input: &mut Vec<Value>| {
        if !text.is_empty() {
            input.push(json!({"role": "assistant", "content": text.join("\n\n")}));
            text.clear();
        }
    };
    let mut dropped = 0;
    for block in &message.content {
        match block {
            ContentBlock::Text { text: t, .. } => text.push(t),
            ContentBlock::ToolCall { id, name, args, .. } => {
                flush(&mut text, input);
                input.push(json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": name,
                    "arguments": Value::Object(args.clone()).to_string(),
                }));
            }
            // Sent as written, so the error result that answers it still
            // has a matching call.
            ContentBlock::InvalidToolCall {
                id, name, raw_args, ..
            } => {
                flush(&mut text, input);
                input.push(json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": name.as_deref().unwrap_or("unknown"),
                    "arguments": raw_args,
                }));
            }
            ContentBlock::Reasoning {
                text: t, extras, ..
            } => {
                let item = extras
                    .get(provider)
                    .and_then(|e| e.get("reasoning"))
                    .and_then(Value::as_object);
                let Some(item) = item.filter(|i| i.get("id").is_some_and(Value::is_string)) else {
                    dropped += 1;
                    continue;
                };
                flush(&mut text, input);
                let summary: Vec<Value> = t
                    .iter()
                    .filter(|t| !t.is_empty())
                    .map(|t| json!({"type": "summary_text", "text": t}))
                    .collect();
                let mut value = json!({"type": "reasoning", "id": item["id"], "summary": summary});
                if let Some(encrypted) = item.get("encrypted_content").filter(|v| v.is_string()) {
                    value["encrypted_content"] = encrypted.clone();
                }
                input.push(value);
            }
            ContentBlock::Provider { provider: p, data } if p == provider => {
                flush(&mut text, input);
                input.push(data.clone());
            }
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
    flush(&mut text, input);
    dropped
}

fn encode_tool(message: &Message, warnings: &mut Vec<Warning>) -> Value {
    let mut text = Vec::new();
    let mut images = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text: t, .. } => text.push(t.clone()),
            ContentBlock::Json { value, .. } => text.push(value.to_string()),
            ContentBlock::Image {
                source, mime_type, ..
            } => images.push(image_part(source, mime_type.as_deref())),
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
    let mut content = text.join("\n");
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
    if content.is_empty() && images.is_empty() {
        content = "(no output)".into();
    }
    let output = if images.is_empty() {
        json!(content)
    } else {
        let mut parts = Vec::new();
        if !content.is_empty() {
            parts.push(json!({"type": "input_text", "text": content}));
        }
        parts.append(&mut images);
        Value::Array(parts)
    };
    json!({
        "type": "function_call_output",
        "call_id": message.call_id.as_deref().unwrap_or_default(),
        "output": output,
    })
}

// ---------- decode ----------

/// The extras that keep a reasoning item's opaque fields, under `provider`.
fn reasoning_extras(item: &Value, provider: &str) -> Option<Extras> {
    let id = item.get("id").filter(|v| v.is_string())?;
    let mut kept = json!({"id": id});
    if let Some(encrypted) = item.get("encrypted_content").filter(|v| v.is_string()) {
        kept["encrypted_content"] = encrypted.clone();
    }
    let mut extras = Extras::new();
    extras.insert(provider.to_string(), json!({"reasoning": kept}));
    Some(extras)
}

/// The readable text of a reasoning item: its summaries, else its raw
/// reasoning text (open models on compatible servers).
fn reasoning_item_text(item: &Value) -> Option<String> {
    let texts = |field: &str| -> Vec<String> {
        item.get(field)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect()
    };
    let mut parts = texts("summary");
    if parts.is_empty() {
        parts = texts("content");
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

fn refusal_extras() -> Extras {
    let mut extras = Extras::new();
    extras.insert("openai".into(), json!({"refusal": true}));
    extras
}

fn annotation_extras(part: &Value, provider: &str) -> Option<Extras> {
    let annotations = part
        .get("annotations")
        .filter(|a| a.as_array().is_some_and(|a| !a.is_empty()))?;
    let mut extras = Extras::new();
    extras.insert(provider.to_string(), json!({"annotations": annotations}));
    Some(extras)
}

/// Decodes a complete (non-streamed) response body.
pub(crate) fn decode(body: &[u8], provider: &str) -> Result<ModelResponse, ModelError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|err| invalid_response(format!("body is not JSON: {err}")))?;
    if let Some(error) = response_error(&value) {
        return Err(error);
    }
    let output = value
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_response("the response has no output"))?;

    let mut assembler = Assembler::new();
    assembler.push(StreamItem::Start {
        provider_message_id: value.get("id").and_then(Value::as_str).map(str::to_string),
    });
    let mut index = 0;
    let mut next = || {
        index += 1;
        index - 1
    };
    let mut calls = false;
    let mut refused = false;
    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("reasoning") => {
                let at = next();
                assembler.push(StreamItem::Delta {
                    index: at,
                    delta: BlockDelta::Reasoning {
                        text: reasoning_item_text(item),
                        signature: None,
                    },
                });
                if let Some(extras) = reasoning_extras(item, provider) {
                    assembler.push(StreamItem::Extras { index: at, extras });
                }
            }
            Some("message") => {
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    match part.get("type").and_then(Value::as_str) {
                        Some("output_text") => {
                            let at = next();
                            let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                            assembler.push(StreamItem::Delta {
                                index: at,
                                delta: BlockDelta::Text {
                                    text: text.to_string(),
                                },
                            });
                            if let Some(extras) = annotation_extras(part, provider) {
                                assembler.push(StreamItem::Extras { index: at, extras });
                            }
                        }
                        Some("refusal") => {
                            refused = true;
                            let text = part.get("refusal").and_then(Value::as_str).unwrap_or("");
                            assembler.push(StreamItem::Delta {
                                index: next(),
                                delta: BlockDelta::Block {
                                    block: ContentBlock::Text {
                                        text: text.to_string(),
                                        extras: refusal_extras(),
                                    },
                                },
                            });
                        }
                        _ => {}
                    }
                }
            }
            Some("function_call") => {
                calls = true;
                let at = next();
                assembler.push(StreamItem::Delta {
                    index: at,
                    delta: BlockDelta::ToolCall {
                        id: text_field(item, "call_id"),
                        name: text_field(item, "name"),
                        args_fragment: arguments(item.get("arguments")),
                    },
                });
                assembler.push(StreamItem::BlockEnd { index: at });
            }
            _ => {
                assembler.push(StreamItem::Delta {
                    index: next(),
                    delta: BlockDelta::Block {
                        block: ContentBlock::Provider {
                            provider: provider.to_string(),
                            data: item.clone(),
                        },
                    },
                });
            }
        }
    }
    if let Some(usage) = value.get("usage").filter(|u| u.is_object()) {
        assembler.push(StreamItem::Usage(usage_from(usage)));
    }
    let (reason, raw) = finish_reason(&value, calls, refused);
    assembler.push(StreamItem::Finish { reason, raw });
    let mut response = assembler.finish()?;
    let metadata = &mut response.message.metadata;
    metadata.provider = Some(provider.to_string());
    metadata.model = text_field(&value, "model");
    Ok(response)
}

fn text_field(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Arguments arrive as a JSON string; some servers send an object.
fn arguments(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// The finish reason of a response object.
fn finish_reason(response: &Value, calls: bool, refused: bool) -> (FinishReason, Option<String>) {
    let status = response.get("status").and_then(Value::as_str);
    let incomplete = response
        .get("incomplete_details")
        .and_then(|d| d.get("reason"))
        .and_then(Value::as_str);
    let reason = match (status, incomplete) {
        _ if refused => FinishReason::ContentFilter,
        (Some("incomplete"), Some("max_output_tokens")) => FinishReason::Length,
        (Some("incomplete"), Some("content_filter")) => FinishReason::ContentFilter,
        (Some("incomplete"), _) => FinishReason::Other,
        _ if calls => FinishReason::ToolCalls,
        (Some("completed") | None, _) => FinishReason::Stop,
        _ => FinishReason::Other,
    };
    let raw = match (status, incomplete) {
        (Some("incomplete"), Some(why)) => Some(format!("incomplete:{why}")),
        (Some(status), _) => Some(status.to_string()),
        (None, _) => None,
    };
    (reason, raw)
}

fn usage_from(value: &Value) -> Usage {
    let number = |path: &[&str]| {
        path.iter()
            .try_fold(value, |v, key| v.get(*key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let mut usage = Usage::new(number(&["input_tokens"]), number(&["output_tokens"]));
    usage.cache_read_tokens = number(&["input_tokens_details", "cached_tokens"]);
    usage.reasoning_tokens = number(&["output_tokens_details", "reasoning_tokens"]);
    usage
}

/// A classified error from an error object (`{"code", "message"}`).
fn classified(error: &Value) -> ModelError {
    let text = json!({"error": error}).to_string();
    // Status 0: the kind comes from the in-band code, else `unknown`.
    let mut error = classify_http(&HttpFailure::new(0, &[], &text), std::time::UNIX_EPOCH);
    error.status = None;
    // A response that failed on the server after it was accepted: unlike an
    // HTTP error body, `server_error` here is the whole story, and retryable.
    let code = error_code(error_value(&text));
    if error.kind == ModelErrorKind::Unknown && matches!(code.as_deref(), Some("server_error")) {
        error.kind = ModelErrorKind::Server;
    }
    error
}

fn error_value(text: &str) -> Value {
    serde_json::from_str::<Value>(text).unwrap_or(Value::Null)
}

fn error_code(value: Value) -> Option<String> {
    value
        .get("error")
        .and_then(|e| e.get("code"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The error of a failed response, or of an `{"error": …}` body.
fn response_error(response: &Value) -> Option<ModelError> {
    let error = response.get("error").filter(|e| e.is_object());
    let failed = response.get("status").and_then(Value::as_str) == Some("failed");
    match (error, failed) {
        (Some(error), _) => Some(classified(error)),
        (None, true) => Some(classified(
            &json!({"code": "server_error", "message": "the response failed"}),
        )),
        _ => None,
    }
}

// ---------- streaming ----------

/// What an output item of a stream is, and where its block is.
#[derive(Debug)]
enum Item {
    Reasoning {
        index: u32,
        text_seen: bool,
        last_summary: Option<u64>,
    },
    Message,
    Call {
        index: u32,
        args_seen: bool,
    },
    Other,
}

/// Decodes the Responses event stream into [`StreamItem`]s.
#[derive(Debug, Default)]
pub(crate) struct EventDecoder {
    done: bool,
    model: Option<String>,
    next_index: u32,
    provider: String,
    /// `output_index` → the item.
    items: HashMap<u64, Item>,
    /// (`output_index`, `content_index`) → text block (refusals included).
    parts: HashMap<(u64, u64), u32>,
    calls: bool,
    refused: bool,
}

impl EventDecoder {
    /// A decoder for the endpoint named `provider`.
    pub(crate) fn new(provider: &str) -> Self {
        Self {
            provider: provider.to_string(),
            ..Self::default()
        }
    }

    /// The response ended (completed, incomplete or failed).
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

    fn part(&mut self, output: u64, content: u64) -> (u32, bool) {
        if let Some(&index) = self.parts.get(&(output, content)) {
            return (index, false);
        }
        let index = self.allocate();
        self.parts.insert((output, content), index);
        (index, true)
    }

    fn note_model(&mut self, response: &Value) {
        if self.model.is_none() {
            self.model = text_field(response, "model");
        }
    }

    pub(crate) fn on_event(&mut self, event: &SseEvent) -> Result<Vec<StreamItem>, ModelError> {
        let mut out = Vec::new();
        let data = event.data.trim();
        if data.is_empty() || data == "[DONE]" {
            return Ok(out);
        }
        let value: Value = serde_json::from_str(data)
            .map_err(|err| invalid_response(format!("stream event is not JSON: {err}")))?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(event.event.as_deref().unwrap_or(""));
        let output_index = value.get("output_index").and_then(Value::as_u64);
        let content_index = value
            .get("content_index")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let delta = value.get("delta").and_then(Value::as_str).unwrap_or("");
        match kind {
            "response.created" => {
                let response = value.get("response").unwrap_or(&Value::Null);
                self.note_model(response);
                out.push(StreamItem::Start {
                    provider_message_id: text_field(response, "id"),
                });
            }
            "response.output_item.added" => {
                let (Some(at), Some(item)) = (output_index, value.get("item")) else {
                    return Ok(out);
                };
                let entry = match item.get("type").and_then(Value::as_str) {
                    Some("reasoning") => {
                        let index = self.allocate();
                        out.push(StreamItem::Delta {
                            index,
                            delta: BlockDelta::Reasoning {
                                text: None,
                                signature: None,
                            },
                        });
                        Item::Reasoning {
                            index,
                            text_seen: false,
                            last_summary: None,
                        }
                    }
                    Some("message") => Item::Message,
                    Some("function_call") => {
                        self.calls = true;
                        let index = self.allocate();
                        let args = arguments(item.get("arguments"));
                        out.push(StreamItem::Delta {
                            index,
                            delta: BlockDelta::ToolCall {
                                id: text_field(item, "call_id"),
                                name: text_field(item, "name"),
                                args_fragment: args.clone(),
                            },
                        });
                        Item::Call {
                            index,
                            args_seen: !args.is_empty(),
                        }
                    }
                    _ => Item::Other,
                };
                self.items.insert(at, entry);
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let Some(at) = output_index else {
                    return Ok(out);
                };
                let (index, _) = self.part(at, content_index);
                if kind == "response.refusal.delta" && !self.refused {
                    self.refused = true;
                    out.push(StreamItem::Extras {
                        index,
                        extras: refusal_extras(),
                    });
                }
                if !delta.is_empty() {
                    out.push(StreamItem::Delta {
                        index,
                        delta: BlockDelta::Text {
                            text: delta.to_string(),
                        },
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(Item::Call { index, args_seen }) =
                    output_index.and_then(|at| self.items.get_mut(&at))
                    && !delta.is_empty()
                {
                    *args_seen = true;
                    out.push(StreamItem::Delta {
                        index: *index,
                        delta: BlockDelta::ToolCall {
                            id: None,
                            name: None,
                            args_fragment: delta.to_string(),
                        },
                    });
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let summary = value
                    .get("summary_index")
                    .or_else(|| value.get("content_index"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if let Some(Item::Reasoning {
                    index,
                    text_seen,
                    last_summary,
                }) = output_index.and_then(|at| self.items.get_mut(&at))
                    && !delta.is_empty()
                {
                    let mut text = String::new();
                    if *text_seen && *last_summary != Some(summary) {
                        text.push_str("\n\n");
                    }
                    text.push_str(delta);
                    *text_seen = true;
                    *last_summary = Some(summary);
                    out.push(StreamItem::Delta {
                        index: *index,
                        delta: BlockDelta::Reasoning {
                            text: Some(text),
                            signature: None,
                        },
                    });
                }
            }
            "response.output_item.done" => {
                let (Some(at), Some(item)) = (output_index, value.get("item")) else {
                    return Ok(out);
                };
                self.item_done(at, item, &mut out);
            }
            "response.completed" | "response.incomplete" => {
                let response = value.get("response").unwrap_or(&Value::Null);
                self.note_model(response);
                if let Some(usage) = response.get("usage").filter(|u| u.is_object()) {
                    out.push(StreamItem::Usage(usage_from(usage)));
                }
                let (reason, raw) = finish_reason(response, self.calls, self.refused);
                out.push(StreamItem::Finish { reason, raw });
                self.done = true;
            }
            "response.failed" => {
                self.done = true;
                let response = value.get("response").unwrap_or(&Value::Null);
                return Err(response_error(response).unwrap_or_else(|| {
                    classified(&json!({"code": "server_error", "message": "the response failed"}))
                }));
            }
            "error" => {
                self.done = true;
                let error = value
                    .get("error")
                    .filter(|e| e.is_object())
                    .unwrap_or(&value);
                return Err(classified(error));
            }
            _ => {}
        }
        Ok(out)
    }

    /// An output item is complete: anything the deltas did not carry (a
    /// server that sends whole items, reasoning's opaque fields, hosted
    /// tool calls) comes from the item itself.
    fn item_done(&mut self, at: u64, item: &Value, out: &mut Vec<StreamItem>) {
        let entry = self.items.remove(&at);
        match (item.get("type").and_then(Value::as_str), entry) {
            (
                Some("reasoning"),
                Some(Item::Reasoning {
                    index, text_seen, ..
                }),
            ) => {
                if !text_seen && let Some(text) = reasoning_item_text(item) {
                    out.push(StreamItem::Delta {
                        index,
                        delta: BlockDelta::Reasoning {
                            text: Some(text),
                            signature: None,
                        },
                    });
                }
                if let Some(extras) = reasoning_extras(item, &self.provider) {
                    out.push(StreamItem::Extras { index, extras });
                }
            }
            (Some("function_call"), Some(Item::Call { index, args_seen })) => {
                if !args_seen {
                    let args = arguments(item.get("arguments"));
                    if !args.is_empty() {
                        out.push(StreamItem::Delta {
                            index,
                            delta: BlockDelta::ToolCall {
                                id: text_field(item, "call_id"),
                                name: text_field(item, "name"),
                                args_fragment: args,
                            },
                        });
                    }
                }
                out.push(StreamItem::BlockEnd { index });
            }
            (Some("message"), _) => {
                for (position, part) in item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    let (index, new) = self.part(at, position as u64);
                    match part.get("type").and_then(Value::as_str) {
                        Some("output_text") => {
                            let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                            if new && !text.is_empty() {
                                out.push(StreamItem::Delta {
                                    index,
                                    delta: BlockDelta::Text {
                                        text: text.to_string(),
                                    },
                                });
                            }
                            if let Some(extras) = annotation_extras(part, &self.provider) {
                                out.push(StreamItem::Extras { index, extras });
                            }
                        }
                        Some("refusal") if new => {
                            self.refused = true;
                            let text = part.get("refusal").and_then(Value::as_str);
                            out.push(StreamItem::Delta {
                                index,
                                delta: BlockDelta::Block {
                                    block: ContentBlock::Text {
                                        text: text.unwrap_or("").to_string(),
                                        extras: refusal_extras(),
                                    },
                                },
                            });
                        }
                        _ => {}
                    }
                }
            }
            (Some("function_call"), _) => {
                // Never announced: the whole call at once.
                self.calls = true;
                let index = self.allocate();
                out.push(StreamItem::Delta {
                    index,
                    delta: BlockDelta::ToolCall {
                        id: text_field(item, "call_id"),
                        name: text_field(item, "name"),
                        args_fragment: arguments(item.get("arguments")),
                    },
                });
                out.push(StreamItem::BlockEnd { index });
            }
            (Some("reasoning"), _) => {
                let index = self.allocate();
                out.push(StreamItem::Delta {
                    index,
                    delta: BlockDelta::Reasoning {
                        text: reasoning_item_text(item),
                        signature: None,
                    },
                });
                if let Some(extras) = reasoning_extras(item, &self.provider) {
                    out.push(StreamItem::Extras { index, extras });
                }
            }
            _ => {
                let index = self.allocate();
                out.push(StreamItem::Delta {
                    index,
                    delta: BlockDelta::Block {
                        block: ContentBlock::Provider {
                            provider: self.provider.clone(),
                            data: item.clone(),
                        },
                    },
                });
            }
        }
    }
}
