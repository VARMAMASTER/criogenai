//! Turns run events into finished spans (design 0009 §3). Pure: no I/O.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use criogen_core::{Event, EventKind};
use serde_json::Value;

/// A function applied to every captured payload (messages, arguments,
/// results) before it leaves the process.
pub type MaskFn = Arc<dyn Fn(Value) -> Value + Send + Sync>;

/// What content is exported.
#[derive(Clone)]
pub(crate) struct ContentPolicy {
    pub capture: bool,
    pub mask: Option<MaskFn>,
    pub max_chars: usize,
}

impl ContentPolicy {
    /// The payload as a JSON string, masked and bounded; `None` when content
    /// capture is off.
    fn render(&self, value: &Value) -> Option<String> {
        if !self.capture {
            return None;
        }
        let value = match &self.mask {
            Some(mask) => match catch_unwind(AssertUnwindSafe(|| mask(value.clone()))) {
                Ok(masked) => masked,
                Err(_) => Value::String("[mask failed]".into()),
            },
            None => value.clone(),
        };
        let text = match value {
            Value::String(s) => s,
            other => other.to_string(),
        };
        Some(truncate(text, self.max_chars))
    }
}

impl ContentPolicy {
    /// An error as it is exported: `code: message`, masked and bounded,
    /// when content capture is on; **the code alone** when it is off. Error
    /// text echoes what failed (a rejected value, an exception's message,
    /// a path), which is content.
    fn detail(&self, code: &str, message: &str) -> String {
        match self.render(&Value::String(message.to_string())) {
            Some(message) => format!("{code}: {message}"),
            None => code.to_string(),
        }
    }

    /// A run-metadata value as exported: `None` for identities and free
    /// fields (`user_id`, `session_id`, custom keys) when content capture is
    /// off, masked and bounded when it is on.
    fn metadata(&self, value: String) -> Option<String> {
        self.render(&Value::String(value))
    }
}

fn truncate(mut text: String, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text;
    }
    let cut = text
        .char_indices()
        .nth(max_chars)
        .map_or(text.len(), |(i, _)| i);
    let dropped = text[cut..].chars().count();
    text.truncate(cut);
    text.push_str(&format!("…[truncated: {dropped} more characters]"));
    text
}

/// An attribute value (the OTLP `AnyValue` kinds we use).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Attr {
    Str(String),
    Int(i64),
    Bool(bool),
    StrList(Vec<String>),
}

/// OTLP status.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Status {
    Unset,
    Ok,
    Error(String),
}

/// A timestamped event inside a span.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SpanEvent {
    pub name: String,
    pub time_ms: u64,
    pub attributes: Vec<(String, Attr)>,
}

/// A finished span.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Span {
    pub trace_id: String,
    pub span_id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub attributes: Vec<(String, Attr)>,
    pub events: Vec<SpanEvent>,
    pub status: Status,
}

impl Span {
    fn set(&mut self, key: &str, value: Attr) {
        match self.attributes.iter_mut().find(|(k, _)| k == key) {
            Some((_, existing)) => *existing = value,
            None => self.attributes.push((key.to_string(), value)),
        }
    }

    fn set_str(&mut self, key: &str, value: impl Into<String>) {
        self.set(key, Attr::Str(value.into()));
    }

    #[cfg(test)]
    pub(crate) fn attr(&self, key: &str) -> Option<&Attr> {
        self.attributes
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }
}

/// Spans of one run segment that are still open.
struct RunTrace {
    trace_id: String,
    root: Span,
    /// Attributes copied onto every span (Langfuse reads trace-level fields
    /// from any span).
    trace_attributes: Vec<(String, Attr)>,
    nodes: HashMap<(usize, String), Span>,
    /// Open generation per node.
    generations: HashMap<(usize, String), Span>,
    tools: HashMap<String, Span>,
    saw_model: bool,
    /// First model input and last model output: the trace's input/output.
    first_input: Option<String>,
    last_output: Option<String>,
}

/// Builds spans from the events of any number of runs.
pub(crate) struct SpanBuilder {
    policy: ContentPolicy,
    runs: HashMap<String, RunTrace>,
}

impl SpanBuilder {
    pub(crate) fn new(policy: ContentPolicy) -> Self {
        Self {
            policy,
            runs: HashMap::new(),
        }
    }

