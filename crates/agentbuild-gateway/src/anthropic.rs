//! The Anthropic Messages wire format, server side (design 0006 §13.1):
//! `POST /v1/messages` requests decoded into 0002, answers and stream
//! deltas encoded back as Anthropic's events. The mirror of
//! `agentbuild_providers`' Anthropic client.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use agentbuild_providers::{
    BlockDelta, ContentBlock, FinishReason, MediaSource, Message, MessageDelta, ModelRequest,
    ModelResponse, Role, Settings, ToolChoice, ToolSpec, ToolStatus, Usage,
};
use serde_json::{Map, Value, json};

use crate::GatewayError;

/// A decoded `POST /v1/messages`.
#[derive(Debug)]
pub(crate) struct MessagesRequest {
    /// The route the client asked for (`model`).
    pub route: String,
    pub request: ModelRequest,
    pub stream: bool,
}

fn invalid(message: impl Into<String>) -> GatewayError {
    GatewayError::InvalidRequest(message.into())
}

/// Decodes a client request.
pub(crate) fn decode_request(body: &Value) -> Result<MessagesRequest, GatewayError> {
    let body = body
        .as_object()
        .ok_or_else(|| invalid("the body must be a JSON object"))?;
    let route = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("`model` is required"))?
        .to_string();
    let max_tokens = body
        .get("max_tokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("`max_tokens` is required"))?;
    let raw_messages = body
        .get("messages")
        .and_then(Value::as_array)
        .filter(|m| !m.is_empty())
        .ok_or_else(|| invalid("`messages` must be a non-empty array"))?;

    let mut messages = Vec::with_capacity(raw_messages.len() + 1);
    match body.get("system") {
        None | Some(Value::Null) => {}
        Some(system) => {
            let text = text_of(system).map_err(|why| invalid(format!("system: {why}")))?;
            messages.push(Message::system(text));
        }
    }
    let mut tool_names: HashMap<String, String> = HashMap::new();
    for (i, raw) in raw_messages.iter().enumerate() {
        decode_message(raw, &mut tool_names, &mut messages)
            .map_err(|why| invalid(format!("messages[{i}]: {why}")))?;
    }

    let mut request = ModelRequest::new(messages);
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        request.tools = tools
            .iter()
            .enumerate()
            .map(|(i, t)| decode_tool(t).map_err(|why| invalid(format!("tools[{i}]: {why}"))))
            .collect::<Result<_, _>>()?;
    }
    if let Some(choice) = body.get("tool_choice").filter(|c| !c.is_null()) {
        request.tool_choice = match choice.get("type").and_then(Value::as_str) {
            Some("auto") => ToolChoice::Auto,
            Some("any") => ToolChoice::Required,
            Some("none") => ToolChoice::None,
            Some("tool") => match choice.get("name").and_then(Value::as_str) {
                Some(name) => ToolChoice::Tool(name.to_string()),
                None => return Err(invalid("`tool_choice` of type `tool` needs a `name`")),
            },
            _ => return Err(invalid("unsupported `tool_choice`")),
        };
    }
    let mut settings = Map::new();
    settings.insert("max_tokens".into(), json!(max_tokens));
    if let Some(t) = body.get("temperature").filter(|v| !v.is_null()) {
        settings.insert("temperature".into(), t.clone());
    }
    if let Some(stop) = body.get("stop_sequences").filter(|v| v.is_array()) {
        settings.insert("stop".into(), stop.clone());
    }
    request.settings = serde_json::from_value::<Settings>(Value::Object(settings))
        .map_err(|err| invalid(format!("invalid settings: {err}")))?;
    Ok(MessagesRequest {
        route,
        request,
        stream: body.get("stream").and_then(Value::as_bool) == Some(true),
    })
}

