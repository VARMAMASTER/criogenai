//! The gateway's second slice end to end (design 0006 §13.5): Anthropic
//! clients on `/v1/messages` (AgentBuild's own Anthropic client, so both
//! codecs run, plus raw HTTP for the wire details), and per-key rate
//! limits and budgets. Refill timing is unit-tested on the paused clock in
//! `src/limits.rs`.

use std::sync::Arc;
use std::time::Duration;

use agentbuild_gateway::{BudgetPeriod, Gateway, KeyLimits, UsageRecord};
use agentbuild_providers::{
    BlockDelta, ContentBlock, Endpoint, FakeModel, FakeReply, FinishReason, Message, Model,
    ModelErrorKind, ModelRequest, ProviderModel, ToolSpec, ToolStatus,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const KEY: &str = "team-a-test-key-0001";

fn fake(provider: &str) -> FakeModel {
    FakeModel::with_delay(Duration::ZERO, Duration::ZERO).named(provider, "fake")
}

/// Serves `gateway`; its base URL (`…/v1`).
async fn serve(gateway: &Gateway) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = gateway.router();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{address}/v1")
}

/// AgentBuild's Anthropic client, pointed at the gateway.
fn anthropic_client(base: &str, route: &str, key: Option<&str>) -> ProviderModel {
    ProviderModel::new(
        Endpoint::anthropic_compatible("gateway", base, key.map(str::to_string)),
        route,
    )
    .unwrap()
}

/// AgentBuild's OpenAI client, pointed at the gateway.
fn openai_client(base: &str, route: &str, key: Option<&str>) -> ProviderModel {
    ProviderModel::new(
        Endpoint::openai_compatible("gateway", base, key.map(str::to_string)),
        route,
    )
    .unwrap()
}

fn ask(text: &str) -> ModelRequest {
    ModelRequest::new(vec![Message::user(text)])
}

fn lookup() -> ToolSpec {
    ToolSpec::new(
        "lookup",
        "Looks up an order",
        json!({"type": "object", "properties": {"order": {"type": "string"}}}),
    )
}

#[tokio::test]
async fn an_anthropic_client_gets_answers_tools_and_streams() {
    let (sender, mut usage) = mpsc::channel(16);
    let upstream = fake("acme")
        .then(FakeReply::text("Hello from upstream"))
        .then(FakeReply::tool_call("lookup", json!({"order": "A-1"})))
        .then(FakeReply::text("It shipped."))
        .then(FakeReply::tool_call("lookup", json!({"order": "B-2"})))
        .then(FakeReply::text("Streamed hello"));
    let gateway = Gateway::builder()
        .route("support", Arc::new(upstream))
        .usage(sender)
        .build()
        .unwrap();
    let base = serve(&gateway).await;
    let model = anthropic_client(&base, "support", None);

    let answer = model.invoke(&ask("hi")).await.unwrap();
    assert_eq!(answer.message.text(), "Hello from upstream");
    assert_eq!(answer.finish_reason, FinishReason::Stop);
    assert!(answer.usage.input_tokens > 0 && answer.usage.output_tokens > 0);
    let record: UsageRecord = usage.recv().await.unwrap();
    assert_eq!(
        (record.route.as_str(), record.outcome.as_str()),
        ("support", "ok")
    );

    // A tool call, then its result going back as a tool_result block.
    let request = ask("where is A-1?").with_tools(vec![lookup()]);
    let answer = model.invoke(&request).await.unwrap();
    assert_eq!(answer.finish_reason, FinishReason::ToolCalls);
    let call = answer.message.tool_calls().next().unwrap();
    assert_eq!(call.name, "lookup");
    assert_eq!(call.args["order"], "A-1");
    let call_id = call.id.to_string();
    let mut messages = request.messages.clone();
    messages.push(answer.message.clone());
    messages.push(Message::tool_result(
        call_id,
        "lookup",
        ToolStatus::Success,
        vec![ContentBlock::text("shipped Monday")],
    ));
    let next = ModelRequest::new(messages).with_tools(vec![lookup()]);
    assert_eq!(
        model.invoke(&next).await.unwrap().message.text(),
        "It shipped."
    );

    // Streamed: a tool call, then text; the client's assembler rebuilds both.
    let mut deltas = 0;
    let answer = model.stream(&request, &mut |_| deltas += 1).await.unwrap();
    assert!(deltas > 0);
    assert_eq!(
        answer.message.tool_calls().next().unwrap().args["order"],
        "B-2"
    );
    let mut text = String::new();
    let answer = model
        .stream(&ask("hello"), &mut |d| {
            if let BlockDelta::Text { text: t } = &d.delta {
                text.push_str(t);
            }
        })
        .await
        .unwrap();
    assert_eq!(text, "Streamed hello");
    assert_eq!(answer.message.text(), "Streamed hello");
    assert!(
        answer.usage.output_tokens > 0,
        "usage arrives in message_delta"
    );
}

