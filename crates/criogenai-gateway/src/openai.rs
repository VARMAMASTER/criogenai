//! The OpenAI chat completions wire format, server side (design 0006 §7,
//! §12.2): client requests decoded into 0002, answers and stream deltas
//! encoded back. The mirror of `criogenai_providers`' client codec.

use std::collections::{BTreeMap, HashMap};

use criogenai_providers::{
    BlockDelta, ContentBlock, FinishReason, MediaSource, Message, MessageDelta, ModelRequest,
    ModelResponse, OutputSpec, OutputStrategy, Role, Settings, ToolChoice, ToolSpec, ToolStatus,
    Usage,
};
use serde_json::{Map, Value, json};

use crate::GatewayError;

/// A decoded `POST /v1/chat/completions`.
#[derive(Debug)]
pub(crate) struct ChatRequest {
    /// The route the client asked for (`model`).
    pub route: String,
    pub request: ModelRequest,
    pub stream: bool,
    /// `stream_options.include_usage`.
    pub include_usage: bool,
}

fn invalid(message: impl Into<String>) -> GatewayError {
    GatewayError::InvalidRequest(message.into())
}

/// Decodes a client request.
pub(crate) fn decode_request(body: &Value) -> Result<ChatRequest, GatewayError> {
    let body = body
        .as_object()
        .ok_or_else(|| invalid("the body must be a JSON object"))?;
    let route = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("`model` is required"))?
        .to_string();
    if body
        .get("n")
        .and_then(Value::as_u64)
        .is_some_and(|n| n != 1)
    {
        return Err(invalid("only `n: 1` is supported"));
    }
    let raw_messages = body
        .get("messages")
        .and_then(Value::as_array)
        .filter(|m| !m.is_empty())
        .ok_or_else(|| invalid("`messages` must be a non-empty array"))?;
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut messages = Vec::with_capacity(raw_messages.len());
    for (i, raw) in raw_messages.iter().enumerate() {
        messages.push(
            decode_message(raw, &mut tool_names)
                .map_err(|why| invalid(format!("messages[{i}]: {why}")))?,
        );
    }

    let mut request = ModelRequest::new(messages);
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        request.tools = tools
            .iter()
            .enumerate()
            .map(|(i, t)| decode_tool(t).map_err(|why| invalid(format!("tools[{i}]: {why}"))))
            .collect::<Result<_, _>>()?;
    }
    if let Some(choice) = body.get("tool_choice") {
        request.tool_choice = match choice {
            Value::String(s) if s == "auto" => ToolChoice::Auto,
            Value::String(s) if s == "none" => ToolChoice::None,
            Value::String(s) if s == "required" => ToolChoice::Required,
            other => match other.pointer("/function/name").and_then(Value::as_str) {
                Some(name) => ToolChoice::Tool(name.to_string()),
                None => return Err(invalid("unsupported `tool_choice`")),
            },
        };
    }
    if let Some(format) = body.get("response_format") {
        match format.get("type").and_then(Value::as_str) {
            Some("json_schema") => {
                let schema = format
                    .pointer("/json_schema/schema")
                    .cloned()
                    .ok_or_else(|| invalid("`response_format.json_schema.schema` is required"))?;
                let mut output = OutputSpec::new(schema);
                output.strategy = Some(OutputStrategy::Native);
                request.output = Some(output);
            }
            Some("json_object") => {
                let mut output = OutputSpec::new(json!({"type": "object"}));
                output.strategy = Some(OutputStrategy::Native);
                request.output = Some(output);
            }
            Some("text") | None => {}
            Some(other) => return Err(invalid(format!("unsupported response_format `{other}`"))),
        }
    }
    request.settings = settings(body)?;
    let stream = body.get("stream").and_then(Value::as_bool) == Some(true);
    let include_usage = body
        .get("stream_options")
        .and_then(|o| o.get("include_usage"))
        .and_then(Value::as_bool)
        == Some(true);
    Ok(ChatRequest {
        route,
        request,
        stream,
        include_usage,
    })
}