    /// Number of runs with open spans.
    #[cfg(test)]
    pub(crate) fn open_runs(&self) -> usize {
        self.runs.len()
    }

    /// Consumes one event; returns the spans it finished.
    pub(crate) fn on_event(&mut self, event: &Event) -> Vec<Span> {
        let mut done = Vec::new();
        if let EventKind::RunStarted { resumed, metadata } = &event.kind {
            self.start_run(event, *resumed, metadata.as_ref());
            return done;
        }
        let policy = self.policy.clone();
        let Some(run) = self.runs.get_mut(&event.run_id) else {
            return done; // events of a run that started before we listened
        };
        let node_key = || {
            (
                event.step.unwrap_or(0),
                event.node.clone().unwrap_or_default(),
            )
        };
        match &event.kind {
            EventKind::NodeStarted { attempt } => {
                let key = node_key();
                if *attempt == 1 || !run.nodes.contains_key(&key) {
                    let span = run.child(
                        &run.root.span_id.clone(),
                        &format!("node:{}:{}", key.0, key.1),
                        &key.1,
                        event.ts_ms,
                    );
                    run.nodes.insert(key, span);
                }
                if *attempt > 1
                    && let Some(span) = run.nodes.get_mut(&node_key())
                {
                    span.set("criogen.attempts", Attr::Int(i64::from(*attempt)));
                }
            }
            EventKind::NodeFinished {
                update_keys,
                goto,
                suspended,
                reused,
                ..
            } => {
                let key = node_key();
                let mut span = match run.nodes.remove(&key) {
                    Some(span) => span,
                    None => run.child(
                        &run.root.span_id.clone(),
                        &format!("node:{}:{}", key.0, key.1),
                        &key.1,
                        event.ts_ms,
                    ),
                };
                span.set("criogen.update_keys", Attr::StrList(update_keys.clone()));
                if !goto.is_empty() {
                    span.set("criogen.goto", Attr::StrList(goto.clone()));
                }
                if *suspended {
                    span.set("criogen.suspended", Attr::Bool(true));
                }
                if *reused {
                    span.set("criogen.reused", Attr::Bool(true));
                }
                span.status = Status::Ok;
                span.end_ms = event.ts_ms;
                done.push(span);
            }
            EventKind::NodeFailed { code, message, .. } => {
                if let Some(mut span) = run.nodes.remove(&node_key()) {
                    fail(&mut span, policy.detail(code, message), event.ts_ms);
                    span.set_str("criogen.error_code", code.clone());
                    done.push(span);
                }
            }
            EventKind::NodeCancelled { reason } => {
                if let Some(mut span) = run.nodes.remove(&node_key()) {
                    fail(&mut span, policy.detail("cancelled", reason), event.ts_ms);
                    done.push(span);
                }
            }
            EventKind::AttemptFailed {
                attempt,
                code,
                message,
                retry_in_ms,
            } => {
                let parent = run.node_span_id(event);
                let mut span = run.child(
                    &parent,
                    &format!("event:{}", event.seq),
                    "attempt_failed",
                    event.ts_ms,
                );
                span.set_str("langfuse.observation.type", "event");
                span.set_str("langfuse.observation.level", "WARNING");
                span.set_str(
                    "langfuse.observation.status_message",
                    format!(
                        "attempt {attempt} failed: {}; retry in {retry_in_ms} ms",
                        policy.detail(code, message)
                    ),
                );
                span.set("criogen.attempt", Attr::Int(i64::from(*attempt)));
                span.status = Status::Ok;
                done.push(span);
            }
            EventKind::Suspended { interrupts } => {
                run.root.set("criogen.suspended", Attr::Bool(true));
                let nodes = interrupts.iter().map(|i| i.node.clone()).collect();
                run.root.set("criogen.interrupts", Attr::StrList(nodes));
            }
            EventKind::Custom { name, payload } => {
                run.custom(&policy, event, name, payload, &mut done);
            }
            EventKind::RunFinished {
                status,
                error,
                steps,
            } => {
                let mut run = self.runs.remove(&event.run_id).expect("present above");
                let error = error.as_deref().map(|e| policy.detail("error", e));
                done.extend(run.finish(event.ts_ms, status, error.as_deref(), *steps));
            }
            _ => {}
        }
        done
    }

