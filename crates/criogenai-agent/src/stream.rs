//! Streaming an agent run (design 0004 §8) and a thread operation (0008
//! §17).

use std::future::Future;

use criogenai_core::{CancelToken, Event, EventKind, EventReceiver, RunOptions, event_channel};
use criogenai_providers::{BlockDelta, Message, MessageDelta, ToolStatus};
use criogenai_rt::JoinHandle;
use serde_json::{Map, Value};

use crate::agent::{Agent, AgentError, AgentInput, AgentResult};
use crate::approval::{ApprovalRequest, Decision};
use crate::thread::{Reply, Thread};

/// Channel capacity for [`Agent::stream`].
const STREAM_CAPACITY: usize = 512;

/// What happened in an agent run, for a UI.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum AgentEvent {
    /// More of the model's answer.
    Text {
        /// The message being written.
        message_id: String,
        /// The new text.
        text: String,
    },
    /// A tool call started.
    ToolCallStarted {
        /// Call id.
        call_id: String,
        /// Tool name.
        tool: String,
        /// Arguments.
        args: Map<String, Value>,
    },
    /// A tool call finished.
    ToolCallFinished {
        /// Call id.
        call_id: String,
        /// Tool name.
        tool: String,
        /// How it ended.
        status: ToolStatus,
    },
    /// The model finished a message (the authoritative version).
    Message(Box<Message>),
    /// The run paused for approval; resume with [`Agent::resume`].
    ApprovalNeeded(Vec<ApprovalRequest>),
    /// Any other event (steps, nodes, retries, warnings, custom events).
    Other(Box<Event>),
}

impl AgentEvent {
    /// Classifies a run event.
    pub fn from_event(event: Event) -> Self {
        if let EventKind::Suspended { interrupts } = &event.kind {
            let requests: Vec<ApprovalRequest> = interrupts
                .iter()
                .filter_map(|i| {
                    serde_json::from_value::<Vec<ApprovalRequest>>(i.payload["requests"].clone())
                        .ok()
                })
                .flatten()
                .collect();
            if !requests.is_empty() {
                return AgentEvent::ApprovalNeeded(requests);
            }
        }
        match &event.kind {
            EventKind::Custom { name, payload } => custom(name, payload),
            _ => None,
        }
        .unwrap_or_else(|| AgentEvent::Other(Box::new(event)))
    }
}

/// The agent's own custom events, typed.
fn custom(name: &str, payload: &Value) -> Option<AgentEvent> {
    match name {
        "message_delta" => {
            let delta: MessageDelta = serde_json::from_value(payload.clone()).ok()?;
            match delta.delta {
                BlockDelta::Text { text } => Some(AgentEvent::Text {
                    message_id: delta.message_id,
                    text,
                }),
                _ => None,
            }
        }
        "tool_call_started" => Some(AgentEvent::ToolCallStarted {
            call_id: payload["call_id"].as_str()?.to_string(),
            tool: payload["tool"].as_str()?.to_string(),
            args: payload["args"].as_object().cloned().unwrap_or_default(),
        }),
        "tool_call_finished" => Some(AgentEvent::ToolCallFinished {
            call_id: payload["call_id"].as_str()?.to_string(),
            tool: payload["tool"].as_str()?.to_string(),
            status: serde_json::from_value(payload["status"].clone()).ok()?,
        }),
        "message_completed" => serde_json::from_value(payload["message"].clone())
            .ok()
            .map(|m| AgentEvent::Message(Box::new(m))),
        _ => None,
    }
}

/// An agent run in the background, with its events. Created by
/// [`Agent::stream`].
///
/// Read events with [`next`](Self::next) until it returns `None`, then call
/// [`finish`](Self::finish). [`cancel`](Self::cancel) stops the run
/// gracefully; dropping the stream stops it at once.
#[derive(Debug)]
pub struct AgentStream {
    events: Option<EventReceiver>,
    handle: Option<JoinHandle<Result<AgentResult, AgentError>>>,
    cancel: CancelToken,
}

impl AgentStream {
    /// The next event, or `None` after the run's last one.
    pub async fn next(&mut self) -> Option<AgentEvent> {
        let event = self.events.as_mut()?.recv().await?;
        Some(AgentEvent::from_event(event))
    }

    /// Cancels the run (it fails with `cancelled`).
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Waits for the run and returns its result. Unread events are dropped.
    pub async fn finish(mut self) -> Result<AgentResult, AgentError> {
        self.events = None;
        let handle = self.handle.take().expect("finish consumes the stream");
        match handle.await {
            Ok(result) => result,
            Err(err) if err.is_panic() => std::panic::resume_unwind(err.into_panic()),
            Err(_) => panic!("agent task was cancelled (is the Tokio runtime shutting down?)"),
        }
    }
}

impl Drop for AgentStream {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

impl Agent {
    /// Runs the agent in the background (on the current Tokio runtime) and
    /// returns its events as they happen.
    pub fn stream(&self, input: impl Into<AgentInput>) -> AgentStream {
        self.stream_with(input, self.run_options())
    }

