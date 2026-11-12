//! Named providers (design 0006 §4.1) end to end against a scripted local
//! server: the path, the key header, the provider name in replies and
//! errors, and each server's quirks in the request body. Never the network.

#![cfg(feature = "http")]

#[path = "support/server.rs"]
mod server;

use std::time::Duration;

use criogenai_providers::{
    ContentBlock, HttpConfig, Message, Model, ModelErrorKind, ModelRequest, PRESETS, ProviderModel,
    Role, ToolStatus, preset, preview_request,
};
use serde_json::{Map, Value, json};
use server::{json_response, serve};

fn model(name: &str, base: &str) -> ProviderModel {
    let endpoint = preset(name)
        .unwrap()
        .endpoint(Some("sk-test".into()))
        .unwrap()
        .with_base_url(base);
    let config = HttpConfig::default()
        .connect_timeout(Duration::from_secs(2))
        .idle_timeout(Duration::from_secs(2))
        .total_timeout(Duration::from_secs(5));
    ProviderModel::with_config(endpoint, "test-model", config).unwrap()
}

const REPLY: &str = r#"{"choices": [{"message": {"role": "assistant", "content": "Hi"},
    "finish_reason": "stop"}], "usage": {"prompt_tokens": 3, "completion_tokens": 1}}"#;

#[tokio::test]
async fn every_openai_style_preset_calls_chat_completions_with_its_key() {
    // `openai` speaks the Responses API: tests/openai_responses.rs.
    for p in PRESETS
        .iter()
        .filter(|p| !p.anthropic && p.key_env.is_some() && p.name != "openai")
    {
        let (base, server) = serve(vec![json_response(200, REPLY)]).await;
        let response = model(p.name, &base)
            .invoke(&ModelRequest::new(vec![Message::user("hi")]))
            .await
            .unwrap();
        assert_eq!(response.message.text(), "Hi", "{}", p.name);
        assert_eq!(response.message.metadata.provider.as_deref(), Some(p.name));
        let received = server.await.unwrap();
        assert!(
            received.head.starts_with("POST /v1/chat/completions"),
            "{}: {}",
            p.name,
            received.head
        );
        assert_eq!(
            received.header("authorization").as_deref(),
            Some("Bearer sk-test")
        );
    }
}

#[tokio::test]
async fn errors_name_the_preset_and_hide_the_key() {
    let (base, _server) = serve(vec![json_response(
        401,
        r#"{"error": {"message": "bad key sk-test"}}"#,
    )])
    .await;
    let err = model("groq", &base)
        .invoke(&ModelRequest::new(vec![Message::user("hi")]))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::Auth);
    assert_eq!(err.provider.as_deref(), Some("groq"));
    assert!(!err.to_string().contains("sk-test"), "{err}");
}

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

/// A turn where the model thought, then called a tool.
fn thought_then_tool_call() -> ModelRequest {
    let assistant = Message::with_content(
        Role::Assistant,
        vec![
            ContentBlock::reasoning("The user wants the weather."),
            ContentBlock::tool_call("call_1", "weather", args(json!({"city": "Pune"}))),
        ],
    );
    ModelRequest::new(vec![
        Message::user("Weather in Pune?"),
        assistant,
        Message::tool_result(
            "call_1",
            "weather",
            ToolStatus::Success,
            vec![ContentBlock::text("31 C")],
        ),
    ])
}

fn body(name: &str, request: &ModelRequest, stream: bool) -> Value {
    let endpoint = preset(name).unwrap().endpoint(Some("k".into())).unwrap();
    preview_request(&endpoint, "m", request, stream).unwrap().0
}

#[test]
fn thinking_servers_get_their_reasoning_back_with_tool_calls() {
    for name in ["deepseek", "moonshot"] {
        let sent = body(name, &thought_then_tool_call(), false);
        assert_eq!(
            sent["messages"][1]["reasoning_content"], "The user wants the weather.",
            "{name}"
        );
    }
    // Others never see the field (reasoning is dropped with a warning).
    let sent = body("groq", &thought_then_tool_call(), false);
    assert!(sent["messages"][1].get("reasoning_content").is_none());
}

#[test]
fn strict_servers_get_no_stream_options() {
    let request = ModelRequest::new(vec![Message::user("hi")]);
    for name in ["mistral", "cerebras"] {
        assert!(
            body(name, &request, true).get("stream_options").is_none(),
            "{name}"
        );
    }
    assert_eq!(
        body("deepseek", &request, true)["stream_options"],
        json!({"include_usage": true})
    );
}
