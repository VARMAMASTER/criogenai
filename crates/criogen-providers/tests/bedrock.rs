//! Amazon Bedrock with an API key (design 0006 §18) against a scripted
//! local server: Claude over the Messages API with `x-api-key`, other
//! models over Chat Completions with a Bearer key, the key kept out of
//! errors, and no default region. Never the network.

#![cfg(feature = "http")]

#[path = "support/server.rs"]
mod server;

use std::time::Duration;

use criogen_providers::{
    BedrockApi, HttpConfig, Message, Model, ModelErrorKind, ModelRequest, ProviderModel,
    bedrock_endpoint,
};
use server::{json_response, serve};

fn model(base: &str, id: &str, key: &str) -> ProviderModel {
    let endpoint = bedrock_endpoint("us-east-1", id, key, BedrockApi::Auto)
        .unwrap()
        .allow_insecure_http()
        .with_base_url(base);
    let config = HttpConfig::default()
        .connect_timeout(Duration::from_secs(2))
        .idle_timeout(Duration::from_secs(2))
        .total_timeout(Duration::from_secs(5));
    ProviderModel::with_config(endpoint, id, config).unwrap()
}

fn hi() -> ModelRequest {
    ModelRequest::new(vec![Message::user("hi")])
}

#[tokio::test]
async fn claude_goes_over_the_messages_api_with_x_api_key() {
    let (base, server) = serve(vec![json_response(
        200,
        r#"{"id": "msg_1", "type": "message", "role": "assistant", "model": "claude",
            "content": [{"type": "text", "text": "Hi from Bedrock"}],
            "stop_reason": "end_turn", "usage": {"input_tokens": 3, "output_tokens": 2}}"#,
    )])
    .await;
    let response = model(&base, "us.anthropic.claude-sonnet-5", "bedrock-key-123")
        .invoke(&hi())
        .await
        .unwrap();
    assert_eq!(response.message.text(), "Hi from Bedrock");
    assert_eq!(
        response.message.metadata.provider.as_deref(),
        Some("bedrock")
    );
    let received = server.await.unwrap();
    assert!(
        received.head.starts_with("POST /v1/messages"),
        "{}",
        received.head
    );
    assert_eq!(
        received.header("x-api-key").as_deref(),
        Some("bedrock-key-123")
    );
    assert_eq!(
        received.header("anthropic-version").as_deref(),
        Some("2023-06-01")
    );
    assert_eq!(received.json()["model"], "us.anthropic.claude-sonnet-5");
}

#[tokio::test]
async fn other_models_go_over_chat_completions_with_a_bearer_key() {
    let (base, server) = serve(vec![json_response(
        200,
        r#"{"choices": [{"message": {"role": "assistant", "content": "Hi from gpt-oss"},
            "finish_reason": "stop"}]}"#,
    )])
    .await;
    let response = model(&base, "openai.gpt-oss-120b-1:0", "bedrock-key-123")
        .invoke(&hi())
        .await
        .unwrap();
    assert_eq!(response.message.text(), "Hi from gpt-oss");
    let received = server.await.unwrap();
    assert!(received.head.starts_with("POST /v1/chat/completions"));
    assert_eq!(
        received.header("authorization").as_deref(),
        Some("Bearer bedrock-key-123")
    );
}

#[tokio::test]
async fn errors_hide_the_key() {
    let (base, _server) = serve(vec![json_response(
        403,
        r#"{"message": "The security token bedrock-key-secret is invalid"}"#,
    )])
    .await;
    let err = model(&base, "openai.gpt-oss-120b-1:0", "bedrock-key-secret")
        .invoke(&hi())
        .await
        .unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::Permission);
    assert_eq!(err.provider.as_deref(), Some("bedrock"));
    assert!(!err.to_string().contains("bedrock-key-secret"), "{err}");
}

#[test]
fn a_missing_region_or_key_names_the_variable() {
    // Only explicit values: the process environment is shared by tests.
    let err = criogen_providers::bedrock_from_env("m", Some("us-east-1"), None, BedrockApi::Auto);
    if std::env::var("AWS_BEARER_TOKEN_BEDROCK").is_err() {
        let err = err.unwrap_err();
        assert!(err.message.contains("AWS_BEARER_TOKEN_BEDROCK"), "{err}");
    }
    if std::env::var("AWS_REGION").is_err() && std::env::var("AWS_DEFAULT_REGION").is_err() {
        let err =
            criogen_providers::bedrock_from_env("m", None, Some("k".into()), BedrockApi::Auto)
                .unwrap_err();
        assert!(err.message.contains("AWS_REGION"), "{err}");
    }
}