    /// [`stream`](Self::stream) with options. Any event sender or cancel
    /// token in `options` is replaced: cancel with [`AgentStream::cancel`].
    pub fn stream_with(
        &self,
        input: impl Into<AgentInput>,
        mut options: RunOptions,
    ) -> AgentStream {
        let (sender, events) = event_channel(STREAM_CAPACITY);
        let cancel = CancelToken::new();
        options = options.cancel_token(cancel.clone()).events(sender);
        let agent = self.clone();
        let input = input.into();
        let handle = criogenai_rt::spawn(async move { agent.run_with(input, options).await });
        AgentStream {
            events: Some(events),
            handle: Some(handle),
            cancel,
        }
    }
}

/// A thread operation in the background, with its events (design 0008
/// §17). Created by [`Thread::stream`] and the other `stream_*` methods.
///
/// Read events with [`next`](Self::next) until it returns `None`, then call
/// [`finish`](Self::finish) for the saved [`Reply`]. Dropping the stream
/// (a client that went away) cancels the run instead of aborting it, so the
/// run is still saved (as failed with `cancelled`) and
/// [`Thread::retry`] continues it.
#[derive(Debug)]
pub struct ThreadStream {
    events: Option<EventReceiver>,
    handle: Option<JoinHandle<Result<Reply, AgentError>>>,
    cancel: CancelToken,
}

impl ThreadStream {
    /// Runs `operation` on `thread` in a Tokio task, with an event channel
    /// and a cancel token added to `options`.
    fn spawn<F, Fut>(thread: &Thread, mut options: RunOptions, operation: F) -> Self
    where
        F: FnOnce(Thread, RunOptions) -> Fut,
        Fut: Future<Output = Result<Reply, AgentError>> + Send + 'static,
    {
        let (sender, events) = event_channel(STREAM_CAPACITY);
        let cancel = CancelToken::new();
        options = options.cancel_token(cancel.clone()).events(sender);
        let handle = criogenai_rt::spawn(operation(thread.clone(), options));
        Self {
            events: Some(events),
            handle: Some(handle),
            cancel,
        }
    }

    /// The next event, or `None` after the run's last one (at once if the
    /// operation failed before its run started).
    pub async fn next(&mut self) -> Option<AgentEvent> {
        let event = self.events.as_mut()?.recv().await?;
        Some(AgentEvent::from_event(event))
    }

    /// Cancels the run (it fails with `cancelled` and is saved).
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// The token that cancels this run: for cancelling from another task
    /// while one waits in [`next`](Self::next).
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Waits for the operation and returns its reply, as the non-streaming
    /// method would. Unread events are dropped.
    pub async fn finish(mut self) -> Result<Reply, AgentError> {
        self.events = None;
        let handle = self.handle.take().expect("finish consumes the stream");
        match handle.await {
            Ok(result) => result,
            Err(err) if err.is_panic() => std::panic::resume_unwind(err.into_panic()),
            Err(_) => panic!("thread task was cancelled (is the Tokio runtime shutting down?)"),
        }
    }
}

impl Drop for ThreadStream {
    fn drop(&mut self) {
        // Cancel, never abort: the operation must still save the thread.
        if self.handle.is_some() {
            self.cancel.cancel();
        }
    }
}

impl Thread {
    /// [`send`](Self::send), streaming its events (design 0008 §17).
    pub fn stream(&self, input: impl Into<AgentInput>) -> ThreadStream {
        self.stream_with(input, self.run_options())
    }

    /// [`stream`](Self::stream) with options. Any event sender or cancel
    /// token in `options` is replaced: cancel with [`ThreadStream::cancel`].
    pub fn stream_with(&self, input: impl Into<AgentInput>, options: RunOptions) -> ThreadStream {
        let input = input.into();
        ThreadStream::spawn(self, options, move |thread, options| async move {
            thread.send_with(input, options).await
        })
    }

    /// [`approve`](Self::approve), streaming.
    pub fn stream_approve(&self) -> ThreadStream {
        ThreadStream::spawn(self, self.run_options(), |thread, options| async move {
            thread.answer_all_with(Decision::approve, options).await
        })
    }

    /// [`reject`](Self::reject), streaming.
    pub fn stream_reject(&self, message: impl Into<String>) -> ThreadStream {
        let message = message.into();
        ThreadStream::spawn(self, self.run_options(), |thread, options| async move {
            thread
                .answer_all_with(|| Decision::reject(message.clone()), options)
                .await
        })
    }

    /// [`decide`](Self::decide), streaming.
    pub fn stream_decide<I>(&self, decisions: I) -> ThreadStream
    where
        I: IntoIterator<Item = (String, Decision)>,
    {
        let decisions: Vec<(String, Decision)> = decisions.into_iter().collect();
        ThreadStream::spawn(self, self.run_options(), |thread, options| async move {
            thread.decide_with(decisions, options).await
        })
    }

    /// [`retry`](Self::retry), streaming.
    pub fn stream_retry(&self) -> ThreadStream {
        ThreadStream::spawn(self, self.run_options(), |thread, options| async move {
            thread.retry_with(options).await
        })
    }
}
