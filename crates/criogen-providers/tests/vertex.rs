//! Google Vertex AI (design 0006 §19) against scripted local servers:
//! Claude's rawPredict path and body, the OpenAI-compatible path for
//! Gemini, tokens from a refresh token and from the metadata server,
//! credentials files, refused names, no secret in errors. Never the network.

#![cfg(feature = "http")]

#[path = "support/server.rs"]
mod server;

use std::time::Duration;

use criogen_providers::{
    HttpConfig, Message, MessageDelta, Model, ModelErrorKind, ModelRequest, VertexApi,
    VertexCredential, VertexModel,
};
use server::{Step, json_response, serve, serve_each, sse_head};

const CLAUDE: &str = r#"{"id": "msg_1", "type": "message", "role": "assistant",
    "model": "claude-sonnet-5", "content": [{"type": "text", "text": "Hi from Vertex"}],
    "stop_reason": "end_turn", "usage": {"input_tokens": 3, "output_tokens": 2}}"#;
const CHAT: &str = r#"{"choices": [{"message": {"role": "assistant", "content": "Hi from Gemini"},
    "finish_reason": "stop"}]}"#;

fn config() -> HttpConfig {
    HttpConfig::default()
        .connect_timeout(Duration::from_secs(2))
        .idle_timeout(Duration::from_secs(2))
        .total_timeout(Duration::from_secs(5))
}

fn origin(base: &str) -> String {
    base.trim_end_matches("/v1").to_string()
}

fn hi() -> ModelRequest {
    ModelRequest::new(vec![Message::user("hi")])
}

fn model(base: &str, location: &str, name: &str, credential: VertexCredential) -> VertexModel {
    VertexModel::new("my-project", location, name, credential)
        .unwrap()
        .with_host(origin(base))
        .with_config(config())
}

#[tokio::test]
async fn claude_goes_to_raw_predict_with_the_version_in_the_body() {
    let (base, server) = serve(vec![json_response(200, CLAUDE)]).await;
    let m = model(
        &base,
        "global",
        "claude-sonnet-5",
        VertexCredential::token("ya29.token"),
    );
    let response = m.invoke(&hi()).await.unwrap();
    assert_eq!(response.message.text(), "Hi from Vertex");
    assert_eq!(
        response.message.metadata.provider.as_deref(),
        Some("vertex")
    );
    let received = server.await.unwrap();
    assert!(
        received.head.starts_with(
            "POST /v1/projects/my-project/locations/global/publishers/anthropic/models/claude-sonnet-5:rawPredict"
        ),
        "{}",
        received.head
    );
    assert_eq!(
        received.header("authorization").as_deref(),
        Some("Bearer ya29.token")
    );
    assert_eq!(received.header("x-api-key"), None);
    let body = received.json();
    assert_eq!(body["anthropic_version"], "vertex-2023-10-16");
    assert!(body.get("model").is_none(), "{body}");
}

#[tokio::test]
async fn claude_streams_from_stream_raw_predict() {
    let events = [
        r#"{"type": "message_start", "message": {"id": "m", "type": "message", "role": "assistant", "model": "c", "content": [], "usage": {"input_tokens": 1, "output_tokens": 0}}}"#,
        r#"{"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}"#,
        r#"{"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "streamed"}}"#,
        r#"{"type": "content_block_stop", "index": 0}"#,
        r#"{"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}}"#,
        r#"{"type": "message_stop"}"#,
    ];
    let mut script = vec![sse_head()];
    script.extend(events.iter().map(|e| Step::send(format!("data: {e}\n\n"))));
    let (base, server) = serve(script).await;
    let m = model(
        &base,
        "us-east5",
        "claude-sonnet-5",
        VertexCredential::token("t"),
    );
    let mut deltas = Vec::new();
    let mut sink = |d: MessageDelta| deltas.push(d);
    let response = m.stream(&hi(), &mut sink).await.unwrap();
    assert_eq!(response.message.text(), "streamed");
    assert!(!deltas.is_empty());
    let received = server.await.unwrap();
    assert!(
        received.head.contains(
            "/locations/us-east5/publishers/anthropic/models/claude-sonnet-5:streamRawPredict"
        ),
        "{}",
        received.head
    );
}

#[tokio::test]
async fn gemini_goes_to_the_openai_compatible_endpoint() {
    let (base, server) = serve(vec![json_response(200, CHAT)]).await;
    let m = model(
        &base,
        "europe-west4",
        "google/gemini-3.6-flash",
        VertexCredential::token("t"),
    );
    assert_eq!(
        m.invoke(&hi()).await.unwrap().message.text(),
        "Hi from Gemini"
    );
    let received = server.await.unwrap();
    assert!(
        received.head.starts_with(
            "POST /v1/projects/my-project/locations/europe-west4/endpoints/openapi/chat/completions"
        ),
        "{}",
        received.head
    );
    assert_eq!(received.json()["model"], "google/gemini-3.6-flash");
    assert_eq!(
        received.header("authorization").as_deref(),
        Some("Bearer t")
    );
}

