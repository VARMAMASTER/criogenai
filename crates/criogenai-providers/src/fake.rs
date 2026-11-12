//! A simulated model for tests and examples. Never calls a network.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
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
/// ([`then`](Self::then)) are used first, in order; with
/// [`by_turn`](Self::by_turn) they are chosen by the conversation's turn
/// instead, so one model can serve many runs, also at once.
#[derive(Debug)]
pub struct FakeModel {
    min: Duration,
    max: Duration,
    fail_marker: Option<String>,
    flaky_calls: usize,
    by_turn: bool,
    log_requests: bool,
    script: Mutex<VecDeque<FakeReply>>,
    calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
    provider: String,
    model: String,
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
            by_turn: false,
            log_requests: true,
            script: Mutex::new(VecDeque::new()),
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            provider: "fake".into(),
            model: "fake".into(),
        }
    }

    /// Reports this provider and model name (default `fake` / `fake`), e.g.
    /// to tell a primary model from a fallback in tests.
    pub fn named(mut self, provider: impl Into<String>, model: impl Into<String>) -> Self {
        self.provider = provider.into();
        self.model = model.into();
        self
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

    /// Chooses the scripted reply by the request's turn (the number of
    /// assistant messages it already holds) instead of using replies up:
    /// turn 0 gets the first reply, turn 1 the second, and past the end the
    /// last one repeats. Concurrent runs each get their own sequence.
    pub fn by_turn(mut self) -> Self {
        self.by_turn = true;
        self
    }

    /// Keeps no copy of the requests, for long benchmarks: [`requests`]
    /// is then empty ([`calls`] still counts).
    ///
    /// [`requests`]: Self::requests
    /// [`calls`]: Self::calls
    pub fn without_request_log(mut self) -> Self {
        self.log_requests = false;
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
        self.calls.load(Ordering::SeqCst)
    }

    /// Every request received, in order (none with
    /// [`without_request_log`](Self::without_request_log)).
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().expect("requests lock").clone()
    }

    async fn answer(&self, request: &ModelRequest) -> Result<ModelResponse, ModelError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.log_requests {
            self.requests
                .lock()
                .expect("requests lock")
                .push(request.clone());
        }
        let prompt = request
            .messages
            .last()
            .map(Message::text)
            .unwrap_or_default();
        let delay = self.delay_for(&prompt);
        if delay.is_zero() {
            // A timer, even of zero, waits for the next tick (about 1 ms on
            // Windows): yield instead, so zero means zero.
            YieldOnce(false).await;
        } else {
            criogenai_rt::sleep(delay).await;
        }

        let fail = |kind, message: String| {
            Err(ModelError::new(kind, message).for_model(self.provider.clone(), self.model.clone()))
        };
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

        let scripted = {
            let mut script = self.script.lock().expect("script lock");
            if self.by_turn {
                let turn = request
                    .messages
                    .iter()
                    .filter(|m| m.role == Role::Assistant)
                    .count();
                script.get(turn).or_else(|| script.back()).cloned()
            } else {
                script.pop_front()
            }
        };
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
        message.metadata.model = Some(self.model.clone());
        message.metadata.provider = Some(self.provider.clone());
        let input: usize = request.messages.iter().map(|m| m.text().len()).sum();
        let output = message.text().len();
        let usage = Usage::new(estimate_tokens(input), estimate_tokens(output));
        Ok(ModelResponse::new(message, usage, finish))
    }
}

impl Model for FakeModel {
    fn provider(&self) -> &str {
        &self.provider
    }

    fn model(&self) -> &str {
        &self.model
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

/// Lets other tasks run once, without a timer (and without Tokio's `rt`
/// feature, which this crate does not use).
struct YieldOnce(bool);

impl std::future::Future for YieldOnce {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.0 {
            return std::task::Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
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
