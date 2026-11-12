//! Azure OpenAI and Foundry (design 0006 §17) against scripted local
//! servers: the `api-key` header and the v1 paths, Entra ID tokens from a
//! client secret and from a managed identity, renewal after a 401, and no
//! secret in `Debug` or errors. Never the network.

#![cfg(feature = "http")]

#[path = "support/server.rs"]
mod server;

use std::time::Duration;

use criogenai_providers::{
    AzureApi, AzureCredential, AzureModel, Endpoint, HttpConfig, Message, Model, ModelErrorKind,
    ModelRequest, ProviderModel, azure_base_url,
};
use server::{json_response, serve, serve_each};

const ANSWER: &str = r#"{"status": "completed", "output": [{"type": "message",
    "content": [{"type": "output_text", "text": "Hi from Azure"}]}]}"#;
const CHAT_ANSWER: &str = r#"{"choices": [{"message": {"role": "assistant", "content": "Hi from Azure"},
    "finish_reason": "stop"}]}"#;

fn config() -> HttpConfig {
    HttpConfig::default()
        .connect_timeout(Duration::from_secs(2))
        .idle_timeout(Duration::from_secs(2))
        .total_timeout(Duration::from_secs(5))
}

/// The server's origin (`serve` returns `http://127.0.0.1:<port>/v1`).
fn origin(base: &str) -> String {
    base.trim_end_matches("/v1").to_string()
}

fn hi() -> ModelRequest {
    ModelRequest::new(vec![Message::user("hi")])
}

#[test]
fn resource_names_and_urls_become_v1_base_urls() {
    assert_eq!(
        azure_base_url("my-res"),
        "https://my-res.openai.azure.com/openai/v1"
    );
    for url in [
        "https://my-res.openai.azure.com",
        "https://my-res.openai.azure.com/",
        "https://my-res.openai.azure.com/openai",
        "https://my-res.openai.azure.com/openai/v1/",
    ] {
        assert_eq!(
            azure_base_url(url),
            "https://my-res.openai.azure.com/openai/v1",
            "{url}"
        );
    }
    assert_eq!(
        azure_base_url("https://hub.services.ai.azure.com"),
        "https://hub.services.ai.azure.com/openai/v1"
    );
    let endpoint = Endpoint::azure("my-res", "k");
    assert_eq!(endpoint.name, "azure");
    assert_eq!(endpoint.key_header.as_deref(), Some("api-key"));
    assert!(!format!("{endpoint:?}").contains("\"k\""));
}

#[tokio::test]
async fn an_api_key_goes_in_the_api_key_header() {
    let (base, server) = serve(vec![json_response(200, ANSWER)]).await;
    let model = ProviderModel::with_config(
        Endpoint::azure(&origin(&base), "azure-key-123"),
        "my-deployment",
        config(),
    )
    .unwrap();
    let response = model.invoke(&hi()).await.unwrap();
    assert_eq!(response.message.text(), "Hi from Azure");
    assert_eq!(response.message.metadata.provider.as_deref(), Some("azure"));
    let received = server.await.unwrap();
    assert!(
        received.head.starts_with("POST /openai/v1/responses"),
        "{}",
        received.head
    );
    assert_eq!(received.header("api-key").as_deref(), Some("azure-key-123"));
    assert_eq!(received.header("authorization"), None);
    assert_eq!(received.json()["model"], "my-deployment");
}

#[tokio::test]
async fn chat_completions_for_other_makers_models() {
    let (base, server) = serve(vec![json_response(200, CHAT_ANSWER)]).await;
    let model = AzureModel::new(
        &origin(&base),
        "DeepSeek-V4",
        AzureCredential::api_key("azure-key-123"),
    )
    .with_api(AzureApi::Chat)
    .with_config(config());
    assert_eq!(
        model.invoke(&hi()).await.unwrap().message.text(),
        "Hi from Azure"
    );
    let received = server.await.unwrap();
    assert!(
        received
            .head
            .starts_with("POST /openai/v1/chat/completions")
    );
    assert_eq!(received.header("api-key").as_deref(), Some("azure-key-123"));
}

#[tokio::test]
async fn a_client_secret_buys_a_bearer_token() {
    let (base, server) = serve_each(vec![
        vec![json_response(
            200,
            r#"{"token_type": "Bearer", "expires_in": 3599, "access_token": "entra-token-1"}"#,
        )],
        vec![json_response(200, ANSWER)],
    ])
    .await;
    let model = AzureModel::new(
        &origin(&base),
        "gpt-5-mini",
        AzureCredential::client_secret("my-tenant", "app-id", "s3cret&value"),
    )
    .with_authority(origin(&base))
    .with_config(config());
    assert_eq!(
        model.invoke(&hi()).await.unwrap().message.text(),
        "Hi from Azure"
    );

    let [token, call] = <[_; 2]>::try_from(server.await.unwrap()).ok().unwrap();
    assert!(
        token.head.starts_with("POST /my-tenant/oauth2/v2.0/token"),
        "{}",
        token.head
    );
    let form = String::from_utf8(token.body).unwrap();
    assert!(form.contains("grant_type=client_credentials"), "{form}");
    assert!(form.contains("client_id=app-id"), "{form}");
    assert!(form.contains("client_secret=s3cret%26value"), "{form}");
    assert!(
        form.contains("scope=https%3A%2F%2Fcognitiveservices.azure.com%2F.default"),
        "{form}"
    );
    assert_eq!(
        call.header("authorization").as_deref(),
        Some("Bearer entra-token-1")
    );
    assert_eq!(call.header("api-key"), None);
}