fn settings(body: &Map<String, Value>) -> Result<Settings, GatewayError> {
    let mut settings = Map::new();
    if let Some(n) = body
        .get("max_completion_tokens")
        .or_else(|| body.get("max_tokens"))
        .filter(|v| !v.is_null())
    {
        settings.insert("max_tokens".into(), n.clone());
    }
    if let Some(t) = body.get("temperature").filter(|v| !v.is_null()) {
        settings.insert("temperature".into(), t.clone());
    }
    match body.get("stop") {
        Some(Value::String(s)) => {
            settings.insert("stop".into(), json!([s]));
        }
        Some(Value::Array(list)) => {
            settings.insert("stop".into(), Value::Array(list.clone()));
        }
        _ => {}
    }
    if let Some(p) = body.get("parallel_tool_calls").filter(|v| v.is_boolean()) {
        settings.insert("parallel_tool_calls".into(), p.clone());
    }
    if let Some(effort) = body.get("reasoning_effort").and_then(Value::as_str) {
        settings.insert("reasoning".into(), json!({"effort": effort}));
    }
    serde_json::from_value(Value::Object(settings))
        .map_err(|err| invalid(format!("invalid settings: {err}")))
}

fn decode_tool(tool: &Value) -> Result<ToolSpec, String> {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return Err("only `function` tools are supported".into());
    }
    let function = tool.get("function").ok_or("`function` is required")?;
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .ok_or("`function.name` is required")?;
    let description = function
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let schema = function
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    Ok(ToolSpec::new(name, description, schema))
}

/// Text of a `content` that is a string or an array of text parts.
fn content_text(content: Option<&Value>) -> Result<String, String> {
    match content {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(Value::Array(parts)) => {
            let mut text = String::new();
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => text.push_str(part["text"].as_str().unwrap_or_default()),
                    other => return Err(format!("unsupported content part {other:?} here")),
                }
            }
            Ok(text)
        }
        Some(_) => Err("`content` must be a string or an array".into()),
    }
}

/// A `data:<mime>;base64,<data>` URL as (mime, data), or a plain URL.
fn image_source(url: &str) -> (MediaSource, Option<String>) {
    if let Some(rest) = url.strip_prefix("data:")
        && let Some((meta, data)) = rest.split_once(',')
        && let Some(mime) = meta.strip_suffix(";base64")
    {
        return (
            MediaSource::Base64 {
                data: data.to_string(),
            },
            Some(mime.to_string()),
        );
    }
    (
        MediaSource::Url {
            url: url.to_string(),
        },
        None,
    )
}

fn user_content(content: Option<&Value>) -> Result<Vec<ContentBlock>, String> {
    match content {
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|part| match part.get("type").and_then(Value::as_str) {
                Some("text") => Ok(ContentBlock::text(
                    part["text"].as_str().unwrap_or_default(),
                )),
                Some("image_url") => {
                    let url = part
                        .pointer("/image_url/url")
                        .and_then(Value::as_str)
                        .ok_or("`image_url.url` is required")?;
                    let (source, mime) = image_source(url);
                    Ok(ContentBlock::image(source, mime))
                }
                other => Err(format!("unsupported content part {other:?}")),
            })
            .collect(),
        other => Ok(vec![ContentBlock::text(content_text(other)?)]),
    }
}

fn decode_message(
    raw: &Value,
    tool_names: &mut HashMap<String, String>,
) -> Result<Message, String> {
    let role = raw
        .get("role")
        .and_then(Value::as_str)
        .ok_or("`role` is required")?;
    let content = raw.get("content");
    let mut message = match role {
        "system" | "developer" => Message::system(content_text(content)?),
        "user" => Message::with_content(Role::User, user_content(content)?),
        "assistant" => {
            let mut blocks = Vec::new();
            let text = content_text(content)?;
            if !text.is_empty() {
                blocks.push(ContentBlock::text(text));
            }
            for call in raw
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let id = call["id"].as_str().ok_or("a tool call needs an `id`")?;
                let name = call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .ok_or("a tool call needs `function.name`")?;
                let arguments = call
                    .pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                let args: Map<String, Value> = if arguments.trim().is_empty() {
                    Map::new()
                } else {
                    serde_json::from_str(arguments)
                        .map_err(|err| format!("tool call `{id}` arguments: {err}"))?
                };
                tool_names.insert(id.to_string(), name.to_string());
                blocks.push(ContentBlock::tool_call(id, name, args));
            }
            Message::with_content(Role::Assistant, blocks)
        }
        "tool" => {
            let call_id = raw
                .get("tool_call_id")
                .and_then(Value::as_str)
                .ok_or("a tool message needs `tool_call_id`")?;
            let tool = tool_names.get(call_id).cloned().unwrap_or_default();
            Message::tool_result(
                call_id,
                tool,
                ToolStatus::Success,
                vec![ContentBlock::text(content_text(content)?)],
            )
        }
        other => return Err(format!("unknown role `{other}`")),
    };
    if let Some(name) = raw.get("name").and_then(Value::as_str)
        && role != "tool"
    {
        message = message.named(name);
    }
    Ok(message)
}

