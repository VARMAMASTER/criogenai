//! A simulated model for tests and examples. Never calls a network.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::error::{ModelError, ModelErrorKind};
use crate::message::{ContentBlock, FinishReason, Message, Role, Usage};
use crate::model::{BoxFuture, Model};
use crate::request::{Capabilities, ModelRequest, ModelResponse};

/// A model that answers after a simulated delay (2–3 s by default, fixed per
/// prompt so timing is reproducible). Run tests on Tokio's paused clock
/// (`#[tokio::test(start_paused = true)]`) to make them instant and exact.
///
/// The prompt is the text of the request's last message. Without a script,
/// the answer is `"[fake-llm] reply to: <prompt>"`. Scripted replies
/// ([`then`](Self::then)) are used first, in order.
#[derive(Debug)]
pub struct FakeModel {
    min: Duration,
    max: Duration,
    fail_marker: Option<String>,
    flaky_calls: usize,
    script: Mutex<VecDeque<FakeReply>>,
    requests: Mutex<Vec<ModelRequest>>,
}

/// One scripted answer.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum FakeReply {
    /// An assistant message with this text.
    Text(String),
    /// An assistant message calling these tools, `(name, args)`. Call ids
    /// are `call_<n>_<i>` (n = call number, i = position).
    ToolCalls(Vec<(String, Map<String, Value>)>),
    /// A failure.
    Error(ModelError),
    /// This exact assistant message (e.g. with an invalid tool call). Its
    /// `metadata.finish_reason`, if set, is the response's finish reason.
    /// Build it with [`FakeReply::message`].
    Message(Box<Message>),
}

impl FakeReply {
    /// This exact assistant message.
    pub fn message(message: Message) -> Self {
        Self::Message(Box::new(message))
    }

    /// A text answer.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    /// One tool call. `args` must be a JSON object (anything else becomes
    /// `{}`).
    pub fn tool_call(name: impl Into<String>, args: Value) -> Self {
        Self::ToolCalls(vec![(name.into(), into_object(args))])
    }

    /// Several tool calls in one answer.
    pub fn tool_calls<I, S>(calls: I) -> Self
    where
        I: IntoIterator<Item = (S, Value)>,
        S: Into<String>,
    {
        Self::ToolCalls(
            calls
                .into_iter()
                .map(|(name, args)| (name.into(), into_object(args)))
                .collect(),
        )
    }
}

fn into_object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

impl Default for FakeModel {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeModel {
    /// Replies take 2–3 s.
    pub fn new() -> Self {
        Self::with_delay(Duration::from_secs(2), Duration::from_secs(3))
    }

    /// Replies take between `min` and `max`.
    pub fn with_delay(min: Duration, max: Duration) -> Self {
        assert!(min <= max, "min delay must not exceed max delay");
        Self {
            min,
            max,
            fail_marker: None,
            flaky_calls: 0,
            script: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Prompts containing `marker` fail with `invalid_request` (not
    /// transient, so never retried).
    pub fn failing_on(mut self, marker: impl Into<String>) -> Self {
        self.fail_marker = Some(marker.into());
        self
    }

    /// The first `calls` calls fail with `rate_limited` (transient), after
    /// the usual delay.
    pub fn flaky_first(mut self, calls: usize) -> Self {
        self.flaky_calls = calls;
        self
    }

    /// Adds a scripted reply; scripted replies are used in order.
    pub fn then(self, reply: FakeReply) -> Self {
        self.script.lock().expect("script lock").push_back(reply);
        self
    }

    /// How long the reply to `prompt` takes.
    pub fn delay_for(&self, prompt: &str) -> Duration {
        let span = (self.max - self.min).as_millis() as u64;
        let offset = if span == 0 {
            0
        } else {
            fnv1a(prompt) % (span + 1)
        };
        self.min + Duration::from_millis(offset)
    }

    /// Number of calls so far.
    pub fn calls(&self) -> usize {
        self.requests.lock().expect("requests lock").len()
    }

    /// Every request received, in order.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().expect("requests lock").clone()
    }

    async fn answer(&self, request: &ModelRequest) -> Result<ModelResponse, ModelError> {
        let call = {
            let mut requests = self.requests.lock().expect("requests lock");
            requests.push(request.clone());
            requests.len()
        };
        let prompt = request
            .messages
            .last()
            .map(Message::text)
            .unwrap_or_default();
        tokio::time::sleep(self.delay_for(&prompt)).await;

        let fail =
            |kind, message: String| Err(ModelError::new(kind, message).for_model("fake", "fake"));
        if call <= self.flaky_calls {
            return fail(
                ModelErrorKind::RateLimited,
                format!("simulated rate limit (call {call})"),
            );
        }
        if let Some(marker) = &self.fail_marker
            && prompt.contains(marker.as_str())
        {
            return fail(
                ModelErrorKind::InvalidRequest,
                format!("simulated failure: prompt contains `{marker}`"),
            );
        }

        let scripted = self.script.lock().expect("script lock").pop_front();
        let (content, finish) = match scripted {
            Some(FakeReply::Error(error)) => return Err(error),
            Some(FakeReply::Message(message)) => {
                let finish = if let Some(reason) = message.metadata.finish_reason {
                    reason
                } else if message.call_ids().next().is_some() {
                    FinishReason::ToolCalls
                } else {
                    FinishReason::Stop
                };
                (message.content, finish)
            }
            Some(FakeReply::Text(text)) => (vec![ContentBlock::text(text)], FinishReason::Stop),
            Some(FakeReply::ToolCalls(calls)) => (
                calls
                    .into_iter()
                    .enumerate()
                    .map(|(i, (name, args))| {
                        ContentBlock::tool_call(format!("call_{call}_{i}"), name, args)
                    })
                    .collect(),
                FinishReason::ToolCalls,
            ),
            None => (
                vec![ContentBlock::text(format!("[fake-llm] reply to: {prompt}"))],
                FinishReason::Stop,
            ),
        };

        let mut message = Message::with_content(Role::Assistant, content);
        message.metadata.model = Some("fake".into());
        message.metadata.provider = Some("fake".into());
        let input: usize = request.messages.iter().map(|m| m.text().len()).sum();
        let output = message.text().len();
        let usage = Usage::new(estimate_tokens(input), estimate_tokens(output));
        Ok(ModelResponse::new(message, usage, finish))
    }
}

impl Model for FakeModel {
    fn provider(&self) -> &str {
        "fake"
    }

    fn model(&self) -> &str {
        "fake"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tools: Some(true),
            parallel_tool_calls: Some(true),
            native_structured_output: Some(false),
            streaming: Some(false),
            vision: Some(false),
            reasoning: Some(false),
            tool_result_images: Some(false),
            max_context_tokens: Some(128_000),
        }
    }

    fn invoke<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        Box::pin(self.answer(request))
    }
}

/// About four bytes per token, at least one token for non-empty text.
fn estimate_tokens(bytes: usize) -> u64 {
    (bytes as u64).div_ceil(4)
}

fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}