#[tokio::test]
async fn the_wire_format_is_anthropics() {
    let upstream = fake("acme")
        .then(FakeReply::text("Hi there"))
        .then(FakeReply::text("Hi again"));
    let gateway = Gateway::builder()
        .route("support", Arc::new(upstream))
        .key("team-a", KEY, ["support"])
        .build()
        .unwrap();
    let base = serve(&gateway).await;
    let http = reqwest::Client::new();
    let post = |body: Value| {
        http.post(format!("{base}/messages"))
            .header("x-api-key", KEY)
            .json(&body)
    };

    let body = json!({"model": "support", "max_tokens": 64, "system": "Be brief.",
        "messages": [{"role": "user", "content": "hi"}]});
    let reply: Value = post(body.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reply["type"], "message");
    assert_eq!(reply["role"], "assistant");
    assert_eq!(reply["model"], "support");
    assert!(reply["id"].as_str().unwrap().starts_with("msg_"));
    assert_eq!(
        reply["content"],
        json!([{"type": "text", "text": "Hi there"}])
    );
    assert_eq!(reply["stop_reason"], "end_turn");
    assert!(reply["usage"]["input_tokens"].as_u64().unwrap() > 0);

    // Streamed: named events in Anthropic's order.
    let mut streamed = body.clone();
    streamed["stream"] = json!(true);
    let response = post(streamed).send().await.unwrap();
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let text = response.text().await.unwrap();
    let names: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("event: "))
        .collect();
    assert_eq!(
        names,
        [
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop"
        ]
    );

    // Errors in Anthropic's shape.
    let missing: reqwest::Response = post(json!({"model": "support",
        "messages": [{"role": "user", "content": "hi"}]}))
    .send()
    .await
    .unwrap();
    assert_eq!(missing.status(), 400);
    let error: Value = missing.json().await.unwrap();
    assert_eq!(error["type"], "error");
    assert_eq!(error["error"]["type"], "invalid_request_error");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("max_tokens")
    );
    let unknown: Value = post(json!({"model": "nope", "max_tokens": 5,
        "messages": [{"role": "user", "content": "hi"}]}))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(unknown["error"]["type"], "not_found_error");
    let anonymous = http
        .post(format!("{base}/messages"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);
    let error: Value = anonymous.json().await.unwrap();
    assert_eq!(error["error"]["type"], "authentication_error");
}

#[tokio::test]
async fn an_upstream_failure_keeps_its_kind_and_falls_back() {
    let broken = fake("broken").failing_on("hi");
    let down = Arc::new(FakeModel::with_delay(Duration::ZERO, Duration::ZERO).flaky_first(9));
    let gateway = Gateway::builder()
        .route("strict", Arc::new(broken))
        .route_with_fallbacks(
            "spare",
            [
                down as Arc<dyn Model>,
                Arc::new(fake("backup").then(FakeReply::text("from the backup"))),
            ],
        )
        .build()
        .unwrap();
    let base = serve(&gateway).await;
    let error = anthropic_client(&base, "strict", None)
        .invoke(&ask("hi"))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ModelErrorKind::InvalidRequest);
    let answer = anthropic_client(&base, "spare", None)
        .stream(&ask("hello"), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(answer.message.text(), "from the backup");
}

