//! Streaming: what protocol decoders produce, and the assembler that turns
//! it into `message_delta` events and the final message (design 0002 §8,
//! 0006 §5, research litellm-providers §10.7).

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{Dispatch, ModelError, ModelErrorKind};
use crate::message::{
    ContentBlock, Extras, FinishReason, Message, Role, Usage, new_call_id, new_message_id,
};
use crate::request::ModelResponse;

/// One step of a decoded stream. Protocol decoders produce these; the
/// [`Assembler`] consumes them.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum StreamItem {
    /// The provider's message id, if it has one.
    Start {
        /// The provider's id (kept in metadata, not used as our id).
        provider_message_id: Option<String>,
    },
    /// More content for block `index`.
    Delta {
        /// Block position in the message.
        index: u32,
        /// The new content.
        delta: BlockDelta,
    },
    /// Block `index` is complete (tool-call arguments are parsed now).
    BlockEnd {
        /// Block position in the message.
        index: u32,
    },
    /// Token counts so far. Fields that are non-zero replace earlier values
    /// (providers report cumulative counts, sometimes split across events).
    Usage(Usage),
    /// The model stopped.
    Finish {
        /// Normalized reason.
        reason: FinishReason,
        /// The provider's own value.
        raw: Option<String>,
    },
    /// Something was dropped or repaired; surfaced as a `warning` event.
    Warning(Warning),
    /// Provider-specific fields for block `index` (e.g. citations), merged
    /// into the block's `extras` when the message is built. Fields of the
    /// same provider key are merged; later values win.
    Extras {
        /// Block position in the message.
        index: u32,
        /// The fields, keyed by provider.
        extras: Extras,
    },
}

/// New content for one block. JSON form as in 0002 §8.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BlockDelta {
    /// More text.
    Text {
        /// The new text.
        text: String,
    },
    /// More reasoning, or its signature.
    Reasoning {
        /// The new reasoning text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// Signature data, appended.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// A tool call. The first delta usually carries `id` and `name`.
    ToolCall {
        /// Call id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Tool name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// A piece of the JSON arguments.
        #[serde(default)]
        args_fragment: String,
    },
    /// A block that arrives whole (e.g. redacted reasoning, a provider block).
    Block {
        /// The block.
        block: ContentBlock,
    },
}

/// A non-fatal problem: data was dropped or repaired (0006 principle 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Warning {
    /// Stable snake_case code, e.g. `duplicate_call_id`.
    pub code: String,
    /// Human-readable detail.
    pub message: String,
}

impl Warning {
    /// A warning.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// A `message_delta` event (0002 §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageDelta {
    /// Our id of the message being built.
    pub message_id: String,
    /// Block position.
    pub index: u32,
    /// The new content.
    pub delta: BlockDelta,
}

/// Builds a message from [`StreamItem`]s. Text is concatenated; tool-call
/// arguments are concatenated and parsed **once**, at block end.
#[derive(Debug)]
pub struct Assembler {
    message_id: String,
    provider_message_id: Option<String>,
    blocks: BTreeMap<u32, Partial>,
    usage: Usage,
    finish: Option<(FinishReason, Option<String>)>,
    warnings: Vec<Warning>,
    emitted: bool,
    extras: BTreeMap<u32, Extras>,
}

#[derive(Debug)]
enum Partial {
    Text(String),
    Reasoning {
        text: Option<String>,
        signature: Option<String>,
    },
    ToolCall {
        id: Option<String>,
        name: Option<String>,
        args: String,
        ended: bool,
    },
    Done(ContentBlock),
}

impl Default for Assembler {
    fn default() -> Self {
        Self::new()
    }
}

impl Assembler {
    /// An assembler for a new message with a fresh id.
    pub fn new() -> Self {
        Self::with_message_id(new_message_id())
    }

    /// An assembler whose message gets `message_id` (a retry after
    /// `message_aborted` must use a new one).
    pub fn with_message_id(message_id: impl Into<String>) -> Self {
        Self {
            message_id: message_id.into(),
            provider_message_id: None,
            blocks: BTreeMap::new(),
            usage: Usage::default(),
            finish: None,
            warnings: Vec::new(),
            emitted: false,
            extras: BTreeMap::new(),
        }
    }

