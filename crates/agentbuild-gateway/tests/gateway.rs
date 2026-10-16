//! The gateway end to end (design 0006 §12.7): a server on 127.0.0.1:0
//! with simulated upstream models, called with AgentBuild's own OpenAI
//! client (both codecs) and with raw HTTP (the wire details).

use std::sync::Arc;
use std::time::Duration;

use agentbuild_gateway::{Gateway, UsageRecord};
use agentbuild_providers::{
    Endpoint, FakeModel, FakeReply, FinishReason, Message, Model, ModelErrorKind, ModelRequest,
    ProviderModel, ToolSpec,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const KEY: &str = "team-a-test-key-0001";

fn fake(provider: &str) -> FakeModel {
    FakeModel::with_delay(Duration::ZERO, Duration::ZERO).named(provider, "fake")
}

/// Serves `gateway`; its OpenAI base URL.
async fn serve(gateway: &Gateway) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = gateway.router();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{address}/v1")
}

/// AgentBuild's OpenAI client, pointed at the gateway.
fn client(base: &str, route: &str, key: Option<&str>) -> ProviderModel {
    ProviderModel::new(
        Endpoint::openai_compatible("gateway", base, key.map(str::to_string)),
        route,
    )
    .unwrap()
}

fn ask(text: &str) -> ModelRequest {
    ModelRequest::new(vec![Message::user(text)])
}

#[tokio::test]
async fn an_openai_client_gets_answers_and_tool_calls_streamed_or_not() {
    let (sender, mut usage) = mpsc::channel(16);
    let upstream = fake("acme")
        .then(FakeReply::text("Hello from upstream"))
        .then(FakeReply::tool_call("lookup", json!({"order": "A-1"})))
        .then(FakeReply::text("Streamed hello"));
    let gateway = Gateway::builder()
        .route("support", Arc::new(upstream))
        .usage(sender)
        .build()
        .unwrap();
    let base = serve(&gateway).await;
    let model = client(&base, "support", None);

    let answer = model.invoke(&ask("hi")).await.unwrap();
    assert_eq!(answer.message.text(), "Hello from upstream");
    assert_eq!(answer.finish_reason, FinishReason::Stop);
    assert!(answer.usage.input_tokens > 0);
    let record: UsageRecord = usage.recv().await.unwrap();
    assert_eq!(record.route, "support");
    assert_eq!(record.model, "acme/fake");
    assert_eq!(record.outcome, "ok");
    assert!(!record.stream);
    assert_eq!(record.key, None);

    // A tool call, streamed: the client's assembler rebuilds it.
    let request = ask("where is A-1?").with_tools(vec![ToolSpec::new(
        "lookup",
        "Looks up an order",
        json!({"type": "object", "properties": {"order": {"type": "string"}}}),
    )]);
    let mut deltas = 0;
    let answer = model.stream(&request, &mut |_| deltas += 1).await.unwrap();
    assert!(deltas > 0);
    assert_eq!(answer.finish_reason, FinishReason::ToolCalls);
    let call = answer.message.tool_calls().next().unwrap();
    assert_eq!(call.name, "lookup");
    assert_eq!(call.args["order"], "A-1");
    assert!(usage.recv().await.unwrap().stream);

    // Streamed text.
    let mut text = String::new();
    let answer = model
        .stream(&ask("hello"), &mut |d| {
            if let agentbuild_providers::BlockDelta::Text { text: t } = &d.delta {
                text.push_str(t);
            }
        })
        .await
        .unwrap();
    assert_eq!(text, "Streamed hello");
    assert_eq!(answer.message.text(), "Streamed hello");
}