#[tokio::test]
async fn rate_limits_refuse_with_retry_after() {
    let (sender, mut usage) = mpsc::channel(16);
    let upstream = fake("acme")
        .then(FakeReply::text("one"))
        .then(FakeReply::text("two"));
    let gateway = Gateway::builder()
        .route("support", Arc::new(upstream))
        .key("team-a", KEY, ["support"])
        .key_limits("team-a", KeyLimits::new().requests_per_minute(2))
        .usage(sender)
        .build()
        .unwrap();
    let base = serve(&gateway).await;
    let openai = openai_client(&base, "support", Some(KEY));
    let anthropic = anthropic_client(&base, "support", Some(KEY));
    assert_eq!(
        openai.invoke(&ask("a")).await.unwrap().message.text(),
        "one"
    );
    assert_eq!(
        anthropic.invoke(&ask("b")).await.unwrap().message.text(),
        "two"
    );

    // The third request within the minute: 429 with Retry-After, both formats.
    let http = reqwest::Client::new();
    let refused = http
        .post(format!("{base}/chat/completions"))
        .bearer_auth(KEY)
        .json(&json!({"model": "support", "messages": [{"role": "user", "content": "c"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 429);
    let wait: u64 = refused.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=30).contains(&wait), "{wait}");
    let body: Value = refused.json().await.unwrap();
    assert_eq!(body["error"]["code"], "key_rate_limited");
    let error = anthropic.invoke(&ask("d")).await.unwrap_err();
    assert_eq!(error.kind, ModelErrorKind::RateLimited);

    let outcomes: Vec<String> = [
        usage.recv().await.unwrap(),
        usage.recv().await.unwrap(),
        usage.recv().await.unwrap(),
    ]
    .into_iter()
    .map(|r| r.outcome)
    .collect();
    assert_eq!(outcomes, ["ok", "ok", "key_rate_limited"]);
}

#[tokio::test]
async fn budgets_refuse_the_request_after_the_one_that_spent_them() {
    let upstream = fake("acme").then(FakeReply::text("a long answer that spends the budget"));
    let gateway = Gateway::builder()
        .route("support", Arc::new(upstream))
        .key("team-a", KEY, ["support"])
        .key("team-b", "team-b-test-key-0002", ["support"])
        .key_limits("team-a", KeyLimits::new().budget(5, BudgetPeriod::Day))
        .build()
        .unwrap();
    let base = serve(&gateway).await;
    let model = openai_client(&base, "support", Some(KEY));
    model.invoke(&ask("spend it")).await.unwrap();
    let spent = gateway.spent("team-a").unwrap();
    assert!(
        spent > 5,
        "the request in flight may pass the budget: {spent}"
    );
    assert_eq!(gateway.spent("team-b"), None, "no limits");

    let http = reqwest::Client::new();
    let refused = http
        .post(format!("{base}/chat/completions"))
        .bearer_auth(KEY)
        .json(&json!({"model": "support", "messages": [{"role": "user", "content": "more"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 429);
    let wait: u64 = refused.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(wait <= 86_400, "until the end of the UTC day");
    let body: Value = refused.json().await.unwrap();
    assert_eq!(body["error"]["code"], "budget_exceeded");
    // Another key is not affected.
    let other = openai_client(&base, "support", Some("team-b-test-key-0002"));
    assert!(other.invoke(&ask("hi")).await.is_ok());

    // Limits for a key that does not exist are a configuration error.
    let err = Gateway::builder()
        .route("support", Arc::new(fake("acme")))
        .key_limits("ghost", KeyLimits::new().requests_per_minute(1))
        .build()
        .unwrap_err();
    assert!(err.to_string().contains("unknown key `ghost`"), "{err}");
}
