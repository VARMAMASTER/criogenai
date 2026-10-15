//! `ProviderEmbedder` against a scripted local server (design 0008 §16.1):
//! the request it sends, responses out of order, errors, key redaction.
#![cfg(feature = "http")]

use agentbuild_providers::{Embedder, Endpoint, ModelErrorKind, ProviderEmbedder};
use serde_json::json;

#[path = "support/server.rs"]
mod server;
use server::{json_response, serve};

fn texts(list: &[&str]) -> Vec<String> {
    list.iter().map(|t| t.to_string()).collect()
}

#[tokio::test]
async fn embeddings_are_requested_and_returned_in_input_order() {
    // The server answers out of order; `index` puts them back.
    let (base, server) = serve(vec![json_response(
        200,
        &json!({
            "object": "list",
            "data": [
                {"object": "embedding", "index": 1, "embedding": [0.0, 1.0]},
                {"object": "embedding", "index": 0, "embedding": [1.0, 0.0]}
            ],
            "model": "text-embedding-3-small",
            "usage": {"prompt_tokens": 5, "total_tokens": 5}
        })
        .to_string(),
    )])
    .await;
    let endpoint = Endpoint::openai_compatible("local", base, Some("sk-test-123".into()));
    let embedder = ProviderEmbedder::new(endpoint, "text-embedding-3-small")
        .unwrap()
        .dimensions(2);
    assert_eq!(embedder.provider(), "local");
    assert_eq!(embedder.model(), "text-embedding-3-small");
    let vectors = embedder.embed(&texts(&["first", "second"])).await.unwrap();
    assert_eq!(vectors, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);

    let received = server.await.unwrap();
    assert!(received.head.starts_with("POST /v1/embeddings HTTP/1.1"));
    assert_eq!(
        received.header("authorization").as_deref(),
        Some("Bearer sk-test-123")
    );
    assert_eq!(
        received.json(),
        json!({"model": "text-embedding-3-small", "input": ["first", "second"], "dimensions": 2})
    );
    // Nothing to embed: no request.
    assert!(embedder.embed(&[]).await.unwrap().is_empty());
}

#[tokio::test]
async fn short_or_broken_responses_are_invalid_responses() {
    for body in [
        json!({"data": [{"index": 0, "embedding": [1.0]}]}).to_string(),
        json!({"data": [{"index": 0, "embedding": [1.0]}, {"index": 0, "embedding": [2.0]}]})
            .to_string(),
        json!({"data": [{"index": 0, "embedding": []}, {"index": 1, "embedding": [2.0]}]})
            .to_string(),
        "not json".to_string(),
    ] {
        let (base, _server) = serve(vec![json_response(200, &body)]).await;
        let embedder =
            ProviderEmbedder::new(Endpoint::openai_compatible("local", base, None), "m").unwrap();
        let err = embedder.embed(&texts(&["a", "b"])).await.unwrap_err();
        assert_eq!(err.kind, ModelErrorKind::InvalidResponse, "{body}");
        assert_eq!(err.provider.as_deref(), Some("local"));
    }
}

#[tokio::test]
async fn errors_are_classified_and_never_show_the_key() {
    let (base, _server) = serve(vec![json_response(
        401,
        &json!({"error": {"message": "Incorrect API key provided: sk-secret-999", "type": "invalid_request_error"}})
            .to_string(),
    )])
    .await;
    let endpoint = Endpoint::openai_compatible("local", base, Some("sk-secret-999".into()));
    let embedder = ProviderEmbedder::new(endpoint, "m").unwrap();
    let err = embedder.embed(&texts(&["a"])).await.unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::Auth);
    assert!(!err.to_string().contains("sk-secret-999"), "{err}");
    assert_eq!(err.model.as_deref(), Some("m"));

    // Anthropic has no embeddings API.
    let err = ProviderEmbedder::new(Endpoint::anthropic("key"), "claude").unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::InvalidRequest);
}
