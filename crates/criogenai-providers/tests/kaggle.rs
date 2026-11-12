//! Kaggle's Model Proxy (design 0020) against two scripted local servers:
//! one plays Kaggle's API (minting proxy tokens), one the proxy. No network.

#![cfg(feature = "http")]

use criogenai_providers::{Dispatch, KaggleModel, Message, Model, ModelErrorKind, ModelRequest};

#[path = "support/server.rs"]
mod server;
use server::{Received, Step, json_response, serve_each, sse_data, sse_head};

const API_TOKEN: &str = "KGAT_test_api_token_123";

/// `http://127.0.0.1:<port>` (the helper returns `…/v1`).
fn root(base: &str) -> String {
    base.trim_end_matches("/v1").to_string()
}

fn mint(proxy_root: &str, token: &str, expiry: &str) -> Vec<Step> {
    vec![json_response(
        200,
        &format!(
            r#"{{"baseUri":"{proxy_root}/models","token":"{token}","expiryTime":"{expiry}"}}"#
        ),
    )]
}

fn chat_ok(text: &str) -> Vec<Step> {
    vec![json_response(
        200,
        &format!(
            r#"{{"id":"x","object":"chat.completion","model":"m","choices":[{{"index":0,"message":{{"role":"assistant","content":"{text}"}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}}}"#
        ),
    )]
}

fn request() -> ModelRequest {
    ModelRequest::new(vec![Message::user("hi")])
}

fn path(received: &Received) -> String {
    received.head.lines().next().unwrap_or("").to_string()
}

const LATER: &str = "2999-01-01T00:00:00.123456789Z";
const PAST: &str = "2020-01-01T00:00:00Z";

#[tokio::test]
async fn mints_once_and_reuses_the_token_on_the_proxy() {
    let (proxy, proxy_seen) = serve_each(vec![chat_ok("one"), chat_ok("two")]).await;
    let (api, api_seen) = serve_each(vec![mint(&root(&proxy), "proxy-token-A", LATER)]).await;
    let model = KaggleModel::new(API_TOKEN, "google/gemini-3.6-flash").with_api_base(root(&api));

    assert_eq!(
        model.invoke(&request()).await.unwrap().message.text(),
        "one"
    );
    assert_eq!(
        model.invoke(&request()).await.unwrap().message.text(),
        "two"
    );

    let api_seen = api_seen.await.unwrap();
    assert_eq!(api_seen.len(), 1, "one token for both calls");
    assert!(
        path(&api_seen[0]).contains("/v1/models.ModelProxyApiService/CreateDefaultModelProxyToken")
    );
    assert_eq!(
        api_seen[0].header("authorization").unwrap(),
        format!("Bearer {API_TOKEN}")
    );
    let proxy_seen = proxy_seen.await.unwrap();
    assert!(path(&proxy_seen[0]).contains("POST /models/openapi/chat/completions"));
    assert_eq!(
        proxy_seen[0].header("authorization").unwrap(),
        "Bearer proxy-token-A"
    );
    assert_eq!(proxy_seen[0].json()["model"], "google/gemini-3.6-flash");
    assert_eq!(model.provider(), "kaggle");
}

#[tokio::test]
async fn renews_a_token_that_is_due() {
    let (proxy, proxy_seen) = serve_each(vec![chat_ok("one"), chat_ok("two")]).await;
    let (api, api_seen) = serve_each(vec![
        mint(&root(&proxy), "proxy-token-A", PAST),
        mint(&root(&proxy), "proxy-token-B", LATER),
    ])
    .await;
    let model = KaggleModel::new(API_TOKEN, "m").with_api_base(root(&api));

    model.invoke(&request()).await.unwrap();
    model.invoke(&request()).await.unwrap();

    assert_eq!(
        api_seen.await.unwrap().len(),
        2,
        "the expired token was renewed"
    );
    let proxy_seen = proxy_seen.await.unwrap();
    assert_eq!(
        proxy_seen[1].header("authorization").unwrap(),
        "Bearer proxy-token-B"
    );
}