fn decode_tool(tool: &Value) -> Result<ToolSpec, String> {
    let name = tool
        .get("name")
        .and_then(Value::as_str)
        .ok_or("`name` is required")?;
    if tool
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t != "custom")
    {
        return Err(format!(
            "only client tools are supported, not `{}`",
            tool["type"]
        ));
    }
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let schema = tool
        .get("input_schema")
        .cloned()
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    Ok(ToolSpec::new(name, description, schema))
}

/// Text of a string or an array of text blocks.
fn text_of(value: &Value) -> Result<String, String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Array(blocks) => {
            let mut text = String::new();
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => text.push_str(block["text"].as_str().unwrap_or_default()),
                    other => return Err(format!("unsupported block {other:?} here")),
                }
            }
            Ok(text)
        }
        _ => Err("must be a string or an array of blocks".into()),
    }
}

fn image(block: &Value) -> Result<ContentBlock, String> {
    let source = block.get("source").ok_or("an image needs a `source`")?;
    match source.get("type").and_then(Value::as_str) {
        Some("base64") => Ok(ContentBlock::image(
            MediaSource::Base64 {
                data: source["data"].as_str().unwrap_or_default().to_string(),
            },
            source["media_type"].as_str().map(str::to_string),
        )),
        Some("url") => Ok(ContentBlock::image(
            MediaSource::Url {
                url: source["url"]
                    .as_str()
                    .ok_or("an image URL source needs a `url`")?
                    .to_string(),
            },
            None,
        )),
        other => Err(format!("unsupported image source {other:?}")),
    }
}

/// One Anthropic message, which may become several of ours: its
/// `tool_result` blocks are tool messages, in order, before the rest.
fn decode_message(
    raw: &Value,
    tool_names: &mut HashMap<String, String>,
    out: &mut Vec<Message>,
) -> Result<(), String> {
    let role = raw
        .get("role")
        .and_then(Value::as_str)
        .ok_or("`role` is required")?;
    let blocks: Vec<Value> = match raw.get("content") {
        Some(Value::String(text)) => vec![json!({"type": "text", "text": text})],
        Some(Value::Array(blocks)) => blocks.clone(),
        _ => return Err("`content` must be a string or an array of blocks".into()),
    };
    match role {
        "user" => {
            let mut content = Vec::new();
            for block in &blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => content.push(ContentBlock::text(
                        block["text"].as_str().unwrap_or_default(),
                    )),
                    Some("image") => content.push(image(block)?),
                    Some("tool_result") => {
                        let call_id = block
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .ok_or("a tool_result needs `tool_use_id`")?;
                        let mut result = Vec::new();
                        match block.get("content") {
                            None | Some(Value::Null) => {}
                            Some(Value::String(text)) => result.push(ContentBlock::text(text)),
                            Some(Value::Array(parts)) => {
                                for part in parts {
                                    match part.get("type").and_then(Value::as_str) {
                                        Some("text") => result.push(ContentBlock::text(
                                            part["text"].as_str().unwrap_or_default(),
                                        )),
                                        Some("image") => result.push(image(part)?),
                                        other => {
                                            return Err(format!(
                                                "unsupported tool_result block {other:?}"
                                            ));
                                        }
                                    }
                                }
                            }
                            Some(_) => return Err("tool_result `content` is invalid".into()),
                        }
                        let status = if block.get("is_error").and_then(Value::as_bool) == Some(true)
                        {
                            ToolStatus::Error
                        } else {
                            ToolStatus::Success
                        };
                        let tool = tool_names.get(call_id).cloned().unwrap_or_default();
                        out.push(Message::tool_result(call_id, tool, status, result));
                    }
                    other => return Err(format!("unsupported user block {other:?}")),
                }
            }
            if !content.is_empty() {
                out.push(Message::with_content(Role::User, content));
            }
        }
        "assistant" => {
            let mut content = Vec::new();
            for block in &blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => content.push(ContentBlock::text(
                        block["text"].as_str().unwrap_or_default(),
                    )),
                    Some("tool_use") => {
                        let id = block["id"].as_str().ok_or("a tool_use needs an `id`")?;
                        let name = block["name"].as_str().ok_or("a tool_use needs a `name`")?;
                        let args = match block.get("input") {
                            Some(Value::Object(args)) => args.clone(),
                            None | Some(Value::Null) => Map::new(),
                            Some(_) => {
                                return Err(format!("tool_use `{id}` input is not an object"));
                            }
                        };
                        tool_names.insert(id.to_string(), name.to_string());
                        content.push(ContentBlock::tool_call(id, name, args));
                    }
                    // Earlier reasoning cannot go to another provider.
                    Some("thinking" | "redacted_thinking") => {}
                    other => return Err(format!("unsupported assistant block {other:?}")),
                }
            }
            out.push(Message::with_content(Role::Assistant, content));
        }
        other => return Err(format!("unknown role `{other}`")),
    }
    Ok(())
}