    /// The id of the message being built.
    pub fn message_id(&self) -> &str {
        &self.message_id
    }

    /// Whether any delta was emitted (a failure now needs `message_aborted`).
    pub fn emitted_deltas(&self) -> bool {
        self.emitted
    }

    /// Applies one item. Returns the `message_delta` to emit, if any.
    pub fn push(&mut self, item: StreamItem) -> Option<MessageDelta> {
        match item {
            StreamItem::Start {
                provider_message_id,
            } => {
                self.provider_message_id = provider_message_id;
                None
            }
            StreamItem::Delta { index, delta } => {
                self.apply(index, &delta);
                self.emitted = true;
                Some(MessageDelta {
                    message_id: self.message_id.clone(),
                    index,
                    delta,
                })
            }
            StreamItem::BlockEnd { index } => {
                if let Some(Partial::ToolCall { ended, .. }) = self.blocks.get_mut(&index) {
                    *ended = true;
                }
                None
            }
            StreamItem::Usage(usage) => {
                merge_usage(&mut self.usage, &usage);
                None
            }
            StreamItem::Finish { reason, raw } => {
                self.finish = Some((reason, raw));
                None
            }
            StreamItem::Warning(warning) => {
                self.warnings.push(warning);
                None
            }
            StreamItem::Extras { index, extras } => {
                merge_extras(self.extras.entry(index).or_default(), extras);
                None
            }
        }
    }

    fn apply(&mut self, index: u32, delta: &BlockDelta) {
        let entry = self.blocks.entry(index);
        match delta {
            BlockDelta::Text { text } => match entry.or_insert(Partial::Text(String::new())) {
                Partial::Text(buffer) => buffer.push_str(text),
                _ => self.mismatch(index, "text"),
            },
            BlockDelta::Reasoning { text, signature } => match entry.or_insert(Partial::Reasoning {
                text: None,
                signature: None,
            }) {
                Partial::Reasoning {
                    text: t,
                    signature: s,
                } => {
                    if let Some(more) = text {
                        t.get_or_insert_with(String::new).push_str(more);
                    }
                    if let Some(more) = signature {
                        s.get_or_insert_with(String::new).push_str(more);
                    }
                }
                _ => self.mismatch(index, "reasoning"),
            },
            BlockDelta::ToolCall {
                id,
                name,
                args_fragment,
            } => match entry.or_insert(Partial::ToolCall {
                id: None,
                name: None,
                args: String::new(),
                ended: false,
            }) {
                Partial::ToolCall {
                    id: i,
                    name: n,
                    args,
                    ..
                } => {
                    if i.is_none() {
                        i.clone_from(id);
                    }
                    if n.is_none() {
                        n.clone_from(name);
                    }
                    args.push_str(args_fragment);
                }
                _ => self.mismatch(index, "tool_call"),
            },
            BlockDelta::Block { block } => {
                if self
                    .blocks
                    .insert(index, Partial::Done(block.clone()))
                    .is_some()
                {
                    self.warnings.push(Warning::new(
                        "block_replaced",
                        format!("block {index} was sent again whole; the later one is kept"),
                    ));
                }
            }
        }
    }

    fn mismatch(&mut self, index: u32, kind: &str) {
        self.warnings.push(Warning::new(
            "delta_type_mismatch",
            format!("a {kind} delta for block {index}, which has another type; dropped"),
        ));
    }