#[tokio::test]
async fn a_refused_token_is_renewed_and_the_call_retried_once() {
    let (proxy, proxy_seen) = serve_each(vec![
        vec![json_response(
            401,
            r#"{"error":{"message":"token expired"}}"#,
        )],
        chat_ok("after renewal"),
    ])
    .await;
    let (api, _) = serve_each(vec![
        mint(&root(&proxy), "proxy-token-A", LATER),
        mint(&root(&proxy), "proxy-token-B", LATER),
    ])
    .await;
    let model = KaggleModel::new(API_TOKEN, "m").with_api_base(root(&api));

    let reply = model.invoke(&request()).await.unwrap();
    assert_eq!(reply.message.text(), "after renewal");
    let proxy_seen = proxy_seen.await.unwrap();
    assert_eq!(
        proxy_seen[0].header("authorization").unwrap(),
        "Bearer proxy-token-A"
    );
    assert_eq!(
        proxy_seen[1].header("authorization").unwrap(),
        "Bearer proxy-token-B"
    );
}

#[tokio::test]
async fn streams_through_the_proxy() {
    let (proxy, _) = serve_each(vec![vec![
        sse_head(),
        sse_data(r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"Hel"}}]}"#),
        sse_data(r#"{"choices":[{"index":0,"delta":{"content":"lo"},"finish_reason":"stop"}]}"#),
        Step::send("data: [DONE]\n\n"),
    ]])
    .await;
    let (api, _) = serve_each(vec![mint(&root(&proxy), "proxy-token-A", LATER)]).await;
    let model = KaggleModel::new(API_TOKEN, "m").with_api_base(root(&api));

    let mut deltas = 0;
    let reply = model
        .stream(&request(), &mut |_| deltas += 1)
        .await
        .unwrap();
    assert_eq!(reply.message.text(), "Hello");
    assert!(deltas >= 2);
}

#[tokio::test]
async fn a_pre_minted_proxy_token_is_used_as_is() {
    let (proxy, proxy_seen) = serve_each(vec![chat_ok("ok")]).await;
    // The `/openapi` suffix (as Kaggle notebooks set MODEL_PROXY_URL) is accepted too.
    let model = KaggleModel::from_proxy(
        format!("{}/models/openapi", root(&proxy)),
        "notebook-token",
        "m",
    )
    .unwrap()
    .with_api_base("http://127.0.0.1:9"); // never contacted
    assert_eq!(model.invoke(&request()).await.unwrap().message.text(), "ok");
    let seen = proxy_seen.await.unwrap();
    assert!(path(&seen[0]).contains("/models/openapi/chat/completions"));
    assert_eq!(
        seen[0].header("authorization").unwrap(),
        "Bearer notebook-token"
    );
}

#[tokio::test]
async fn tokens_never_appear_in_debug_or_errors() {
    let (api, _) = serve_each(vec![vec![json_response(
        401,
        &format!(r#"{{"message":"bad token {API_TOKEN}"}}"#),
    )]])
    .await;
    let model = KaggleModel::new(API_TOKEN, "m").with_api_base(root(&api));
    assert!(!format!("{model:?}").contains(API_TOKEN));
    let error = model.invoke(&request()).await.unwrap_err();
    assert_eq!(error.kind, ModelErrorKind::Auth);
    assert!(!error.to_string().contains(API_TOKEN));
    assert!(!error.message.contains(API_TOKEN));

    let proxy =
        KaggleModel::from_proxy("https://example.invalid/models", "secret-proxy-token", "m")
            .unwrap();
    assert!(!format!("{proxy:?}").contains("secret-proxy-token"));
}

#[tokio::test]
async fn a_reply_without_a_token_is_an_invalid_response() {
    let (api, _) = serve_each(vec![vec![json_response(
        200,
        r#"{"baseUri":"","token":""}"#,
    )]])
    .await;
    let model = KaggleModel::new(API_TOKEN, "m").with_api_base(root(&api));
    let error = model.invoke(&request()).await.unwrap_err();
    assert_eq!(error.kind, ModelErrorKind::InvalidResponse);
}

#[tokio::test]
async fn an_account_without_the_proxy_gets_a_clear_404() {
    let (api, _) = serve_each(vec![vec![json_response(404, r#"{"message":"not found"}"#)]]).await;
    let model = KaggleModel::new(API_TOKEN, "m").with_api_base(root(&api));
    let error = model.invoke(&request()).await.unwrap_err();
    assert_eq!(error.kind, ModelErrorKind::NotFound);
    assert!(
        error.message.contains("not available to this account"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn the_api_token_is_not_sent_over_plain_http_to_another_machine() {
    let model = KaggleModel::new(API_TOKEN, "m").with_api_base("http://kaggle.example.com");
    let error = model.invoke(&request()).await.unwrap_err();
    assert_eq!(error.kind, ModelErrorKind::InvalidRequest);
    assert_eq!(error.dispatch, Dispatch::NotSent);
}