// ---------- encoding ----------

fn stop_reason(reason: FinishReason) -> &'static str {
    match reason {
        FinishReason::ToolCalls => "tool_use",
        FinishReason::Length => "max_tokens",
        FinishReason::ContentFilter => "refusal",
        _ => "end_turn",
    }
}

/// Anthropic counts cache reads and writes outside `input_tokens`; ours
/// includes them (0002 §1).
fn usage_json(usage: &Usage) -> Value {
    json!({
        "input_tokens": usage
            .input_tokens
            .saturating_sub(usage.cache_read_tokens + usage.cache_write_tokens),
        "output_tokens": usage.output_tokens,
        "cache_read_input_tokens": usage.cache_read_tokens,
        "cache_creation_input_tokens": usage.cache_write_tokens,
    })
}

/// A block of an answer in Anthropic's shape; `None` for blocks Anthropic
/// clients have no use for.
fn block_json(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text { text, .. } => Some(json!({"type": "text", "text": text})),
        ContentBlock::Json { value, .. } => {
            Some(json!({"type": "text", "text": value.to_string()}))
        }
        ContentBlock::ToolCall { id, name, args, .. } => {
            Some(json!({"type": "tool_use", "id": id, "name": name, "input": args}))
        }
        ContentBlock::InvalidToolCall { id, name, .. } => Some(json!({
            "type": "tool_use", "id": id, "name": name.clone().unwrap_or_default(), "input": {},
        })),
        _ => None,
    }
}

/// A complete (non-streamed) answer.
pub(crate) fn encode_response(id: &str, route: &str, response: &ModelResponse) -> Value {
    let content: Vec<Value> = response
        .message
        .content
        .iter()
        .filter_map(block_json)
        .collect();
    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": route,
        "content": content,
        "stop_reason": stop_reason(response.finish_reason),
        "stop_sequence": null,
        "usage": usage_json(&response.usage),
    })
}