#[tokio::test]
async fn the_wire_format_is_openais() {
    let upstream = fake("acme").then(FakeReply::text("Hi there"));
    let gateway = Gateway::builder()
        .route("support", Arc::new(upstream))
        .build()
        .unwrap();
    let base = serve(&gateway).await;
    let http = reqwest::Client::new();
    let response = http
        .post(format!("{base}/chat/completions"))
        .json(&json!({
            "model": "support",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
            "stream_options": {"include_usage": true},
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let body = response.text().await.unwrap();
    let events: Vec<&str> = body
        .split("\n\n")
        .filter(|e| !e.is_empty())
        .map(|e| e.strip_prefix("data: ").expect("SSE data lines"))
        .collect();
    assert_eq!(events.last(), Some(&"[DONE]"));
    let chunks: Vec<Value> = events[..events.len() - 1]
        .iter()
        .map(|e| serde_json::from_str(e).unwrap())
        .collect();
    assert!(
        chunks
            .iter()
            .all(|c| c["object"] == "chat.completion.chunk")
    );
    assert_eq!(chunks[0]["choices"][0]["delta"]["role"], "assistant");
    let text: String = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(text, "Hi there");
    let finish = &chunks[chunks.len() - 2];
    assert_eq!(finish["choices"][0]["finish_reason"], "stop");
    let usage = chunks.last().unwrap();
    assert_eq!(usage["choices"], json!([]));
    assert!(usage["usage"]["total_tokens"].as_u64().unwrap() > 0);

    // Errors: OpenAI's shape and status codes.
    let bad = http
        .post(format!("{base}/chat/completions"))
        .body("not json")
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    let body: Value = bad.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_request");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    let missing = http
        .post(format!("{base}/chat/completions"))
        .json(&json!({"model": "nope", "messages": [{"role": "user", "content": "x"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
    assert_eq!(
        http.get(format!("{base}/models"))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["data"][0]["id"],
        "support"
    );
}

#[tokio::test]
async fn upstream_errors_keep_their_kind_and_routes_fall_back() {
    let (sender, mut usage) = mpsc::channel(16);
    let gateway = Gateway::builder()
        .route("limited", Arc::new(fake("busy").flaky_first(10)))
        .route("refused", Arc::new(fake("strict").failing_on("FORBIDDEN")))
        .route_with_fallbacks(
            "resilient",
            [
                Arc::new(fake("busy").flaky_first(10)) as Arc<dyn Model>,
                Arc::new(fake("backup").then(FakeReply::text("from the backup"))),
            ],
        )
        .usage(sender)
        .build()
        .unwrap();
    let base = serve(&gateway).await;

    let err = client(&base, "limited", None)
        .invoke(&ask("hi"))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::RateLimited);
    assert_eq!(err.status, Some(429));
    assert_eq!(usage.recv().await.unwrap().outcome, "rate_limited");

    let err = client(&base, "refused", None)
        .invoke(&ask("FORBIDDEN"))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::InvalidRequest);
    usage.recv().await.unwrap();

    let answer = client(&base, "resilient", None)
        .invoke(&ask("hi"))
        .await
        .unwrap();
    assert_eq!(answer.message.text(), "from the backup");
    let record = usage.recv().await.unwrap();
    assert_eq!(record.model, "backup/fake");
    assert_eq!(record.fallbacks, 1);

    // Streams fall back too, before anything was sent.
    let gateway = Gateway::builder()
        .route_with_fallbacks(
            "resilient",
            [
                Arc::new(fake("busy").flaky_first(10)) as Arc<dyn Model>,
                Arc::new(fake("backup").then(FakeReply::text("streamed backup"))),
            ],
        )
        .build()
        .unwrap();
    let base = serve(&gateway).await;
    let answer = client(&base, "resilient", None)
        .stream(&ask("hi"), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(answer.message.text(), "streamed backup");
}

#[tokio::test]
async fn keys_decide_who_may_use_which_route() {
    let gateway = Gateway::builder()
        .route("support", Arc::new(fake("a").then(FakeReply::text("ok"))))
        .route("internal", Arc::new(fake("b")))
        .key("team-a", KEY, ["support"])
        .build()
        .unwrap();
    let base = serve(&gateway).await;
    let http = reqwest::Client::new();
    let post = |route: &str, key: Option<&str>| {
        let mut request = http
            .post(format!("{base}/chat/completions"))
            .json(&json!({"model": route, "messages": [{"role": "user", "content": "hi"}]}));
        if let Some(key) = key {
            request = request.bearer_auth(key);
        }
        request.send()
    };
    let none = post("support", None).await.unwrap();
    assert_eq!(none.status(), 401);
    assert_eq!(
        none.json::<Value>().await.unwrap()["error"]["code"],
        "invalid_api_key"
    );
    assert_eq!(
        post("support", Some("wrong-key-wrong-key"))
            .await
            .unwrap()
            .status(),
        401
    );
    // A route the key may not use looks like no route at all.
    assert_eq!(post("internal", Some(KEY)).await.unwrap().status(), 404);
    assert_eq!(post("support", Some(KEY)).await.unwrap().status(), 200);
    let models: Value = http
        .get(format!("{base}/models"))
        .bearer_auth(KEY)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(models["data"].as_array().unwrap().len(), 1);
    let health = http
        .get(base.replace("/v1", "/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(health.text().await.unwrap(), "ok");

    // Through the client: a missing key is classified as auth.
    let err = client(&base, "support", None)
        .invoke(&ask("hi"))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::Auth);

    for (key, why) in [("short", "shorter than 16"), (KEY, "unknown route")] {
        let routes = if why == "unknown route" {
            "nope"
        } else {
            "support"
        };
        let err = Gateway::builder()
            .route("support", Arc::new(fake("a")))
            .key("k", key, [routes])
            .build()
            .unwrap_err()
            .to_string();
        assert!(err.contains(why), "{err}");
    }
    assert!(Gateway::builder().build().is_err());
}

#[tokio::test]
async fn usage_records_never_slow_requests_and_a_gone_client_stops_the_upstream() {
    // A channel nobody reads: records beyond its room are dropped.
    let (sender, mut usage) = mpsc::channel(1);
    let gateway = Gateway::builder()
        .route("support", Arc::new(fake("a")))
        .usage(sender)
        .build()
        .unwrap();
    let base = serve(&gateway).await;
    let model = client(&base, "support", None);
    for _ in 0..3 {
        model.invoke(&ask("hi")).await.unwrap();
    }
    assert_eq!(gateway.dropped_usage_records(), 2);
    usage.recv().await.unwrap();

    // The client leaves mid-stream: the upstream call is aborted, so it
    // never finishes (and is never recorded).
    let slow = Arc::new(
        FakeModel::with_delay(Duration::from_secs(1), Duration::from_secs(1))
            .then(FakeReply::text("too late")),
    );
    let (sender, mut usage) = mpsc::channel(4);
    let gateway = Gateway::builder()
        .route("slow", slow.clone())
        .usage(sender)
        .build()
        .unwrap();
    let base = serve(&gateway).await;
    let response = reqwest::Client::new()
        .post(format!("{base}/chat/completions"))
        .json(&json!({"model": "slow", "stream": true, "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    while slow.calls() == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    drop(response);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        usage.try_recv().is_err(),
        "the upstream call ran to the end"
    );
}