// ---------- encoding ----------

fn finish_reason(reason: FinishReason) -> &'static str {
    match reason {
        FinishReason::ToolCalls => "tool_calls",
        FinishReason::Length => "length",
        FinishReason::ContentFilter => "content_filter",
        _ => "stop",
    }
}

pub(crate) fn usage_json(usage: &Usage) -> Value {
    json!({
        "prompt_tokens": usage.input_tokens,
        "completion_tokens": usage.output_tokens,
        "total_tokens": usage.input_tokens + usage.output_tokens,
        "prompt_tokens_details": {"cached_tokens": usage.cache_read_tokens},
        "completion_tokens_details": {"reasoning_tokens": usage.reasoning_tokens},
    })
}

fn tool_call_json(id: &str, name: &str, args: &Map<String, Value>) -> Value {
    json!({
        "id": id,
        "type": "function",
        "function": {"name": name, "arguments": Value::Object(args.clone()).to_string()},
    })
}

/// The assistant message of an answer, in OpenAI's shape.
fn message_json(message: &Message) -> Value {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut calls = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text: t, .. } => text.push_str(t),
            ContentBlock::Json { value, .. } => text.push_str(&value.to_string()),
            ContentBlock::Reasoning { text: Some(t), .. } => reasoning.push_str(t),
            ContentBlock::ToolCall { id, name, args, .. } => {
                calls.push(tool_call_json(id, name, args))
            }
            ContentBlock::InvalidToolCall {
                id, name, raw_args, ..
            } => calls.push(json!({
                "id": id, "type": "function",
                "function": {"name": name.clone().unwrap_or_default(), "arguments": raw_args},
            })),
            _ => {}
        }
    }
    let mut out = json!({
        "role": "assistant",
        "content": if text.is_empty() && !calls.is_empty() { Value::Null } else { Value::String(text) },
    });
    if !calls.is_empty() {
        out["tool_calls"] = Value::Array(calls);
    }
    if !reasoning.is_empty() {
        out["reasoning_content"] = Value::String(reasoning);
    }
    out
}

/// A complete (non-streamed) answer.
pub(crate) fn encode_response(
    id: &str,
    created: u64,
    route: &str,
    response: &ModelResponse,
) -> Value {
    json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": route,
        "choices": [{
            "index": 0,
            "message": message_json(&response.message),
            "finish_reason": finish_reason(response.finish_reason),
        }],
        "usage": usage_json(&response.usage),
    })
}

/// Turns one answer's deltas into `chat.completion.chunk`s, and at the end
/// sends whatever the deltas did not carry (some upstreams stream text
/// only, or nothing).
#[derive(Debug)]
pub(crate) struct ChunkEncoder {
    id: String,
    created: u64,
    route: String,
    /// Block index → tool call ordinal, for tool calls already started.
    tool_calls: BTreeMap<u32, usize>,
    /// Block indexes whose content went out as deltas.
    streamed: BTreeMap<u32, ()>,
    started: bool,
}

impl ChunkEncoder {
    pub(crate) fn new(id: String, created: u64, route: String) -> Self {
        Self {
            id,
            created,
            route,
            tool_calls: BTreeMap::new(),
            streamed: BTreeMap::new(),
            started: false,
        }
    }

