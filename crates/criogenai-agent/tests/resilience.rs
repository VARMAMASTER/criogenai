//! Model retries that honor `Retry-After`, and fallback models (design 0003
//! §5), with `FakeModel` on the paused clock: every wait is exact.

use std::sync::Arc;
use std::time::Duration;

use criogenai_agent::Agent;
use criogenai_core::{EventKind, RetryPolicy, RunOptions, event_channel};
use criogenai_providers::{Dispatch, FakeModel, FakeReply, Message, ModelError, ModelErrorKind};
use serde_json::json;
use tokio::time::Instant;

const SECOND: Duration = Duration::from_secs(1);

fn instant(name: &str, replies: Vec<FakeReply>) -> Arc<FakeModel> {
    let mut model =
        FakeModel::with_delay(Duration::ZERO, Duration::ZERO).named(name, format!("{name}-model"));
    for reply in replies {
        model = model.then(reply);
    }
    Arc::new(model)
}

fn error(kind: ModelErrorKind) -> ModelError {
    ModelError::new(kind, format!("simulated {kind}"))
}

fn fixed_backoff() -> RetryPolicy {
    RetryPolicy::new()
        .max_attempts(3)
        .backoff(SECOND, 2.0, Duration::from_secs(30))
        .jitter(false)
}

#[tokio::test(start_paused = true)]
async fn retry_after_is_honored_exactly() {
    let model = instant(
        "p",
        vec![
            FakeReply::Error(error(ModelErrorKind::RateLimited).with_retry_after(7 * SECOND)),
            FakeReply::text("ok"),
        ],
    );
    let agent = Agent::builder_shared(model.clone())
        .model_retry(fixed_backoff())
        .build()
        .unwrap();
    let started = Instant::now();
    let result = agent.run("hi").await.unwrap();
    assert_eq!(
        started.elapsed(),
        7 * SECOND,
        "the provider's wait, not the backoff"
    );
    assert_eq!(result.text(), Some("ok"));
    assert_eq!((model.calls(), result.model_calls), (2, 1));
}