    /// Closes every open span (e.g. at shutdown) as incomplete.
    pub(crate) fn close_all(&mut self, now_ms: u64) -> Vec<Span> {
        let mut done = Vec::new();
        for (_, mut run) in self.runs.drain() {
            done.extend(run.finish(now_ms, "incomplete", Some("exporter_shutdown"), 0));
        }
        done
    }

    fn start_run(&mut self, event: &Event, resumed: bool, metadata: Option<&Value>) {
        let trace_id = trace_id(&event.run_id);
        let mut trace_attributes: Vec<(String, Attr)> = Vec::new();
        let mut name = "criogen.run".to_string();
        if let Some(Value::Object(meta)) = metadata {
            for (key, value) in meta {
                let text = || match value {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                match key.as_str() {
                    "name" => name = text(),
                    // Who the run is for: an identity, so exported only
                    // with content capture on (masked like content).
                    "user_id" => {
                        if let Some(id) = self.policy.metadata(text()) {
                            trace_attributes.push(("langfuse.user.id".into(), Attr::Str(id)));
                        }
                    }
                    "session_id" => {
                        if let Some(id) = self.policy.metadata(text()) {
                            trace_attributes.push(("langfuse.session.id".into(), Attr::Str(id)));
                        }
                    }
                    "release" => {
                        trace_attributes.push(("langfuse.release".into(), Attr::Str(text())))
                    }
                    "environment" => {
                        trace_attributes.push(("langfuse.environment".into(), Attr::Str(text())))
                    }
                    "tags" => {
                        let tags = value
                            .as_array()
                            .map(|t| {
                                t.iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default();
                        trace_attributes.push(("langfuse.trace.tags".into(), Attr::StrList(tags)));
                    }
                    other => {
                        if let Some(value) = self.policy.metadata(text()) {
                            trace_attributes.push((
                                format!("langfuse.trace.metadata.{other}"),
                                Attr::Str(value),
                            ));
                        }
                    }
                }
            }
        }
        trace_attributes.push(("langfuse.trace.name".into(), Attr::Str(name.clone())));
        trace_attributes.push(("criogen.run_id".into(), Attr::Str(event.run_id.clone())));
        let mut root = Span {
            trace_id: trace_id.clone(),
            span_id: span_id(&event.run_id, &format!("run:{}", event.seq)),
            parent_id: None,
            name,
            start_ms: event.ts_ms,
            end_ms: event.ts_ms,
            attributes: trace_attributes.clone(),
            events: Vec::new(),
            status: Status::Unset,
        };
        root.set("criogen.resumed", Attr::Bool(resumed));
        self.runs.insert(
            event.run_id.clone(),
            RunTrace {
                trace_id,
                root,
                trace_attributes,
                nodes: HashMap::new(),
                generations: HashMap::new(),
                tools: HashMap::new(),
                saw_model: false,
                first_input: None,
                last_output: None,
            },
        );
    }
}

fn fail(span: &mut Span, message: String, at: u64) {
    span.set_str("langfuse.observation.level", "ERROR");
    span.set_str("langfuse.observation.status_message", message.clone());
    span.status = Status::Error(message);
    span.end_ms = at;
}

impl RunTrace {
    fn child(&self, parent: &str, key: &str, name: &str, at: u64) -> Span {
        Span {
            trace_id: self.trace_id.clone(),
            span_id: span_id(&self.trace_id, &format!("{parent}/{key}")),
            parent_id: Some(parent.to_string()),
            name: name.to_string(),
            start_ms: at,
            end_ms: at,
            attributes: self.trace_attributes.clone(),
            events: Vec::new(),
            status: Status::Unset,
        }
    }

    fn node_span_id(&self, event: &Event) -> String {
        let key = (
            event.step.unwrap_or(0),
            event.node.clone().unwrap_or_default(),
        );
        self.nodes
            .get(&key)
            .map_or_else(|| self.root.span_id.clone(), |span| span.span_id.clone())
    }

    fn custom(
        &mut self,
        policy: &ContentPolicy,
        event: &Event,
        name: &str,
        payload: &Value,
        done: &mut Vec<Span>,
    ) {
        let key = (
            event.step.unwrap_or(0),
            event.node.clone().unwrap_or_default(),
        );
        let parent = self.node_span_id(event);
        let field = |k: &str| payload.get(k);
        let text = |k: &str| {
            field(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        match name {
            "model_request" => {
                self.saw_model = true;
                let model = text("model");
                let mut span = self.child(
                    &parent,
                    &format!("gen:{}", event.seq),
                    &format!("{} {model}", "chat"),
                    event.ts_ms,
                );
                span.set_str("langfuse.observation.type", "generation");
                span.set_str("langfuse.observation.model.name", model.clone());
                span.set_str("gen_ai.operation.name", "chat");
                span.set_str("gen_ai.system", text("provider"));
                span.set_str("gen_ai.request.model", model);
                if let Some(settings) = field("settings")
                    && settings.as_object().is_some_and(|s| !s.is_empty())
                {
                    span.set_str(
                        "langfuse.observation.model.parameters",
                        settings.to_string(),
                    );
                }
                let chat = field("messages").map(to_chat);
                if let Some(input) = chat.as_ref().and_then(|m| policy.render(m)) {
                    if self.first_input.is_none() {
                        self.first_input = Some(input.clone());
                    }
                    span.set_str("langfuse.observation.input", input);
                }
                self.generations.insert(key, span);
            }
            "message_delta" => {
                if let Some(span) = self.generations.get_mut(&key)
                    && !span
                        .attributes
                        .iter()
                        .any(|(k, _)| k == "langfuse.observation.completion_start_time")
                {
                    span.set_str(
                        "langfuse.observation.completion_start_time",
                        iso8601(event.ts_ms),
                    );
                }
            }
            "message_completed" => {
                if let Some(mut span) = self.generations.remove(&key) {
                    let usage = field("usage").cloned().unwrap_or(Value::Null);
                    let tokens = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
                    let (input, output) = (tokens("input_tokens"), tokens("output_tokens"));
                    span.set("gen_ai.usage.input_tokens", Attr::Int(input as i64));
                    span.set("gen_ai.usage.output_tokens", Attr::Int(output as i64));
                    // Our counts include cached input and reasoning output;
                    // Langfuse's `input`/`output` exclude them (they have
                    // their own keys), and `total` covers everything.
                    let (cache_read, cache_write, reasoning) = (
                        tokens("cache_read_tokens"),
                        tokens("cache_write_tokens"),
                        tokens("reasoning_tokens"),
                    );
                    let mut details = serde_json::json!({
                        "input": input.saturating_sub(cache_read + cache_write),
                        "output": output.saturating_sub(reasoning),
                        "total": input + output,
                    });
                    for (key, n) in [
                        ("input_cached_tokens", cache_read),
                        ("input_cache_creation", cache_write),
                        ("output_reasoning_tokens", reasoning),
                    ] {
                        if n > 0 {
                            details[key] = n.into();
                        }
                    }
                    span.set_str("langfuse.observation.usage_details", details.to_string());
                    let model = text("model");
                    if !model.is_empty() {
                        span.set_str("gen_ai.response.model", model);
                    }
                    if let Some(reason) = field("finish_reason").and_then(Value::as_str) {
                        span.set(
                            "gen_ai.response.finish_reasons",
                            Attr::StrList(vec![reason.to_string()]),
                        );
                    }
                    let chat = field("message").map(chat_message);
                    if let Some(output) = chat.as_ref().and_then(|m| policy.render(m)) {
                        self.last_output = Some(output.clone());
                        span.set_str("langfuse.observation.output", output);
                    }
                    span.status = Status::Ok;
                    span.end_ms = event.ts_ms;
                    done.push(span);
                }
            }
            "message_aborted" => {
                if let Some(mut span) = self.generations.remove(&key) {
                    fail(
                        &mut span,
                        policy.detail("stream_aborted", &text("reason")),
                        event.ts_ms,
                    );
                    done.push(span);
                }
            }
            "tool_call_started" => {
                let call_id = text("call_id");
                let tool = text("tool");
                let mut span = self.child(&parent, &format!("tool:{call_id}"), &tool, event.ts_ms);
                span.set_str("langfuse.observation.type", "tool");
                span.set_str("gen_ai.operation.name", "execute_tool");
                span.set_str("gen_ai.tool.name", tool);
                span.set_str("gen_ai.tool.call.id", call_id.clone());
                if let Some(args) = field("args").and_then(|a| policy.render(a)) {
                    span.set_str("langfuse.observation.input", args);
                }
                self.tools.insert(call_id, span);
            }
            "tool_call_finished" => {
                if let Some(mut span) = self.tools.remove(&text("call_id")) {
                    if let Some(output) = field("content").and_then(|c| policy.render(c)) {
                        span.set_str("langfuse.observation.output", output);
                    }
                    span.set_str("criogen.tool.status", text("status"));
                    let error = field("error").filter(|e| !e.is_null());
                    match (text("status").as_str(), error) {
                        ("error", Some(error)) => {
                            let message = error
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("tool error");
                            let code = error.get("code").and_then(Value::as_str).unwrap_or("error");
                            // The model may recover: a warning, not a failure.
                            span.set_str("langfuse.observation.level", "WARNING");
                            let detail = policy.detail(code, message);
                            span.set_str("langfuse.observation.status_message", detail.clone());
                            span.status = Status::Error(detail);
                        }
                        _ => span.status = Status::Ok,
                    }
                    span.end_ms = event.ts_ms;
                    done.push(span);
                }
            }
            other => {
                // Warnings, repairs and user events become `event`
                // observations: Langfuse does not store OTLP span events.
                let mut span =
                    self.child(&parent, &format!("event:{}", event.seq), other, event.ts_ms);
                span.set_str("langfuse.observation.type", "event");
                if other == "warning" {
                    span.set_str("langfuse.observation.level", "WARNING");
                    span.set_str(
                        "langfuse.observation.status_message",
                        policy.detail(&text("code"), &text("message")),
                    );
                } else if let Some(rendered) = policy.render(payload) {
                    span.set_str("langfuse.observation.input", rendered);
                }
                span.status = Status::Ok;
                done.push(span);
            }
        }
    }

    /// Closes the segment: open children become errors, then the root.
    fn finish(&mut self, at: u64, status: &str, error: Option<&str>, steps: usize) -> Vec<Span> {
        let mut done = Vec::new();
        let open = self
            .generations
            .drain()
            .map(|(_, s)| s)
            .chain(self.tools.drain().map(|(_, s)| s))
            .chain(self.nodes.drain().map(|(_, s)| s));
        for mut span in open {
            fail(&mut span, format!("not finished: run {status}"), at);
            done.push(span);
        }
        let mut root = self.root.clone();
        root.end_ms = at;
        root.set_str(
            "langfuse.observation.type",
            if self.saw_model { "agent" } else { "chain" },
        );
        root.set_str("criogen.status", status);
        if let Some(input) = self.first_input.take() {
            root.set_str("langfuse.observation.input", input);
        }
        if let Some(output) = self.last_output.take() {
            root.set_str("langfuse.observation.output", output);
        }
        root.set("criogen.steps", Attr::Int(steps as i64));
        match (status, error) {
            ("failed" | "incomplete", error) => {
                fail(&mut root, error.unwrap_or(status).to_string(), at);
            }
            _ => root.status = Status::Ok,
        }
        done.push(root);
        done
    }
}

/// Messages in the OpenAI chat shape (`{role, content, tool_calls}`), which
/// Langfuse and most tracing UIs render as a conversation.
fn to_chat(messages: &Value) -> Value {
    match messages {
        Value::Array(items) => Value::Array(items.iter().map(chat_message).collect()),
        other => other.clone(),
    }
}

/// One Criogen message (0002) as a chat message.
fn chat_message(message: &Value) -> Value {
    let Some(blocks) = message.get("content").and_then(Value::as_array) else {
        return message.clone();
    };
    let mut text = Vec::new();
    let mut tool_calls = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                text.push(block["text"].as_str().unwrap_or_default().to_string());
            }
            Some("json") => text.push(block["value"].to_string()),
            Some("tool_call") => tool_calls.push(serde_json::json!({
                "id": block["id"],
                "type": "function",
                "function": {"name": block["name"], "arguments": block["args"].to_string()},
            })),
            Some("invalid_tool_call") => tool_calls.push(serde_json::json!({
                "id": block["id"],
                "type": "function",
                "function": {"name": block["name"], "arguments": block["raw_args"]},
            })),
            Some("reasoning") => {}
            Some(other) => text.push(format!("[{other}]")),
            None => {}
        }
    }
    let mut chat = serde_json::json!({
        "role": message.get("role").cloned().unwrap_or(Value::Null),
        "content": if text.is_empty() && !tool_calls.is_empty() {
            Value::Null
        } else {
            Value::String(text.join("\n"))
        },
    });
    if !tool_calls.is_empty() {
        chat["tool_calls"] = Value::Array(tool_calls);
    }
    if let Some(call_id) = message.get("call_id").filter(|c| !c.is_null()) {
        chat["tool_call_id"] = call_id.clone();
    }
    if let Some(name) = message.get("name").filter(|n| !n.is_null()) {
        chat["name"] = name.clone();
    }
    chat
}

/// The run id as an OTLP trace id (32 hex digits). A UUID already is one.
pub(crate) fn trace_id(run_id: &str) -> String {
    let hex: String = run_id.chars().filter(|c| *c != '-').collect();
    if hex.len() == 32 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return hex.to_ascii_lowercase();
    }
    format!("{:016x}{:016x}", fnv(run_id, 1), fnv(run_id, 2))
}

/// A deterministic, non-zero 16-hex-digit span id.
fn span_id(trace: &str, key: &str) -> String {
    let id = fnv(&format!("{trace}|{key}"), 0).max(1);
    format!("{id:016x}")
}

fn fnv(text: &str, salt: u8) -> u64 {
    std::iter::once(salt)
        .chain(text.bytes())
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        })
}

