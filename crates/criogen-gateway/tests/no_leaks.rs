//! What the gateway records never contains content or secrets (design 0006
//! §12.5). LiteLLM's October 2026 breach began with developers' login tokens
//! found in logged response bodies: this test fails if a prompt, an answer,
//! or a key ever reaches a usage record or the gateway's debug output.

use std::sync::Arc;
use std::time::Duration;

use criogen_gateway::Gateway;
use criogen_providers::{
    Endpoint, FakeModel, FakeReply, Message, Model, ModelRequest, ProviderModel,
};
use serde_json::json;
use tokio::sync::mpsc;

const KEY: &str = "SECRET-KEY-team-a-0001";
const PROMPT: &str = "SECRET-PROMPT-patient-name-Jane-Doe";
const ANSWER: &str = "SECRET-ANSWER-diagnosis";

#[tokio::test]
async fn records_and_debug_output_carry_no_content_or_keys() {
    let (sender, mut usage) = mpsc::channel(16);
    let upstream = FakeModel::with_delay(Duration::ZERO, Duration::ZERO)
        .then(FakeReply::text(ANSWER))
        .then(FakeReply::text(ANSWER))
        .then(FakeReply::text(ANSWER));
    let gateway = Gateway::builder()
        .route("support", Arc::new(upstream))
        .key("team-a", KEY, ["support"])
        .usage(sender)
        .build()
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let router = gateway.router();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    // Both wire formats, streamed and not, plus a refused request.
    let openai = ProviderModel::new(
        Endpoint::openai_compatible("gateway", &base, Some(KEY.into())),
        "support",
    )
    .unwrap();
    let anthropic = ProviderModel::new(
        Endpoint::anthropic_compatible("gateway", &base, Some(KEY.into())),
        "support",
    )
    .unwrap();
    let request = ModelRequest::new(vec![Message::user(PROMPT)]);
    openai.invoke(&request).await.unwrap();
    anthropic.stream(&request, &mut |_| {}).await.unwrap();
    openai.stream(&request, &mut |_| {}).await.unwrap();
    let wrong = reqwest::Client::new()
        .post(format!("{base}/chat/completions"))
        .bearer_auth("SECRET-WRONG-KEY-0000")
        .json(&json!({"model": "support", "messages": [{"role": "user", "content": PROMPT}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);
    let unknown = reqwest::Client::new()
        .post(format!("{base}/chat/completions"))
        .bearer_auth(KEY)
        .json(&json!({"model": "no-such-route", "messages": [{"role": "user", "content": PROMPT}]}))
        .send()
        .await
        .unwrap();
    let error = unknown.text().await.unwrap();
    assert!(!error.contains(PROMPT) && !error.contains(KEY), "{error}");

    let mut records = Vec::new();
    for _ in 0..3 {
        records.push(serde_json::to_string(&usage.recv().await.unwrap()).unwrap());
    }
    for record in records
        .iter()
        .map(String::as_str)
        .chain([format!("{gateway:?}").as_str()])
    {
        for secret in [PROMPT, ANSWER, KEY, "SECRET-WRONG-KEY-0000"] {
            assert!(!record.contains(secret), "`{secret}` leaked into: {record}");
        }
    }
    // Only these fields, ever (a new field needs a decision here).
    let value: serde_json::Value = serde_json::from_str(&records[0]).unwrap();
    let mut fields: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "fallbacks",
            "input_tokens",
            "key",
            "latency_ms",
            "model",
            "outcome",
            "output_tokens",
            "route",
            "stream"
        ]
    );
}