    fn chunk(&self, delta: Value, finish: Option<&str>) -> Value {
        json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.route,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        })
    }

    /// The first chunk (the role), once.
    fn start(&mut self, out: &mut Vec<Value>) {
        if !self.started {
            self.started = true;
            out.push(self.chunk(json!({"role": "assistant", "content": ""}), None));
        }
    }

    fn tool_ordinal(&mut self, index: u32) -> (usize, bool) {
        let next = self.tool_calls.len();
        let mut new = false;
        let ordinal = *self.tool_calls.entry(index).or_insert_with(|| {
            new = true;
            next
        });
        (ordinal, new)
    }

    /// Chunks for one delta.
    pub(crate) fn delta(&mut self, delta: &MessageDelta) -> Vec<Value> {
        let mut out = Vec::new();
        self.start(&mut out);
        match &delta.delta {
            BlockDelta::Text { text } => {
                self.streamed.insert(delta.index, ());
                out.push(self.chunk(json!({"content": text}), None));
            }
            BlockDelta::Reasoning {
                text: Some(text), ..
            } => {
                self.streamed.insert(delta.index, ());
                out.push(self.chunk(json!({"reasoning_content": text}), None));
            }
            BlockDelta::ToolCall {
                id,
                name,
                args_fragment,
            } => {
                self.streamed.insert(delta.index, ());
                let (ordinal, new) = self.tool_ordinal(delta.index);
                let mut call = json!({"index": ordinal, "function": {"arguments": args_fragment}});
                if new {
                    call["type"] = json!("function");
                }
                if let Some(id) = id {
                    call["id"] = json!(id);
                }
                if let Some(name) = name {
                    call["function"]["name"] = json!(name);
                }
                out.push(self.chunk(json!({"tool_calls": [call]}), None));
            }
            _ => {}
        }
        out
    }

    /// The last chunks: unstreamed content, the finish reason, and usage
    /// when asked for.
    pub(crate) fn finish(&mut self, response: &ModelResponse, include_usage: bool) -> Vec<Value> {
        let mut out = Vec::new();
        self.start(&mut out);
        for (index, block) in response.message.content.iter().enumerate() {
            let index = index as u32;
            if self.streamed.contains_key(&index) {
                continue;
            }
            match block {
                ContentBlock::Text { text, .. } if !text.is_empty() => {
                    out.push(self.chunk(json!({"content": text}), None));
                }
                ContentBlock::Json { value, .. } => {
                    out.push(self.chunk(json!({"content": value.to_string()}), None));
                }
                ContentBlock::ToolCall { id, name, args, .. } => {
                    let (ordinal, _) = self.tool_ordinal(index);
                    let mut call = tool_call_json(id, name, args);
                    call["index"] = json!(ordinal);
                    out.push(self.chunk(json!({"tool_calls": [call]}), None));
                }
                _ => {}
            }
        }
        out.push(self.chunk(json!({}), Some(finish_reason(response.finish_reason))));
        if include_usage {
            out.push(json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "created": self.created,
                "model": self.route,
                "choices": [],
                "usage": usage_json(&response.usage),
            }));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_decode_into_0002() {
        let body = json!({
            "model": "support",
            "messages": [
                {"role": "developer", "content": "Be brief."},
                {"role": "user", "content": [
                    {"type": "text", "text": "What is this?"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgo="}},
                ]},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "lookup", "arguments": "{\"q\":\"x\"}"}},
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "found"},
            ],
            "tools": [{"type": "function", "function": {"name": "lookup", "parameters": {"type": "object"}}}],
            "tool_choice": {"type": "function", "function": {"name": "lookup"}},
            "response_format": {"type": "json_schema", "json_schema": {"name": "r", "schema": {"type": "object"}}},
            "max_completion_tokens": 50, "temperature": 0.2, "stop": "END",
            "reasoning_effort": "low", "stream": true, "stream_options": {"include_usage": true},
        });
        let decoded = decode_request(&body).unwrap();
        assert_eq!(decoded.route, "support");
        assert!(decoded.stream && decoded.include_usage);
        let r = decoded.request;
        assert_eq!(r.messages.len(), 4);
        assert_eq!(r.messages[0].role, Role::System);
        assert!(matches!(
            &r.messages[1].content[1],
            ContentBlock::Image { mime_type: Some(m), source: MediaSource::Base64 { .. }, .. } if m == "image/png"
        ));
        assert_eq!(r.messages[2].tool_calls().next().unwrap().name, "lookup");
        assert_eq!(r.messages[3].tool.as_deref(), Some("lookup"));
        assert_eq!(r.tools[0].name, "lookup");
        assert_eq!(r.tool_choice, ToolChoice::Tool("lookup".into()));
        assert!(r.output.is_some());
        assert_eq!(r.settings.max_tokens, Some(50));
        assert_eq!(r.settings.stop, ["END"]);
        assert!(r.settings.reasoning.is_some());
    }

    #[test]
    fn bad_requests_say_what_is_wrong() {
        for (body, needle) in [
            (json!([]), "JSON object"),
            (
                json!({"messages": [{"role": "user", "content": "x"}]}),
                "`model`",
            ),
            (json!({"model": "m", "messages": []}), "`messages`"),
            (
                json!({"model": "m", "n": 2, "messages": [{"role": "user", "content": "x"}]}),
                "n: 1",
            ),
            (
                json!({"model": "m", "messages": [{"role": "robot", "content": "x"}]}),
                "messages[0]: unknown role",
            ),
            (
                json!({"model": "m", "messages": [{"role": "assistant", "tool_calls": [
                    {"id": "c", "function": {"name": "f", "arguments": "{bad"}}]}]}),
                "arguments",
            ),
        ] {
            let err = decode_request(&body).unwrap_err().to_string();
            assert!(err.contains(needle), "{err} (wanted {needle})");
        }
    }
}