    /// The stream ended. Returns the authoritative message (with the
    /// warnings collected), or an error if the stream ended without a finish
    /// reason (0006 §5).
    pub fn finish(mut self) -> Result<ModelResponse, ModelError> {
        let Some((reason, raw)) = self.finish.take() else {
            return Err(ModelError::new(
                ModelErrorKind::Server,
                "the stream ended without a finish reason",
            )
            .with_dispatch(Dispatch::Streaming)
            .with_emitted(self.emitted));
        };
        let mut seen_ids = HashSet::new();
        let mut content = Vec::with_capacity(self.blocks.len());
        for (index, partial) in std::mem::take(&mut self.blocks) {
            let mut block = match partial {
                Partial::Text(text) => ContentBlock::text(text),
                Partial::Reasoning { text, signature } => ContentBlock::Reasoning {
                    text,
                    signature,
                    redacted: None,
                    extras: Extras::new(),
                },
                Partial::Done(block) => block,
                Partial::ToolCall {
                    id,
                    name,
                    args,
                    ended,
                } => {
                    let id = self.call_id(id, &mut seen_ids);
                    let truncated = !ended && reason == FinishReason::Length;
                    tool_call_block(id, name, args, truncated)
                }
            };
            if let Some(extras) = self.extras.remove(&index) {
                match block.extras_mut() {
                    Some(target) => merge_extras(target, extras),
                    None => self.warnings.push(Warning::new(
                        "extras_dropped",
                        format!("block {index} cannot carry extras; dropped"),
                    )),
                }
            }
            content.push(block);
        }
        for index in std::mem::take(&mut self.extras).into_keys() {
            self.warnings.push(Warning::new(
                "extras_dropped",
                format!("extras for block {index}, which never arrived; dropped"),
            ));
        }

        let mut message = Message::with_content(Role::Assistant, content).with_id(self.message_id);
        message.metadata.raw_finish_reason = raw;
        if let Some(id) = self.provider_message_id {
            message
                .metadata
                .other
                .insert("provider_message_id".into(), Value::String(id));
        }
        let mut response = ModelResponse::new(message, self.usage, reason);
        response.warnings = self.warnings;
        Ok(response)
    }

    /// A missing id gets `call_<uuid>`; a repeated id in the same message gets
    /// a fresh one (0002 §7.1).
    fn call_id(&mut self, id: Option<String>, seen: &mut HashSet<String>) -> String {
        let id = match id.filter(|id| !id.is_empty()) {
            Some(id) => id,
            None => {
                let fresh = new_call_id();
                self.warnings.push(Warning::new(
                    "missing_call_id",
                    format!("a tool call had no id; assigned {fresh}"),
                ));
                fresh
            }
        };
        if seen.insert(id.clone()) {
            return id;
        }
        let fresh = new_call_id();
        self.warnings.push(Warning::new(
            "duplicate_call_id",
            format!("tool call id {id} was repeated; the later call is now {fresh}"),
        ));
        seen.insert(fresh.clone());
        fresh
    }
}