/// Milliseconds since the epoch as ISO 8601 UTC (`2026-10-11T12:00:00.000Z`).
pub(crate) fn iso8601(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ms % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use criogen_core::Event;
    use serde_json::json;

    fn event(seq: u64, node: Option<&str>, kind: EventKind) -> Event {
        serde_json::from_value(json!({
            "seq": seq, "run_id": "01920000-aaaa-7bbb-8ccc-0123456789ab",
            "step": node.map(|_| 0), "node": node, "origin": "internal",
            "ts_ms": 1_000 + seq,
            "type": kind.type_name(), "data": serde_json::to_value(&kind).unwrap()["data"],
        }))
        .unwrap()
    }

    fn custom(seq: u64, name: &str, payload: Value) -> Event {
        event(
            seq,
            Some("model"),
            EventKind::Custom {
                name: name.into(),
                payload,
            },
        )
    }

    fn builder(capture: bool) -> SpanBuilder {
        SpanBuilder::new(ContentPolicy {
            capture,
            mask: None,
            max_chars: 10_000,
        })
    }

    fn model_turn(b: &mut SpanBuilder) -> Vec<Span> {
        let mut spans = Vec::new();
        let start = EventKind::RunStarted {
            resumed: false,
            metadata: Some(
                json!({"user_id": "u1", "session_id": "s1", "tags": ["a"], "ward": "icu"}),
            ),
        };
        spans.extend(b.on_event(&event(0, None, start)));
        spans.extend(b.on_event(&event(
            1,
            Some("model"),
            EventKind::NodeStarted { attempt: 1 },
        )));
        spans.extend(b.on_event(&custom(2, "model_request", json!({
            "provider": "openai", "model": "gpt-x", "settings": {"temperature": 0.2},
            "messages": [{"id": "m1", "role": "user", "content": [{"type": "text", "text": "hi"}]}]
        }))));
        spans.extend(b.on_event(&custom(3, "message_delta", json!({}))));
        spans.extend(b.on_event(&custom(4, "message_completed", json!({
            "message": {"id": "m2", "role": "assistant", "content": [
                {"type": "text", "text": "calling"},
                {"type": "tool_call", "id": "c1", "name": "search", "args": {"q": "x"}}
            ]},
            "finish_reason": "tool_calls", "model": "gpt-x-2026",
            "usage": {"input_tokens": 100, "output_tokens": 30, "cache_read_tokens": 60, "reasoning_tokens": 10}
        }))));
        spans.extend(b.on_event(&event(
            5,
            Some("model"),
            EventKind::NodeFinished {
                duration_ms: 3.0,
                update_keys: vec!["messages".into()],
                goto: vec!["tools".into()],
                suspended: false,
                reused: false,
            },
        )));
        spans.extend(b.on_event(&event(
            6,
            None,
            EventKind::RunFinished {
                status: "completed".into(),
                error: None,
                steps: 1,
            },
        )));
        spans
    }

    fn by_type<'a>(spans: &'a [Span], kind: &str) -> &'a Span {
        spans
            .iter()
            .find(|s| s.attr("langfuse.observation.type") == Some(&Attr::Str(kind.into())))
            .unwrap_or_else(|| panic!("no {kind} span"))
    }

    fn text(span: &Span, key: &str) -> String {
        match span.attr(key) {
            Some(Attr::Str(s)) => s.clone(),
            other => panic!("{key}: {other:?}"),
        }
    }

    #[test]
    fn generation_has_chat_input_output_and_normalized_usage() {
        let mut b = builder(true);
        let spans = model_turn(&mut b);
        assert_eq!(b.open_runs(), 0, "nothing left open");
        let generation = by_type(&spans, "generation");
        let input: Value =
            serde_json::from_str(&text(generation, "langfuse.observation.input")).unwrap();
        assert_eq!(input, json!([{"role": "user", "content": "hi"}]));
        let output: Value =
            serde_json::from_str(&text(generation, "langfuse.observation.output")).unwrap();
        assert_eq!(
            output,
            json!({"role": "assistant", "content": "calling", "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "search", "arguments": "{\"q\":\"x\"}"}}
            ]})
        );
        let usage: Value =
            serde_json::from_str(&text(generation, "langfuse.observation.usage_details")).unwrap();
        assert_eq!(
            usage,
            json!({"input": 40, "output": 20, "total": 130,
                                 "input_cached_tokens": 60, "output_reasoning_tokens": 10})
        );
        assert_eq!(text(generation, "langfuse.observation.model.name"), "gpt-x");
        assert_eq!(text(generation, "gen_ai.response.model"), "gpt-x-2026");
        assert_eq!(
            text(generation, "langfuse.observation.completion_start_time"),
            "1970-01-01T00:00:01.003Z"
        );

        let node = spans.iter().find(|s| s.name == "model").unwrap();
        assert_eq!(generation.parent_id.as_ref(), Some(&node.span_id));
        let root = by_type(&spans, "agent");
        assert_eq!(node.parent_id.as_ref(), Some(&root.span_id));
        assert_eq!(root.trace_id, "01920000aaaa7bbb8ccc0123456789ab");
        assert_eq!(
            text(root, "langfuse.observation.input"),
            text(generation, "langfuse.observation.input")
        );
        // Trace fields are on every span.
        for span in &spans {
            assert_eq!(text(span, "langfuse.user.id"), "u1");
            assert_eq!(text(span, "langfuse.session.id"), "s1");
            assert_eq!(text(span, "langfuse.trace.metadata.ward"), "icu");
        }
    }

    #[test]
    fn content_capture_off_sends_no_content() {
        let mut b = builder(false);
        let spans = model_turn(&mut b);
        for span in &spans {
            for key in ["langfuse.observation.input", "langfuse.observation.output"] {
                assert!(span.attr(key).is_none(), "{} has {key}", span.name);
            }
        }
        // Usage and model still go.
        let generation = by_type(&spans, "generation");
        assert!(
            generation
                .attr("langfuse.observation.usage_details")
                .is_some()
        );
    }

    #[test]
    fn iso8601_matches_known_dates() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601(1_709_164_800_123), "2024-02-29T00:00:00.123Z");
        assert_eq!(iso8601(784_111_777_000), "1994-11-06T08:49:37.000Z");
    }

    #[test]
    fn uuid_run_ids_are_trace_ids() {
        assert_eq!(
            trace_id("01920000-aaaa-7bbb-8ccc-0123456789AB"),
            "01920000aaaa7bbb8ccc0123456789ab"
        );
        let hashed = trace_id("not-a-uuid");
        assert_eq!(hashed.len(), 32);
        assert_eq!(hashed, trace_id("not-a-uuid"));
    }

    #[test]
    fn content_policy_masks_truncates_and_survives_panics() {
        let mut policy = ContentPolicy {
            capture: true,
            mask: None,
            max_chars: 5,
        };
        assert_eq!(
            policy.render(&Value::String("abcdefgh".into())).unwrap(),
            "abcde…[truncated: 3 more characters]"
        );
        policy.max_chars = 100;
        policy.mask = Some(Arc::new(|_| Value::String("***".into())));
        assert_eq!(
            policy.render(&serde_json::json!({"ssn": 1})).unwrap(),
            "***"
        );
        policy.mask = Some(Arc::new(|_| panic!("bad mask")));
        assert_eq!(
            policy.render(&serde_json::json!(1)).unwrap(),
            "[mask failed]"
        );
        policy.capture = false;
        assert_eq!(policy.render(&serde_json::json!("x")), None);
    }
}