/// An Anthropic stream event: its name and data.
pub(crate) type Event = (&'static str, Value);

/// Turns one answer's deltas into Anthropic's stream events, and at the end
/// sends whatever the deltas did not carry. Upstream blocks arrive one
/// after another, as every protocol AgentBuild speaks sends them.
#[derive(Debug)]
pub(crate) struct EventEncoder {
    id: String,
    route: String,
    started: bool,
    /// Our block index → Anthropic's block index.
    ordinals: BTreeMap<u32, usize>,
    /// Our block index of the open block.
    open: Option<u32>,
    streamed: BTreeSet<u32>,
}

impl EventEncoder {
    pub(crate) fn new(id: String, route: String) -> Self {
        Self {
            id,
            route,
            started: false,
            ordinals: BTreeMap::new(),
            open: None,
            streamed: BTreeSet::new(),
        }
    }

    /// `message_start`, once.
    fn start(&mut self, out: &mut Vec<Event>) {
        if !self.started {
            self.started = true;
            out.push((
                "message_start",
                json!({"type": "message_start", "message": {
                    "id": self.id, "type": "message", "role": "assistant", "model": self.route,
                    "content": [], "stop_reason": null, "stop_sequence": null,
                    "usage": {"input_tokens": 0, "output_tokens": 0},
                }}),
            ));
        }
    }

    fn close(&mut self, out: &mut Vec<Event>) {
        if let Some(index) = self.open.take() {
            let ordinal = self.ordinals[&index];
            out.push((
                "content_block_stop",
                json!({"type": "content_block_stop", "index": ordinal}),
            ));
        }
    }

    /// Opens block `index` (closing the one before) and returns its
    /// Anthropic index.
    fn open(&mut self, index: u32, block: Value, out: &mut Vec<Event>) -> usize {
        if self.open == Some(index) {
            return self.ordinals[&index];
        }
        self.close(out);
        let ordinal = self.ordinals.len();
        self.ordinals.insert(index, ordinal);
        self.open = Some(index);
        self.streamed.insert(index);
        out.push((
            "content_block_start",
            json!({"type": "content_block_start", "index": ordinal, "content_block": block}),
        ));
        ordinal
    }

    fn block_delta(ordinal: usize, delta: Value) -> Event {
        (
            "content_block_delta",
            json!({"type": "content_block_delta", "index": ordinal, "delta": delta}),
        )
    }

    /// Events for one delta.
    pub(crate) fn delta(&mut self, delta: &MessageDelta) -> Vec<Event> {
        let mut out = Vec::new();
        self.start(&mut out);
        match &delta.delta {
            BlockDelta::Text { text } => {
                let ordinal = self.open(delta.index, json!({"type": "text", "text": ""}), &mut out);
                out.push(Self::block_delta(
                    ordinal,
                    json!({"type": "text_delta", "text": text}),
                ));
            }
            BlockDelta::ToolCall {
                id,
                name,
                args_fragment,
            } => {
                let block = json!({
                    "type": "tool_use",
                    "id": id.clone().unwrap_or_default(),
                    "name": name.clone().unwrap_or_default(),
                    "input": {},
                });
                let ordinal = self.open(delta.index, block, &mut out);
                if !args_fragment.is_empty() {
                    out.push(Self::block_delta(
                        ordinal,
                        json!({"type": "input_json_delta", "partial_json": args_fragment}),
                    ));
                }
            }
            _ => {}
        }
        out
    }

    /// The last events: unstreamed blocks, the stop reason and usage, and
    /// `message_stop`.
    pub(crate) fn finish(&mut self, response: &ModelResponse) -> Vec<Event> {
        let mut out = Vec::new();
        self.start(&mut out);
        self.close(&mut out);
        for (index, block) in response.message.content.iter().enumerate() {
            let index = index as u32;
            if self.streamed.contains(&index) {
                continue;
            }
            let Some(full) = block_json(block) else {
                continue;
            };
            // Started empty, then filled by one delta, as Anthropic does.
            let (start, delta) = match full["type"].as_str() {
                Some("tool_use") => (
                    json!({"type": "tool_use", "id": full["id"], "name": full["name"], "input": {}}),
                    json!({"type": "input_json_delta", "partial_json": full["input"].to_string()}),
                ),
                _ => (
                    json!({"type": "text", "text": ""}),
                    json!({"type": "text_delta", "text": full["text"]}),
                ),
            };
            let ordinal = self.open(index, start, &mut out);
            out.push(Self::block_delta(ordinal, delta));
            self.close(&mut out);
        }
        out.push((
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason(response.finish_reason), "stop_sequence": null},
                "usage": usage_json(&response.usage),
            }),
        ));
        out.push(("message_stop", json!({"type": "message_stop"})));
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
            "max_tokens": 50,
            "system": [{"type": "text", "text": "Be brief."}],
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "What is this?"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="}},
                ]},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "hmm", "signature": "x"},
                    {"type": "tool_use", "id": "toolu_1", "name": "lookup", "input": {"q": "x"}},
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "found", "is_error": false},
                    {"type": "text", "text": "Thanks"},
                ]},
            ],
            "tools": [{"name": "lookup", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "tool", "name": "lookup"},
            "temperature": 0.2, "stop_sequences": ["END"], "stream": true, "metadata": {"user_id": "u"},
        });
        let decoded = decode_request(&body).unwrap();
        assert_eq!(decoded.route, "support");
        assert!(decoded.stream);
        let r = decoded.request;
        let roles: Vec<Role> = r.messages.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            [
                Role::System,
                Role::User,
                Role::Assistant,
                Role::Tool,
                Role::User
            ]
        );
        assert_eq!(r.messages[0].text(), "Be brief.");
        assert!(matches!(
            &r.messages[1].content[1],
            ContentBlock::Image { mime_type: Some(m), source: MediaSource::Base64 { .. }, .. } if m == "image/png"
        ));
        assert_eq!(r.messages[2].content.len(), 1, "thinking is dropped");
        assert_eq!(r.messages[3].tool.as_deref(), Some("lookup"));
        assert_eq!(r.messages[3].status, Some(ToolStatus::Success));
        assert_eq!(r.tool_choice, ToolChoice::Tool("lookup".into()));
        assert_eq!(r.settings.max_tokens, Some(50));
        assert_eq!(r.settings.stop, ["END"]);
    }

    #[test]
    fn bad_requests_say_what_is_wrong() {
        let user = json!([{"role": "user", "content": "x"}]);
        for (body, needle) in [
            (json!([]), "JSON object"),
            (json!({"max_tokens": 5, "messages": user}), "`model`"),
            (json!({"model": "m", "messages": user}), "`max_tokens`"),
            (
                json!({"model": "m", "max_tokens": 5, "messages": []}),
                "`messages`",
            ),
            (
                json!({"model": "m", "max_tokens": 5, "messages": [{"role": "system", "content": "x"}]}),
                "messages[0]: unknown role",
            ),
            (
                json!({"model": "m", "max_tokens": 5, "messages": [{"role": "user", "content": [{"type": "tool_result"}]}]}),
                "tool_use_id",
            ),
            (
                json!({"model": "m", "max_tokens": 5, "messages": user, "tool_choice": {"type": "tool"}}),
                "`name`",
            ),
        ] {
            let err = decode_request(&body).unwrap_err().to_string();
            assert!(err.contains(needle), "{err} (wanted {needle})");
        }
    }

    #[test]
    fn streamed_blocks_open_and_close_in_order() {
        let mut encoder = EventEncoder::new("msg_1".into(), "support".into());
        let text = |t: &str| MessageDelta {
            message_id: "m".into(),
            index: 0,
            delta: BlockDelta::Text { text: t.into() },
        };
        let mut events = encoder.delta(&text("Hel"));
        events.extend(encoder.delta(&text("lo")));
        events.extend(encoder.delta(&MessageDelta {
            message_id: "m".into(),
            index: 1,
            delta: BlockDelta::ToolCall {
                id: Some("toolu_1".into()),
                name: Some("lookup".into()),
                args_fragment: "{\"q\":".into(),
            },
        }));
        let mut message = Message::assistant("Hello");
        message
            .content
            .push(ContentBlock::tool_call("toolu_1", "lookup", Map::new()));
        message.content.push(ContentBlock::text("unstreamed"));
        let response = ModelResponse::new(message, Usage::new(10, 4), FinishReason::ToolCalls);
        events.extend(encoder.finish(&response));
        let names: Vec<&str> = events.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            [
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        let indexes: Vec<u64> = events
            .iter()
            .filter_map(|(_, v)| v.get("index").and_then(Value::as_u64))
            .collect();
        assert_eq!(indexes, [0, 0, 0, 0, 1, 1, 1, 2, 2, 2]);
        assert_eq!(events[11].1["delta"]["stop_reason"], "tool_use");
        assert_eq!(events[11].1["usage"]["input_tokens"], 10);
    }
}
