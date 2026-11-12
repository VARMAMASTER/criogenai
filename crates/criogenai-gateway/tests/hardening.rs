//! Gateway hardening from the October 2026 security audit: accounting that a
//! client cannot dodge, bounded concurrency and memory, and errors that tell
//! a client no more than it needs.

use std::sync::Arc;
use std::time::Duration;

use criogenai_gateway::{BudgetPeriod, Gateway, KeyLimits, UsageRecord};
use criogenai_providers::{
    BlockDelta, BoxFuture, Capabilities, DeltaSink, FakeModel, FakeReply, FinishReason, Message,
    MessageDelta, Model, ModelError, ModelErrorKind, ModelRequest, ModelResponse, Usage,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const KEY: &str = "team-a-test-key-0001";

fn fake(delay: Duration) -> FakeModel {
    FakeModel::with_delay(delay, delay).named("acme", "fake")
}

async fn serve(gateway: &Gateway) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = gateway.router();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{address}/v1")
}

fn body(max_tokens: u64) -> Value {
    json!({"model": "support", "max_tokens": max_tokens,
        "messages": [{"role": "user", "content": "hi"}]})
}

async fn post(base: &str, key: &str, body: &Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/chat/completions"))
        .bearer_auth(key)
        .json(body)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn a_client_that_disconnects_is_still_charged_its_reservation() {
    let upstream = fake(Duration::from_secs(3)).then(FakeReply::text("never delivered"));
    let gateway = Gateway::builder()
        .route("support", Arc::new(upstream))
        .key("team-a", KEY, ["support"])
        .key_limits(
            "team-a",
            KeyLimits::new().budget(1_000_000, BudgetPeriod::Total),
        )
        .build()
        .unwrap();
    let base = serve(&gateway).await;
    // Ask for 5000 tokens, hang up after a moment.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(200))
        .build()
        .unwrap();
    let gone = client
        .post(format!("{base}/chat/completions"))
        .bearer_auth(KEY)
        .json(&body(5_000))
        .send()
        .await;
    assert!(gone.is_err(), "the client gave up");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let spent = gateway.spent("team-a").unwrap();
    assert!(spent >= 5_000, "the reserved tokens stay charged: {spent}");
}

#[tokio::test]
async fn concurrent_requests_cannot_overshoot_a_budget() {
    let upstream = Arc::new(fake(Duration::from_millis(400)));
    let gateway = Gateway::builder()
        .route("support", upstream)
        .key("team-a", KEY, ["support"])
        .key_limits(
            "team-a",
            KeyLimits::new().budget(1_000, BudgetPeriod::Total),
        )
        .build()
        .unwrap();
    let base = serve(&gateway).await;
    let calls: Vec<_> = (0..12)
        .map(|_| {
            let base = base.clone();
            tokio::spawn(async move { post(&base, KEY, &body(600)).await.status().as_u16() })
        })
        .collect();
    let mut admitted = 0;
    for call in calls {
        if call.await.unwrap() == 200 {
            admitted += 1;
        }
    }
    // Each reserves 600+: the first two fit under 1000 before either is
    // answered; every other one is refused at once.
    assert!(
        (1..=2).contains(&admitted),
        "{admitted} of 12 were admitted"
    );
}

#[tokio::test]
async fn too_many_requests_in_flight_are_refused_with_retry_after() {
    let upstream = Arc::new(fake(Duration::from_millis(600)));
    let (sender, mut usage) = mpsc::channel(16);
    let gateway = Gateway::builder()
        .route("support", upstream)
        .max_concurrent_requests(1)
        .usage(sender)
        .build()
        .unwrap();
    let base = serve(&gateway).await;
    let first = {
        let base = base.clone();
        tokio::spawn(async move { post(&base, "", &body(10)).await.status().as_u16() })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    let second = post(&base, "", &body(10)).await;
    assert_eq!(second.status(), 429);
    assert_eq!(second.headers()["retry-after"], "1");
    let error: Value = second.json().await.unwrap();
    assert_eq!(error["error"]["code"], "concurrency_limited");
    assert_eq!(first.await.unwrap(), 200);
    // Afterwards the slot is free again.
    assert_eq!(post(&base, "", &body(10)).await.status(), 200);
    let record: UsageRecord = usage.recv().await.unwrap();
    assert_eq!(record.outcome, "concurrency_limited");

    // Per key too.
    let upstream = Arc::new(fake(Duration::from_millis(600)));
    let gateway = Gateway::builder()
        .route("support", upstream)
        .key("team-a", KEY, ["support"])
        .key_limits("team-a", KeyLimits::new().max_concurrent(1))
        .build()
        .unwrap();
    let base = serve(&gateway).await;
    let first = {
        let base = base.clone();
        tokio::spawn(async move { post(&base, KEY, &body(10)).await.status().as_u16() })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(post(&base, KEY, &body(10)).await.status(), 429);
    assert_eq!(first.await.unwrap(), 200);
}

#[tokio::test]
async fn max_tokens_is_capped_before_it_reaches_the_upstream() {
    let upstream = Arc::new(fake(Duration::ZERO).then(FakeReply::text("ok")));
    let gateway = Gateway::builder()
        .route("support", upstream.clone())
        .max_tokens(100)
        .build()
        .unwrap();
    let base = serve(&gateway).await;
    assert_eq!(post(&base, "", &body(9_999_999)).await.status(), 200);
    assert_eq!(upstream.requests()[0].settings.max_tokens, Some(100));
}

/// Fails the way an upstream does when its account is the problem.
struct Broken;

impl Model for Broken {
    fn provider(&self) -> &str {
        "acme"
    }
    fn model(&self) -> &str {
        "broken"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn invoke<'a>(
        &'a self,
        _: &'a ModelRequest,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        Box::pin(async {
            Err(ModelError::new(
                ModelErrorKind::Auth,
                "organization org-INTERNAL-4711 has no access; contact billing@acme.example",
            ))
        })
    }
}

#[tokio::test]
async fn upstream_failures_tell_a_client_only_the_kind_and_the_model_stays_private() {
    let build = |expose: bool| {
        Gateway::builder()
            .route("support", Arc::new(Broken))
            .expose_model_header(expose)
            .build()
            .unwrap()
    };
    let base = serve(&build(false)).await;
    let response = post(&base, "", &body(10)).await;
    assert!(response.headers().get("x-criogenai-model").is_none());
    let text = response.text().await.unwrap();
    assert!(
        !text.contains("INTERNAL") && !text.contains("billing@"),
        "{text}"
    );
    assert!(text.contains("upstream provider failed"), "{text}");

    let base = serve(&build(true)).await;
    let response = post(&base, "", &body(10)).await;
    assert_eq!(response.headers()["x-criogenai-model"], "acme/broken");
}

/// Streams a great many events, as fast as it can.
struct Firehose;

impl Model for Firehose {
    fn provider(&self) -> &str {
        "acme"
    }
    fn model(&self) -> &str {
        "firehose"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn invoke<'a>(
        &'a self,
        _: &'a ModelRequest,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        unreachable!("streamed only")
    }
    fn stream<'a>(
        &'a self,
        _: &'a ModelRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        Box::pin(async move {
            let message = Message::assistant("done");
            let filler = "x".repeat(2_000);
            for _ in 0..400_000 {
                on_delta(MessageDelta {
                    message_id: message.id.clone(),
                    index: 0,
                    delta: BlockDelta::Text {
                        text: filler.clone(),
                    },
                });
                // Lets the server task see a stalled client between events.
                tokio::task::yield_now().await;
            }
            Ok(ModelResponse::new(
                message,
                Usage::new(10, 10),
                FinishReason::Stop,
            ))
        })
    }
}

#[tokio::test]
async fn a_client_that_stops_reading_a_stream_is_cut_off_not_buffered() {
    let (sender, mut usage) = mpsc::channel(16);
    let gateway = Gateway::builder()
        .route("support", Arc::new(Firehose))
        .usage(sender)
        .build()
        .unwrap();
    let base = serve(&gateway).await;
    let mut streamed = body(10);
    streamed["stream"] = json!(true);
    // Read the headers, then never read the body: 800 MB would follow.
    let response = post(&base, "", &streamed).await;
    assert_eq!(response.status(), 200);
    let record = tokio::time::timeout(Duration::from_secs(30), usage.recv())
        .await
        .expect("the gateway gave up on the slow client")
        .unwrap();
    assert_eq!(record.outcome, "client_stalled");
    drop(response);
}
