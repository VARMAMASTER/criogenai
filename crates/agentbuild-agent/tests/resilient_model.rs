//! `ResilientModel` (design 0018 §3): the agent's retry and fallback chain
//! on its own, with `FakeModel` on the paused clock.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use agentbuild_agent::ResilientModel;
use agentbuild_core::RetryPolicy;
use agentbuild_providers::{
    BlockDelta, BoxFuture, Capabilities, DeltaSink, Dispatch, FakeModel, FakeReply, Message,
    MessageDelta, Model, ModelError, ModelErrorKind, ModelRequest, ModelResponse,
};

fn request(text: &str) -> ModelRequest {
    ModelRequest::new(vec![Message::user(text)])
}

fn fake(name: &str, reply: &str) -> FakeModel {
    FakeModel::with_delay(Duration::from_secs(1), Duration::from_secs(1))
        .named("fake", name)
        .then(FakeReply::text(reply))
}

#[tokio::test(start_paused = true)]
async fn transient_errors_are_retried_with_backoff() {
    let primary = Arc::new(fake("primary", "hello").flaky_first(2));
    let model = ResilientModel::new(primary.clone()).retry(
        RetryPolicy::new()
            .max_attempts(3)
            .backoff(Duration::from_secs(2), 2.0, Duration::from_secs(60))
            .jitter(false),
    );
    let started = tokio::time::Instant::now();
    let response = model.invoke(&request("hi")).await.unwrap();
    assert_eq!(response.message.text(), "hello");
    assert_eq!(primary.calls(), 3);
    // Three 1 s calls, waits of 2 s and 4 s between them.
    assert_eq!(started.elapsed(), Duration::from_secs(9));

    // Out of attempts: the last error.
    let always = Arc::new(fake("primary", "never").flaky_first(5));
    let error = ResilientModel::new(always.clone())
        .retry(RetryPolicy::new().max_attempts(2))
        .invoke(&request("hi"))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ModelErrorKind::RateLimited);
    assert_eq!(always.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn fallbacks_answer_and_the_message_names_them() {
    // Rate limited with a fallback: move on at once.
    let primary = Arc::new(fake("primary", "never").flaky_first(1));
    let backup = Arc::new(fake("backup", "from the backup"));
    let model = ResilientModel::new(primary.clone()).fallback(backup.clone());
    let started = tokio::time::Instant::now();
    let response = model.invoke(&request("hi")).await.unwrap();
    assert_eq!(response.message.text(), "from the backup");
    assert_eq!(response.message.metadata.model.as_deref(), Some("backup"));
    assert_eq!((primary.calls(), backup.calls()), (1, 1));
    assert_eq!(started.elapsed(), Duration::from_secs(2), "no backoff wait");
    assert_eq!(model.model(), "primary");

    // A non-transient error falls back only when asked to.
    let strict = |fallback_on: bool| {
        let primary = Arc::new(fake("primary", "never").failing_on("too long"));
        let model = ResilientModel::new(primary).fallback(Arc::new(fake("backup", "big context")));
        if fallback_on {
            model.fallback_on(ModelErrorKind::InvalidRequest)
        } else {
            model
        }
    };
    let error = strict(false)
        .invoke(&request("too long"))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ModelErrorKind::InvalidRequest);
    let response = strict(true).invoke(&request("too long")).await.unwrap();
    assert_eq!(response.message.text(), "big context");
}

/// Streams "par", fails transiently once, then streams the whole answer.
struct BreaksMidStream {
    calls: AtomicUsize,
}

impl Model for BreaksMidStream {
    fn provider(&self) -> &str {
        "test"
    }
    fn model(&self) -> &str {
        "breaks"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn invoke<'a>(
        &'a self,
        _: &'a ModelRequest,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        unreachable!("only streamed")
    }
    fn stream<'a>(
        &'a self,
        _: &'a ModelRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let message = Message::assistant("partial answer");
            let delta = |text: &str| MessageDelta {
                message_id: message.id.clone(),
                index: 0,
                delta: BlockDelta::Text { text: text.into() },
            };
            if call == 0 {
                on_delta(delta("par"));
                let mut error = ModelError::new(ModelErrorKind::Overloaded, "dropped");
                error.dispatch = Dispatch::Sent;
                error.emitted_deltas = true;
                return Err(error);
            }
            on_delta(delta("partial answer"));
            Ok(ModelResponse::new(
                message,
                Default::default(),
                agentbuild_providers::FinishReason::Stop,
            ))
        })
    }
}

#[tokio::test(start_paused = true)]
async fn a_retried_stream_starts_a_new_message() {
    let model = ResilientModel::new(Arc::new(BreaksMidStream {
        calls: AtomicUsize::new(0),
    }));
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut sink = |delta: MessageDelta| {
        if let BlockDelta::Text { text } = delta.delta {
            seen.push((delta.message_id, text));
        }
    };
    let response = model.stream(&request("hi"), &mut sink).await.unwrap();
    assert_eq!(response.message.text(), "partial answer");
    assert_eq!(seen.len(), 2);
    assert_ne!(seen[0].0, seen[1].0, "the retry is a new message");
    assert_eq!(seen[1].0, response.message.id);
    assert!(format!("{model:?}").contains("test:breaks"));
}