fn tool_call_block(
    id: String,
    name: Option<String>,
    args: String,
    truncated: bool,
) -> ContentBlock {
    let invalid = |name, error: String| {
        ContentBlock::invalid_tool_call(id.clone(), name, args.clone(), error)
    };
    if truncated {
        return invalid(name, "truncated".into());
    }
    let Some(name) = name.filter(|n| !n.is_empty()) else {
        return invalid(None, "the tool call has no name".into());
    };
    if args.trim().is_empty() {
        return ContentBlock::tool_call(id, name, Map::new());
    }
    match serde_json::from_str::<Value>(&args) {
        Ok(Value::Object(map)) => ContentBlock::tool_call(id, name, map),
        Ok(other) => invalid(
            Some(name),
            format!("arguments must be a JSON object, got {}", json_type(&other)),
        ),
        Err(err) => invalid(Some(name), format!("arguments are not valid JSON: {err}")),
    }
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Merges `update` into `target`: objects under the same key are merged
/// field by field, anything else is replaced.
fn merge_extras(target: &mut Extras, update: Extras) {
    for (key, value) in update {
        match (target.get_mut(&key), value) {
            (Some(Value::Object(fields)), Value::Object(more)) => fields.extend(more),
            (_, value) => {
                target.insert(key, value);
            }
        }
    }
}

fn merge_usage(total: &mut Usage, update: &Usage) {
    let pairs = [
        (&mut total.input_tokens, update.input_tokens),
        (&mut total.output_tokens, update.output_tokens),
        (&mut total.cache_read_tokens, update.cache_read_tokens),
        (&mut total.cache_write_tokens, update.cache_write_tokens),
        (&mut total.reasoning_tokens, update.reasoning_tokens),
    ];
    for (field, value) in pairs {
        if value > 0 {
            *field = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn text(index: u32, text: &str) -> StreamItem {
        StreamItem::Delta {
            index,
            delta: BlockDelta::Text { text: text.into() },
        }
    }

    fn call(index: u32, id: Option<&str>, name: Option<&str>, fragment: &str) -> StreamItem {
        StreamItem::Delta {
            index,
            delta: BlockDelta::ToolCall {
                id: id.map(Into::into),
                name: name.map(Into::into),
                args_fragment: fragment.into(),
            },
        }
    }

    fn finish(reason: FinishReason) -> StreamItem {
        StreamItem::Finish { reason, raw: None }
    }

    fn assemble(items: Vec<StreamItem>) -> ModelResponse {
        let mut assembler = Assembler::with_message_id("msg_test");
        for item in items {
            assembler.push(item);
        }
        assembler.finish().unwrap()
    }

    #[test]
    fn text_and_tool_calls_in_index_order() {
        let assembled = assemble(vec![
            StreamItem::Start {
                provider_message_id: Some("prov_1".into()),
            },
            text(0, "Let me "),
            call(1, Some("c1"), Some("search"), "{\"q\": "),
            text(0, "check."),
            call(1, None, None, "\"rust\"}"),
            StreamItem::BlockEnd { index: 1 },
            StreamItem::Usage(Usage::new(100, 0)),
            StreamItem::Usage(Usage::new(0, 25)),
            StreamItem::Finish {
                reason: FinishReason::ToolCalls,
                raw: Some("tool_use".into()),
            },
        ]);
        let response = assembled;
        assert!(response.warnings.is_empty());
        assert_eq!(response.message.id, "msg_test");
        assert_eq!(response.message.text(), "Let me check.");
        let call = response.message.tool_calls().next().unwrap();
        assert_eq!((call.id, call.name), ("c1", "search"));
        assert_eq!(call.args["q"], "rust");
        assert_eq!(response.usage, Usage::new(100, 25));
        assert_eq!(response.finish_reason, FinishReason::ToolCalls);
        let meta = &response.message.metadata;
        assert_eq!(meta.raw_finish_reason.as_deref(), Some("tool_use"));
        assert_eq!(meta.other["provider_message_id"], "prov_1");
        assert!(response.message.validate().is_ok());
    }

    #[test]
    fn deltas_are_passed_through_for_events() {
        let mut assembler = Assembler::with_message_id("msg_x");
        assert!(
            assembler
                .push(StreamItem::Start {
                    provider_message_id: None
                })
                .is_none()
        );
        assert!(!assembler.emitted_deltas());
        let delta = assembler.push(text(0, "Hel")).unwrap();
        assert!(assembler.emitted_deltas());
        assert_eq!(
            serde_json::to_value(&delta).unwrap(),
            json!({"message_id": "msg_x", "index": 0, "delta": {"type": "text", "text": "Hel"}})
        );
    }

    #[test]
    fn bad_arguments_become_invalid_tool_calls() {
        let assembled = assemble(vec![
            call(0, Some("a"), Some("t"), "{\"x\": 1"),
            call(1, Some("b"), Some("t"), "[1, 2]"),
            call(2, Some("c"), None, "{}"),
            call(3, Some("d"), Some("t"), ""),
            finish(FinishReason::ToolCalls),
        ]);
        let blocks = &assembled.message.content;
        let errors: Vec<String> = blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::InvalidToolCall { error, .. } => Some(error.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(errors.len(), 3);
        assert!(errors[0].starts_with("arguments are not valid JSON"));
        assert_eq!(errors[1], "arguments must be a JSON object, got an array");
        assert_eq!(errors[2], "the tool call has no name");
        // Empty arguments mean no arguments.
        let ok = assembled.message.tool_calls().next().unwrap();
        assert_eq!((ok.id, ok.args.len()), ("d", 0));
    }

    #[test]
    fn length_cut_marks_open_tool_calls_truncated() {
        let assembled = assemble(vec![
            call(0, Some("done"), Some("t"), "{}"),
            StreamItem::BlockEnd { index: 0 },
            call(1, Some("open"), Some("t"), "{\"long\": \"abc"),
            finish(FinishReason::Length),
        ]);
        let blocks = &assembled.message.content;
        assert!(matches!(&blocks[0], ContentBlock::ToolCall { id, .. } if id == "done"));
        assert!(matches!(
            &blocks[1],
            ContentBlock::InvalidToolCall { error, raw_args, .. }
                if error == "truncated" && raw_args == "{\"long\": \"abc"
        ));
    }

    #[test]
    fn missing_and_duplicate_ids_are_repaired_with_warnings() {
        let assembled = assemble(vec![
            call(0, None, Some("t"), "{}"),
            call(1, Some("same"), Some("t"), "{}"),
            call(2, Some("same"), Some("t"), "{}"),
            finish(FinishReason::ToolCalls),
        ]);
        let ids: Vec<&str> = assembled.message.tool_calls().map(|c| c.id).collect();
        assert!(ids[0].starts_with("call_"));
        assert_eq!(ids[1], "same");
        assert!(ids[2].starts_with("call_") && ids[2] != "same");
        let codes: Vec<&str> = assembled.warnings.iter().map(|w| w.code.as_str()).collect();
        assert_eq!(codes, ["missing_call_id", "duplicate_call_id"]);
    }

    #[test]
    fn reasoning_with_signature_and_whole_blocks() {
        let assembled = assemble(vec![
            StreamItem::Delta {
                index: 0,
                delta: BlockDelta::Reasoning {
                    text: Some("think".into()),
                    signature: None,
                },
            },
            StreamItem::Delta {
                index: 0,
                delta: BlockDelta::Reasoning {
                    text: None,
                    signature: Some("sig==".into()),
                },
            },
            StreamItem::Delta {
                index: 1,
                delta: BlockDelta::Block {
                    block: ContentBlock::provider("anthropic", json!({"type": "server_tool_use"})),
                },
            },
            text(2, "answer"),
            finish(FinishReason::Stop),
        ]);
        let blocks = &assembled.message.content;
        assert!(matches!(
            &blocks[0],
            ContentBlock::Reasoning { text: Some(t), signature: Some(s), .. } if t == "think" && s == "sig=="
        ));
        assert_eq!(blocks[1].type_name(), "provider");
        assert_eq!(blocks[2].type_name(), "text");
    }

    #[test]
    fn mismatched_delta_is_dropped_with_a_warning() {
        let assembled = assemble(vec![
            text(0, "a"),
            call(0, Some("c"), Some("t"), "{}"),
            finish(FinishReason::Stop),
        ]);
        assert_eq!(assembled.message.text(), "a");
        assert_eq!(assembled.warnings[0].code, "delta_type_mismatch");
    }

    #[test]
    fn stream_without_finish_is_a_streaming_server_error() {
        let mut assembler = Assembler::new();
        assembler.push(text(0, "partial"));
        let err = assembler.finish().unwrap_err();
        assert_eq!(err.kind, ModelErrorKind::Server);
        assert_eq!(err.dispatch, Dispatch::Streaming);
        assert!(err.emitted_deltas);

        let empty = Assembler::new().finish().unwrap_err();
        assert!(!empty.emitted_deltas);
    }

    #[test]
    fn extras_are_merged_into_their_block() {
        let note = |value: Value| {
            let mut extras = Extras::new();
            extras.insert("p".into(), value);
            extras
        };
        let assembled = assemble(vec![
            text(0, "cited"),
            StreamItem::Extras {
                index: 0,
                extras: note(json!({"citations": [1]})),
            },
            StreamItem::Extras {
                index: 0,
                extras: note(json!({"cache": true})),
            },
            StreamItem::Extras {
                index: 5,
                extras: note(json!({})),
            },
            finish(FinishReason::Stop),
        ]);
        match &assembled.message.content[0] {
            ContentBlock::Text { extras, .. } => {
                assert_eq!(extras["p"], json!({"citations": [1], "cache": true}));
            }
            other => panic!("{other:?}"),
        }
        let codes: Vec<&str> = assembled.warnings.iter().map(|w| w.code.as_str()).collect();
        assert_eq!(codes, ["extras_dropped"]);
    }

    #[test]
    fn decoder_warnings_are_collected() {
        let assembled = assemble(vec![
            StreamItem::Warning(Warning::new("dropped_block", "x")),
            finish(FinishReason::Stop),
        ]);
        assert_eq!(assembled.warnings.len(), 1);
        assert!(assembled.message.content.is_empty());
    }
}