#[tokio::test(start_paused = true)]
async fn an_unreasonable_retry_after_fails_at_once() {
    let model = instant(
        "p",
        vec![FakeReply::Error(
            error(ModelErrorKind::RateLimited).with_retry_after(Duration::from_secs(600)),
        )],
    );
    let agent = Agent::builder_shared(model.clone())
        .model_retry(fixed_backoff())
        .build()
        .unwrap();
    let started = Instant::now();
    let err = agent.run("hi").await.unwrap_err();
    assert_eq!(
        started.elapsed(),
        Duration::ZERO,
        "no 10-minute hang (0003 E5)"
    );
    assert_eq!(err.code, "model_failed");
    assert_eq!(err.details.unwrap()["model_error"]["kind"], "rate_limited");
    assert_eq!(model.calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn requests_that_never_left_are_retried_immediately() {
    let model = instant(
        "p",
        vec![
            FakeReply::Error(error(ModelErrorKind::Network).with_dispatch(Dispatch::NotSent)),
            FakeReply::text("ok"),
        ],
    );
    let agent = Agent::builder_shared(model.clone())
        .model_retry(fixed_backoff())
        .build()
        .unwrap();
    let started = Instant::now();
    agent.run("hi").await.unwrap();
    assert_eq!(started.elapsed(), Duration::ZERO);
    assert_eq!(model.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn an_overloaded_primary_falls_back_without_sleeping() {
    let primary = instant(
        "primary",
        vec![FakeReply::Error(error(ModelErrorKind::Overloaded))],
    );
    let fallback = instant("backup", vec![FakeReply::text("from the backup")]);
    let agent = Agent::builder_shared(primary.clone())
        .fallback_shared(fallback.clone())
        .model_retry(fixed_backoff())
        .build()
        .unwrap();
    let started = Instant::now();
    let result = agent.run("hi").await.unwrap();
    assert_eq!(started.elapsed(), Duration::ZERO);
    assert_eq!(result.text(), Some("from the backup"));
    assert_eq!((primary.calls(), fallback.calls()), (1, 1));
    assert_eq!(
        result.messages.last().unwrap().metadata.provider.as_deref(),
        Some("backup")
    );
}

#[tokio::test(start_paused = true)]
async fn server_errors_fall_back_after_the_retries() {
    let primary = instant(
        "primary",
        vec![
            FakeReply::Error(error(ModelErrorKind::Server)),
            FakeReply::Error(error(ModelErrorKind::Server)),
            FakeReply::Error(error(ModelErrorKind::Server)),
        ],
    );
    let fallback = instant("backup", vec![FakeReply::text("ok")]);
    let agent = Agent::builder_shared(primary.clone())
        .fallback_shared(fallback.clone())
        .model_retry(fixed_backoff())
        .build()
        .unwrap();
    let started = Instant::now();
    agent.run("hi").await.unwrap();
    assert_eq!(
        started.elapsed(),
        3 * SECOND,
        "1 s + 2 s backoff, then the fallback"
    );
    assert_eq!((primary.calls(), fallback.calls()), (3, 1));
}

#[tokio::test(start_paused = true)]
async fn fatal_errors_fall_back_only_when_asked() {
    let overflow = || FakeReply::Error(error(ModelErrorKind::ContextOverflow));
    let primary = instant("small", vec![overflow()]);
    let fallback = instant("large", vec![FakeReply::text("fits")]);
    let agent = Agent::builder_shared(primary)
        .fallback_shared(fallback.clone())
        .build()
        .unwrap();
    let err = agent.run("long").await.unwrap_err();
    assert_eq!(err.code, "model_failed");
    assert_eq!(fallback.calls(), 0, "context overflow is not transient");

    let agent = Agent::builder_shared(instant("small", vec![overflow()]))
        .fallback_shared(fallback.clone())
        .fallback_on([ModelErrorKind::ContextOverflow])
        .build()
        .unwrap();
    assert_eq!(agent.run("long").await.unwrap().text(), Some("fits"));
}

#[tokio::test(start_paused = true)]
async fn provider_specific_fields_are_dropped_for_the_fallback() {
    let primary = instant(
        "anthropic",
        vec![FakeReply::Error(error(ModelErrorKind::Overloaded))],
    );
    let fallback = instant("openai", vec![FakeReply::text("ok")]);
    let agent = Agent::builder_shared(primary.clone())
        .fallback_shared(fallback.clone())
        .hook(AddExtras)
        .build()
        .unwrap();
    agent.run("hi").await.unwrap();
    let sent_to_primary = &primary.requests()[0].extras;
    assert!(sent_to_primary.contains_key("anthropic") && sent_to_primary.contains_key("openai"));
    let sent_to_fallback = &fallback.requests()[0].extras;
    assert!(!sent_to_fallback.contains_key("anthropic"));
    assert!(sent_to_fallback.contains_key("openai"));
}

/// Adds provider-specific request fields, as an app would.
struct AddExtras;

impl criogenai_agent::Hook for AddExtras {
    fn name(&self) -> &str {
        "extras"
    }
    fn wrap_model<'a>(
        &'a self,
        mut request: criogenai_providers::ModelRequest,
        next: criogenai_agent::ModelNext<'a>,
    ) -> criogenai_providers::BoxFuture<'a, Result<criogenai_providers::ModelResponse, ModelError>>
    {
        request
            .extras
            .insert("anthropic".into(), json!({"top_k": 5}));
        request.extras.insert("openai".into(), json!({"seed": 1}));
        next.call(request)
    }
}

#[tokio::test(start_paused = true)]
async fn a_wait_past_the_deadline_fails_immediately() {
    let model = instant(
        "p",
        vec![FakeReply::Error(
            error(ModelErrorKind::RateLimited).with_retry_after(30 * SECOND),
        )],
    );
    let agent = Agent::builder_shared(model).build().unwrap();
    let started = Instant::now();
    let err = agent
        .run_with("hi", agent.run_options().deadline(10 * SECOND))
        .await
        .unwrap_err();
    assert_eq!(err.code, "deadline_exceeded");
    assert_eq!(started.elapsed(), Duration::ZERO, "no point waiting 30 s");
}

#[tokio::test(start_paused = true)]
async fn every_failed_attempt_is_an_event() {
    let primary = instant(
        "primary",
        vec![
            FakeReply::Error(error(ModelErrorKind::Server)),
            FakeReply::Error(error(ModelErrorKind::Overloaded)),
        ],
    );
    let fallback = instant("backup", vec![FakeReply::text("ok")]);
    let agent = Agent::builder_shared(primary)
        .fallback_shared(fallback)
        .model_retry(fixed_backoff())
        .build()
        .unwrap();
    let (sender, mut receiver) = event_channel(64);
    let run = agent.run_with(
        vec![Message::user("hi")],
        RunOptions::default().events(sender),
    );
    let read = async {
        let mut names = Vec::new();
        while let Some(event) = receiver.recv().await {
            if let EventKind::Custom { name, payload } = event.kind
                && name.starts_with("model_")
                && name != "model_request"
            {
                names.push(format!(
                    "{name}:{}",
                    payload["kind"].as_str().unwrap_or_default()
                ));
            }
        }
        names
    };
    let (result, names) = tokio::join!(run, read);
    result.unwrap();
    assert_eq!(
        names,
        [
            "model_attempt_failed:server",
            "model_attempt_failed:overloaded",
            "model_fallback:overloaded",
        ]
    );
}