#[tokio::test]
async fn a_refused_token_is_renewed_once() {
    let token = |t: &str| {
        vec![json_response(
            200,
            &format!(r#"{{"expires_in": 3599, "access_token": "{t}"}}"#),
        )]
    };
    let (base, server) = serve_each(vec![
        token("old-token"),
        vec![json_response(
            401,
            r#"{"error": {"code": "401", "message": "expired"}}"#,
        )],
        token("new-token"),
        vec![json_response(200, ANSWER)],
    ])
    .await;
    let model = AzureModel::new(
        &origin(&base),
        "gpt-5-mini",
        AzureCredential::client_secret("t", "c", "s"),
    )
    .with_authority(origin(&base))
    .with_config(config());
    assert_eq!(
        model.invoke(&hi()).await.unwrap().message.text(),
        "Hi from Azure"
    );
    let received = server.await.unwrap();
    assert_eq!(
        received[3].header("authorization").as_deref(),
        Some("Bearer new-token")
    );
}

#[tokio::test]
async fn managed_identities_ask_the_metadata_service() {
    let (base, server) = serve_each(vec![
        vec![json_response(
            200,
            r#"{"access_token": "mi-token", "expires_in": "86399"}"#,
        )],
        vec![json_response(200, ANSWER)],
    ])
    .await;
    let model = AzureModel::new(
        &origin(&base),
        "gpt-5-mini",
        AzureCredential::managed_identity(Some("user-assigned-id".into())),
    )
    .with_identity_endpoint(
        format!("{}/metadata/identity/oauth2/token", origin(&base)),
        None,
    )
    .with_config(config());
    assert_eq!(
        model.invoke(&hi()).await.unwrap().message.text(),
        "Hi from Azure"
    );
    let [token, call] = <[_; 2]>::try_from(server.await.unwrap()).ok().unwrap();
    let line = token.head.lines().next().unwrap();
    assert!(
        line.starts_with("GET /metadata/identity/oauth2/token?"),
        "{line}"
    );
    assert!(line.contains("api-version=2018-02-01"), "{line}");
    assert!(
        line.contains("resource=https%3A%2F%2Fcognitiveservices.azure.com"),
        "{line}"
    );
    assert!(line.contains("client_id=user-assigned-id"), "{line}");
    assert_eq!(token.header("metadata").as_deref(), Some("true"));
    assert_eq!(
        call.header("authorization").as_deref(),
        Some("Bearer mi-token")
    );
}

#[tokio::test]
async fn app_service_identities_send_their_header() {
    let (base, server) = serve_each(vec![
        vec![json_response(
            200,
            r#"{"access_token": "app-token", "expires_on": "4102444800"}"#,
        )],
        vec![json_response(200, ANSWER)],
    ])
    .await;
    let model = AzureModel::new(
        &origin(&base),
        "gpt-5-mini",
        AzureCredential::managed_identity(None),
    )
    .with_identity_endpoint(
        format!("{}/msi/token", origin(&base)),
        Some("identity-secret".into()),
    )
    .with_config(config());
    model.invoke(&hi()).await.unwrap();
    let [token, _] = <[_; 2]>::try_from(server.await.unwrap()).ok().unwrap();
    assert!(
        token.head.contains("api-version=2019-08-01"),
        "{}",
        token.head
    );
    assert_eq!(
        token.header("x-identity-header").as_deref(),
        Some("identity-secret")
    );
}

#[tokio::test]
async fn token_failures_are_auth_errors_without_the_secret() {
    let (base, _server) = serve(vec![json_response(
        401,
        r#"{"error": "invalid_client", "error_description": "AADSTS7000215: bad secret very-secret-value"}"#,
    )])
    .await;
    let model = AzureModel::new(
        &origin(&base),
        "gpt-5-mini",
        AzureCredential::client_secret("t", "c", "very-secret-value"),
    )
    .with_authority(origin(&base))
    .with_config(config());
    let err = model.invoke(&hi()).await.unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::Auth);
    assert_eq!(err.provider.as_deref(), Some("azure"));
    assert!(!err.to_string().contains("very-secret-value"), "{err}");
    assert!(!format!("{model:?}").contains("very-secret-value"));
}

#[tokio::test]
async fn secrets_never_go_over_plain_http_to_another_machine() {
    let model = AzureModel::new(
        "https://my-res.openai.azure.com",
        "gpt-5-mini",
        AzureCredential::client_secret("t", "c", "s"),
    )
    .with_authority("http://login.example.com");
    let err = model.invoke(&hi()).await.unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::InvalidRequest);
    assert!(err.to_string().contains("plain http"), "{err}");

    let bad_tenant = AzureModel::new(
        "my-res",
        "gpt-5-mini",
        AzureCredential::client_secret("../evil", "c", "s"),
    );
    let err = bad_tenant.invoke(&hi()).await.unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::InvalidRequest);
}