#[tokio::test]
async fn a_refresh_token_buys_access_tokens() {
    let (base, server) = serve_each(vec![
        vec![json_response(
            200,
            r#"{"access_token": "ya29.fresh", "expires_in": 3599}"#,
        )],
        vec![json_response(200, CHAT)],
    ])
    .await;
    let credential =
        VertexCredential::authorized_user("client.apps", "client-secret", "1//refresh-xyz");
    let m = model(&base, "global", "google/gemini-3.6-flash", credential)
        .with_token_url(format!("{}/token", origin(&base)));
    m.invoke(&hi()).await.unwrap();
    let [token, call] = <[_; 2]>::try_from(server.await.unwrap()).ok().unwrap();
    assert!(token.head.starts_with("POST /token"));
    let form = String::from_utf8(token.body).unwrap();
    assert!(form.contains("grant_type=refresh_token"), "{form}");
    assert!(form.contains("refresh_token=1%2F%2Frefresh-xyz"), "{form}");
    assert_eq!(
        call.header("authorization").as_deref(),
        Some("Bearer ya29.fresh")
    );
}

#[tokio::test]
async fn the_metadata_server_gives_tokens_on_google_cloud() {
    let (base, server) = serve_each(vec![
        vec![json_response(
            200,
            r#"{"access_token": "ya29.meta", "expires_in": 3599, "token_type": "Bearer"}"#,
        )],
        vec![json_response(200, CHAT)],
    ])
    .await;
    let m = model(
        &base,
        "global",
        "google/gemini-3.6-flash",
        VertexCredential::metadata_server(),
    )
    .with_metadata_url(format!("{}/computeMetadata/v1/token", origin(&base)));
    m.invoke(&hi()).await.unwrap();
    let [token, call] = <[_; 2]>::try_from(server.await.unwrap()).ok().unwrap();
    assert!(
        token.head.starts_with("GET /computeMetadata/v1/token"),
        "{}",
        token.head
    );
    assert_eq!(token.header("metadata-flavor").as_deref(), Some("Google"));
    assert_eq!(
        call.header("authorization").as_deref(),
        Some("Bearer ya29.meta")
    );
}

#[tokio::test]
async fn token_failures_hide_the_secrets() {
    let (base, _server) = serve(vec![json_response(
        400,
        r#"{"error": "invalid_grant", "error_description": "Bad Request 1//refresh-secret"}"#,
    )])
    .await;
    let credential =
        VertexCredential::authorized_user("c", "client-secret-value", "1//refresh-secret");
    let m = model(&base, "global", "google/gemini-3.6-flash", credential)
        .with_token_url(format!("{}/token", origin(&base)));
    let err = m.invoke(&hi()).await.unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::Auth);
    assert!(!err.to_string().contains("1//refresh-secret"), "{err}");
    assert!(!format!("{m:?}").contains("client-secret-value"));
}

#[test]
fn credentials_files_and_names() {
    let dir = std::env::temp_dir().join(format!("criogen-vertex-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let user = dir.join("user.json");
    std::fs::write(
        &user,
        r#"{"type": "authorized_user", "client_id": "c", "client_secret": "s",
            "refresh_token": "r", "quota_project_id": "quota-project"}"#,
    )
    .unwrap();
    let (_, project) = VertexCredential::from_file(&user).unwrap();
    assert_eq!(project.as_deref(), Some("quota-project"));
    let key = dir.join("key.json");
    std::fs::write(
        &key,
        r#"{"type": "service_account", "private_key": "-----BEGIN"}"#,
    )
    .unwrap();
    let err = VertexCredential::from_file(&key).unwrap_err();
    assert!(err.message.contains("not supported yet"), "{err}");
    std::fs::remove_dir_all(&dir).ok();

    for (project, location) in [
        ("My Project", "global"),
        ("p", "us-central1.evil.com"),
        ("p", ""),
    ] {
        let err =
            VertexModel::new(project, location, "m", VertexCredential::token("t")).unwrap_err();
        assert_eq!(
            err.kind,
            ModelErrorKind::InvalidRequest,
            "{project} {location}"
        );
    }
    let m = VertexModel::new(
        "p",
        "global",
        "claude-x/../../evil",
        VertexCredential::token("t"),
    )
    .unwrap()
    .with_api(VertexApi::Claude);
    let err = futures_lite_block_on(m.invoke(&hi())).unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::InvalidRequest);
}

/// Runs one future on a fresh runtime (the test above is synchronous).
fn futures_lite_block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}
