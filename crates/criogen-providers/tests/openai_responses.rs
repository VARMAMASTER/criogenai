//! The OpenAI Responses protocol (design 0006 §16): golden request bodies,
//! response decoding, streamed answers and a reasoning round trip against a
//! scripted local server. Never the network.

#![cfg(feature = "http")]

#[path = "support/server.rs"]
mod server;

use std::time::Duration;

use criogen_providers::{
    BlockDelta, ContentBlock, Endpoint, FinishReason, HttpConfig, MediaSource, Message,
    MessageDelta, Model, ModelError, ModelErrorKind, ModelRequest, ModelResponse, OutputSpec,
    Reasoning, ReasoningEffort, ResponsesOptions, Role, Settings, ToolChoice, ToolKind,
    ToolResultError, ToolSpec, ToolStatus, preset, preview_request,
};
use serde_json::{Map, Value, json};
use server::{Step, json_response, serve, sse_head};

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

fn local() -> Endpoint {
    Endpoint::openai_responses_compatible("local", "http://localhost:9/v1", None)
}

fn body_for(endpoint: &Endpoint, request: &ModelRequest) -> (Value, Vec<String>) {
    let (body, warnings) = preview_request(endpoint, "m", request, false).unwrap();
    (body, warnings.into_iter().map(|w| w.code).collect())
}

fn search_tool() -> ToolSpec {
    ToolSpec::new(
        "search",
        "Search the docs",
        json!({"type": "object", "properties": {"q": {"type": "string"}}}),
    )
}

/// A reasoning block as this protocol returns it: summary text plus the
/// opaque item under the endpoint's name.
fn kept_reasoning(provider: &str) -> ContentBlock {
    let mut block = ContentBlock::reasoning("Look it up first.");
    if let ContentBlock::Reasoning { extras, .. } = &mut block {
        extras.insert(
            provider.into(),
            json!({"reasoning": {"id": "rs_1", "encrypted_content": "gAAA"}}),
        );
    }
    block
}

// ---------- encode ----------

#[test]
fn conversation_with_reasoning_tool_calls_and_results() {
    let assistant = Message::with_content(
        Role::Assistant,
        vec![
            kept_reasoning("local"),
            ContentBlock::text("Let me look."),
            ContentBlock::tool_call("call_1", "search", args(json!({"q": "rust"}))),
            ContentBlock::text("Then this."),
        ],
    );
    let request = ModelRequest::new(vec![
        Message::system("Be brief."),
        Message::user("Find rust docs"),
        assistant,
        Message::tool_result(
            "call_1",
            "search",
            ToolStatus::Success,
            vec![
                ContentBlock::text("2 hits"),
                ContentBlock::json(json!({"top": "doc.rust-lang.org"})),
            ],
        ),
    ])
    .with_tools(vec![search_tool()]);
    let (body, warnings) = body_for(&local(), &request);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        body,
        json!({
            "model": "m",
            "instructions": "Be brief.",
            "input": [
                {"role": "user", "content": "Find rust docs"},
                {"type": "reasoning", "id": "rs_1", "encrypted_content": "gAAA",
                 "summary": [{"type": "summary_text", "text": "Look it up first."}]},
                {"role": "assistant", "content": "Let me look."},
                {"type": "function_call", "call_id": "call_1", "name": "search",
                 "arguments": "{\"q\":\"rust\"}"},
                {"role": "assistant", "content": "Then this."},
                {"type": "function_call_output", "call_id": "call_1",
                 "output": "2 hits\n{\"top\":\"doc.rust-lang.org\"}"}
            ],
            "tools": [{"type": "function", "name": "search", "description": "Search the docs",
                "parameters": {"type": "object", "properties": {"q": {"type": "string"}}},
                "strict": false}],
            "store": false,
            "include": ["reasoning.encrypted_content"]
        })
    );
}

#[test]
fn reasoning_from_another_provider_is_dropped() {
    let assistant = Message::with_content(
        Role::Assistant,
        vec![kept_reasoning("anthropic"), ContentBlock::text("ok")],
    );
    let request = ModelRequest::new(vec![Message::user("hi"), assistant]);
    let (body, warnings) = body_for(&local(), &request);
    assert_eq!(warnings, ["reasoning_dropped"]);
    assert_eq!(
        body["input"][1],
        json!({"role": "assistant", "content": "ok"})
    );
}

#[test]
fn later_system_messages_media_and_tool_images() {
    let user = Message::with_content(
        Role::User,
        vec![
            ContentBlock::text("What is this?"),
            ContentBlock::image(
                MediaSource::Base64 {
                    data: "AAAA".into(),
                },
                Some("image/jpeg".into()),
            ),
            ContentBlock::image(
                MediaSource::Url {
                    url: "https://example.com/cat.png".into(),
                },
                None,
            ),
        ],
    );
    let assistant = Message::with_content(
        Role::Assistant,
        vec![ContentBlock::tool_call("c1", "shot", Map::new())],
    );
    let result = Message::tool_result(
        "c1",
        "shot",
        ToolStatus::Success,
        vec![
            ContentBlock::text("here"),
            ContentBlock::image(
                MediaSource::Base64 {
                    data: "BBBB".into(),
                },
                None,
            ),
        ],
    );
    let request = ModelRequest::new(vec![
        Message::user("hi"),
        Message::system("Answer in French now."),
        user,
        assistant,
        result,
    ]);
    let (body, warnings) = body_for(&local(), &request);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(body.get("instructions").is_none());
    let input = &body["input"];
    assert_eq!(
        input[1],
        json!({"role": "developer", "content": "Answer in French now."})
    );
    assert_eq!(
        input[2]["content"],
        json!([
            {"type": "input_text", "text": "What is this?"},
            {"type": "input_image", "image_url": "data:image/jpeg;base64,AAAA"},
            {"type": "input_image", "image_url": "https://example.com/cat.png"}
        ])
    );
    assert_eq!(
        input[4]["output"],
        json!([
            {"type": "input_text", "text": "here"},
            {"type": "input_image", "image_url": "data:image/png;base64,BBBB"}
        ])
    );
}

#[test]
fn invalid_calls_and_errors_keep_their_pairs() {
    let assistant = Message::with_content(
        Role::Assistant,
        vec![ContentBlock::invalid_tool_call(
            "c1",
            Some("search".into()),
            "{\"q\": ",
            "truncated",
        )],
    );
    let mut error = Message::tool_result("c1", "search", ToolStatus::Error, vec![]);
    error.error = Some(ToolResultError::new("bad_args", "took too long"));
    let request = ModelRequest::new(vec![Message::user("hi"), assistant, error]);
    let (body, _) = body_for(&local(), &request);
    assert_eq!(body["input"][1]["arguments"], "{\"q\": ");
    assert_eq!(body["input"][2]["output"], "Error: took too long");
}

#[test]
fn settings_choice_output_and_extras() {
    let mut request = ModelRequest::new(vec![Message::user("hi")]).with_tools(vec![search_tool()]);
    request.tool_choice = ToolChoice::Tool("search".into());
    let mut settings = Settings::default();
    settings.max_tokens = Some(256);
    settings.temperature = Some(0.2);
    settings.stop = vec!["END".into()];
    settings.parallel_tool_calls = Some(false);
    settings.reasoning = Some(Reasoning::Effort {
        effort: ReasoningEffort::High,
    });
    request.settings = settings;
    request.output = Some(OutputSpec::new(json!({"type": "object"})));
    request.extras.insert(
        "openai".into(),
        json!({"temperature": 0.0, "stream": false, "user": "u1"}),
    );

    let (body, warnings) = body_for(&Endpoint::openai("k"), &request);
    assert_eq!(warnings, ["stop_dropped", "extras_ignored"]);
    assert_eq!(body["max_output_tokens"], 256);
    assert_eq!(body["parallel_tool_calls"], false);
    assert_eq!(
        body["reasoning"],
        json!({"effort": "high", "summary": "auto"})
    );
    assert_eq!(
        body["tool_choice"],
        json!({"type": "function", "name": "search"})
    );
    assert_eq!(body["text"]["format"]["type"], "json_schema");
    assert_eq!(body["text"]["format"]["strict"], true);
    assert_eq!(body["temperature"], 0.0, "extras win");
    assert_eq!(body["user"], "u1");
    assert!(body.get("stop").is_none() && body.get("stream").is_none());

    request.settings.reasoning = Some(Reasoning::Budget {
        budget_tokens: 1000,
    });
    request.settings.stop.clear();
    request.extras.clear();
    let (body, warnings) = body_for(&Endpoint::openai("k"), &request);
    assert_eq!(warnings, ["reasoning_setting_dropped"]);
    assert!(body.get("reasoning").is_none());
}

#[test]
fn hosted_tools_are_dropped_and_options_change_the_body() {
    let mut hosted = search_tool();
    hosted.kind = ToolKind::Hosted;
    let request = ModelRequest::new(vec![Message::user("hi")]).with_tools(vec![hosted]);
    let (body, warnings) = body_for(&local(), &request);
    assert_eq!(warnings, ["hosted_tool_dropped"]);
    assert!(body.get("tools").is_none());

    let stored = local().with_responses_options(
        ResponsesOptions::default()
            .store(true)
            .reasoning_summary(None),
    );
    let mut request = ModelRequest::new(vec![Message::user("hi")]);
    request.settings.reasoning = Some(Reasoning::Effort {
        effort: ReasoningEffort::Low,
    });
    let (body, _) = body_for(&stored, &request);
    assert_eq!(body["store"], true);
    assert!(
        body.get("include").is_none(),
        "stored reasoning needs no copy"
    );
    assert_eq!(body["reasoning"], json!({"effort": "low"}));
}

#[test]
fn hosted_tool_items_go_back_only_to_their_endpoint() {
    let item = json!({"type": "web_search_call", "id": "ws_1", "status": "completed"});
    let assistant = Message::with_content(
        Role::Assistant,
        vec![
            ContentBlock::provider("local", item.clone()),
            ContentBlock::provider("anthropic", json!({"type": "server_tool_use"})),
            ContentBlock::text("Found it."),
        ],
    );
    let request = ModelRequest::new(vec![Message::user("hi"), assistant]);
    let (body, warnings) = body_for(&local(), &request);
    assert_eq!(warnings, ["content_dropped"]);
    assert_eq!(body["input"][1], item);
    assert_eq!(
        body["input"][2],
        json!({"role": "assistant", "content": "Found it."})
    );
}

// ---------- end to end ----------

fn model_at(base: &str, key: Option<&str>) -> criogen_providers::ProviderModel {
    let endpoint = Endpoint::openai_responses_compatible("local", base, key.map(str::to_string));
    let config = HttpConfig::default()
        .connect_timeout(Duration::from_secs(2))
        .idle_timeout(Duration::from_secs(2))
        .total_timeout(Duration::from_secs(5));
    criogen_providers::ProviderModel::with_config(endpoint, "test-model", config).unwrap()
}

const WHOLE: &str = r#"{"id": "resp_1", "object": "response", "model": "gpt-test-2026",
  "status": "completed",
  "output": [
    {"type": "reasoning", "id": "rs_1", "encrypted_content": "gAAA",
     "summary": [{"type": "summary_text", "text": "Need the docs."}]},
    {"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
     "content": [{"type": "output_text", "text": "Searching.", "annotations": []}]},
    {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "search",
     "arguments": "{\"q\":\"rust\"}", "status": "completed"},
    {"type": "web_search_call", "id": "ws_1", "status": "completed"}
  ],
  "usage": {"input_tokens": 20, "output_tokens": 9,
            "input_tokens_details": {"cached_tokens": 4},
            "output_tokens_details": {"reasoning_tokens": 6}}}"#;

#[tokio::test]
async fn invoke_decodes_items_usage_and_sends_auth() {
    let (base, server) = serve(vec![json_response(200, WHOLE)]).await;
    let response = model_at(&base, Some("sk-test"))
        .invoke(&ModelRequest::new(vec![Message::user("hi")]))
        .await
        .unwrap();
    let content = &response.message.content;
    assert_eq!(content.len(), 4, "{content:?}");
    match &content[0] {
        ContentBlock::Reasoning { text, extras, .. } => {
            assert_eq!(text.as_deref(), Some("Need the docs."));
            assert_eq!(
                extras["local"],
                json!({"reasoning": {"id": "rs_1", "encrypted_content": "gAAA"}})
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(response.message.text(), "Searching.");
    match &content[2] {
        ContentBlock::ToolCall { id, name, args, .. } => {
            assert_eq!((id.as_str(), name.as_str()), ("call_1", "search"));
            assert_eq!(args["q"], "rust");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(&content[3], ContentBlock::Provider { provider, .. } if provider == "local"));
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
    let usage = &response.usage;
    assert_eq!((usage.input_tokens, usage.output_tokens), (20, 9));
    assert_eq!((usage.cache_read_tokens, usage.reasoning_tokens), (4, 6));
    let meta = &response.message.metadata;
    assert_eq!(meta.model.as_deref(), Some("gpt-test-2026"));
    assert_eq!(meta.other["provider_message_id"], "resp_1");

    let received = server.await.unwrap();
    assert!(
        received.head.starts_with("POST /v1/responses"),
        "{}",
        received.head
    );
    assert_eq!(
        received.header("authorization").as_deref(),
        Some("Bearer sk-test")
    );
    assert_eq!(received.json()["store"], false);
}

/// What the model said comes back unchanged on the next turn: reasoning
/// (opaque), calls and hosted items in their order.
#[tokio::test]
async fn a_reasoning_turn_round_trips() {
    let (base, _server) = serve(vec![json_response(200, WHOLE)]).await;
    let model = model_at(&base, None);
    let first = model
        .invoke(&ModelRequest::new(vec![Message::user("hi")]))
        .await
        .unwrap();
    let request = ModelRequest::new(vec![
        Message::user("hi"),
        first.message,
        Message::tool_result(
            "call_1",
            "search",
            ToolStatus::Success,
            vec![ContentBlock::text("ok")],
        ),
    ]);
    let (body, warnings) = preview_request(model.endpoint(), "m", &request, false).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let kinds: Vec<&str> = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["type"].as_str().or(item["role"].as_str()).unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "user",
            "reasoning",
            "assistant",
            "function_call",
            "web_search_call",
            "function_call_output"
        ]
    );
    assert_eq!(body["input"][1]["encrypted_content"], "gAAA");
    assert_eq!(body["input"][1]["id"], "rs_1");
}

#[tokio::test]
async fn refusals_incomplete_answers_and_failures() {
    let refusal = r#"{"status": "completed", "output": [{"type": "message", "content": [
        {"type": "refusal", "refusal": "I can't help with that."}]}]}"#;
    let (base, _s) = serve(vec![json_response(200, refusal)]).await;
    let response = model_at(&base, None)
        .invoke(&ModelRequest::new(vec![Message::user("hi")]))
        .await
        .unwrap();
    assert_eq!(response.finish_reason, FinishReason::ContentFilter);
    assert_eq!(response.message.text(), "I can't help with that.");

    let cut = r#"{"status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"},
        "output": [{"type": "message", "content": [{"type": "output_text", "text": "Par"}]}]}"#;
    let (base, _s) = serve(vec![json_response(200, cut)]).await;
    let response = model_at(&base, None)
        .invoke(&ModelRequest::new(vec![Message::user("hi")]))
        .await
        .unwrap();
    assert_eq!(response.finish_reason, FinishReason::Length);
    assert_eq!(
        response.message.metadata.raw_finish_reason.as_deref(),
        Some("incomplete:max_output_tokens")
    );

    let failed = r#"{"status": "failed", "output": [],
        "error": {"code": "rate_limit_exceeded", "message": "slow down"}}"#;
    let (base, _s) = serve(vec![json_response(200, failed)]).await;
    let err = model_at(&base, None)
        .invoke(&ModelRequest::new(vec![Message::user("hi")]))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::RateLimited);
}

#[tokio::test]
async fn http_errors_are_classified_and_the_key_redacted() {
    let (base, _s) = serve(vec![json_response(
        401,
        r#"{"error": {"message": "Incorrect API key sk-test-123", "code": "invalid_api_key"}}"#,
    )])
    .await;
    let err = model_at(&base, Some("sk-test-123"))
        .invoke(&ModelRequest::new(vec![Message::user("hi")]))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::Auth);
    assert!(!err.to_string().contains("sk-test-123"), "{err}");
}

// ---------- streaming ----------

fn event(data: Value) -> Step {
    let kind = data["type"].as_str().unwrap().to_string();
    Step::Send(format!("event: {kind}\ndata: {data}\n\n").into_bytes())
}

async fn stream_at(
    base: &str,
    request: &ModelRequest,
) -> (Result<ModelResponse, ModelError>, Vec<MessageDelta>) {
    let model = model_at(base, None);
    let mut deltas = Vec::new();
    let mut sink = |delta: MessageDelta| deltas.push(delta);
    let result = model.stream(request, &mut sink).await;
    (result, deltas)
}

#[tokio::test]
async fn a_streamed_turn_matches_the_whole_one() {
    let script = vec![
        sse_head(),
        event(
            json!({"type": "response.created", "response": {"id": "resp_2", "model": "gpt-test"}}),
        ),
        event(
            json!({"type": "response.output_item.added", "output_index": 0,
            "item": {"type": "reasoning", "id": "rs_1", "summary": []}}),
        ),
        event(
            json!({"type": "response.reasoning_summary_text.delta", "output_index": 0,
            "summary_index": 0, "delta": "Need "}),
        ),
        event(
            json!({"type": "response.reasoning_summary_text.delta", "output_index": 0,
            "summary_index": 0, "delta": "docs."}),
        ),
        event(
            json!({"type": "response.reasoning_summary_text.delta", "output_index": 0,
            "summary_index": 1, "delta": "Then answer."}),
        ),
        event(
            json!({"type": "response.output_item.done", "output_index": 0,
            "item": {"type": "reasoning", "id": "rs_1", "encrypted_content": "gAAA",
                     "summary": [{"type": "summary_text", "text": "Need docs."}]}}),
        ),
        event(
            json!({"type": "response.output_item.added", "output_index": 1,
            "item": {"type": "message", "id": "msg_1", "content": []}}),
        ),
        event(
            json!({"type": "response.output_text.delta", "output_index": 1,
            "content_index": 0, "delta": "Search"}),
        ),
        event(
            json!({"type": "response.output_text.delta", "output_index": 1,
            "content_index": 0, "delta": "ing."}),
        ),
        event(
            json!({"type": "response.output_item.done", "output_index": 1,
            "item": {"type": "message", "content": [{"type": "output_text", "text": "Searching."}]}}),
        ),
        event(
            json!({"type": "response.output_item.added", "output_index": 2,
            "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1",
                     "name": "search", "arguments": ""}}),
        ),
        event(
            json!({"type": "response.function_call_arguments.delta", "output_index": 2,
            "delta": "{\"q\":"}),
        ),
        event(
            json!({"type": "response.function_call_arguments.delta", "output_index": 2,
            "delta": "\"rust\"}"}),
        ),
        event(
            json!({"type": "response.output_item.done", "output_index": 2,
            "item": {"type": "function_call", "call_id": "call_1", "name": "search",
                     "arguments": "{\"q\":\"rust\"}"}}),
        ),
        event(
            json!({"type": "response.completed", "response": {"id": "resp_2",
            "model": "gpt-test", "status": "completed",
            "usage": {"input_tokens": 11, "output_tokens": 7}}}),
        ),
    ];
    let (base, server) = serve(script).await;
    let request = ModelRequest::new(vec![Message::user("hi")]).with_tools(vec![search_tool()]);
    let (result, deltas) = stream_at(&base, &request).await;
    let response = result.unwrap();
    assert!(server.await.unwrap().json()["stream"] == true);

    let content = &response.message.content;
    assert_eq!(content.len(), 3, "{content:?}");
    match &content[0] {
        ContentBlock::Reasoning { text, extras, .. } => {
            assert_eq!(text.as_deref(), Some("Need docs.\n\nThen answer."));
            assert_eq!(extras["local"]["reasoning"]["encrypted_content"], "gAAA");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(response.message.text(), "Searching.");
    assert!(matches!(&content[2], ContentBlock::ToolCall { args, .. } if args["q"] == "rust"));
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
    assert_eq!(response.usage.input_tokens, 11);
    assert_eq!(response.message.metadata.model.as_deref(), Some("gpt-test"));

    let text: String = deltas
        .iter()
        .filter_map(|d| match &d.delta {
            BlockDelta::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Searching.", "text arrived as deltas, not twice");
}

#[tokio::test]
async fn whole_items_without_deltas_and_hosted_items() {
    let script = vec![
        sse_head(),
        event(json!({"type": "response.created", "response": {"id": "r"}})),
        event(
            json!({"type": "response.output_item.done", "output_index": 0,
            "item": {"type": "web_search_call", "id": "ws_1", "status": "completed"}}),
        ),
        event(
            json!({"type": "response.output_item.added", "output_index": 1,
            "item": {"type": "message", "content": []}}),
        ),
        event(
            json!({"type": "response.output_item.done", "output_index": 1,
            "item": {"type": "message", "content": [{"type": "output_text", "text": "Done.",
                "annotations": [{"type": "url_citation", "url": "https://example.com"}]}]}}),
        ),
        event(
            json!({"type": "response.output_item.done", "output_index": 2,
            "item": {"type": "function_call", "call_id": "c2", "name": "search",
                     "arguments": "{}"}}),
        ),
        event(json!({"type": "response.completed", "response": {"status": "completed"}})),
    ];
    let (base, _s) = serve(script).await;
    let (result, _) = stream_at(&base, &ModelRequest::new(vec![Message::user("hi")])).await;
    let response = result.unwrap();
    let content = &response.message.content;
    assert!(matches!(&content[0], ContentBlock::Provider { data, .. } if data["id"] == "ws_1"));
    match &content[1] {
        ContentBlock::Text { text, extras, .. } => {
            assert_eq!(text, "Done.");
            assert_eq!(
                extras["local"]["annotations"][0]["url"],
                "https://example.com"
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(&content[2], ContentBlock::ToolCall { id, .. } if id == "c2"));
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
}

#[tokio::test]
async fn a_cut_call_is_truncated_and_failures_are_errors() {
    let script = vec![
        sse_head(),
        event(
            json!({"type": "response.output_item.added", "output_index": 0,
            "item": {"type": "function_call", "call_id": "c1", "name": "search"}}),
        ),
        event(
            json!({"type": "response.function_call_arguments.delta", "output_index": 0,
            "delta": "{\"q\": \"ru"}),
        ),
        event(
            json!({"type": "response.incomplete", "response": {"status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"}}}),
        ),
    ];
    let (base, _s) = serve(script).await;
    let (result, _) = stream_at(&base, &ModelRequest::new(vec![Message::user("hi")])).await;
    let response = result.unwrap();
    assert_eq!(response.finish_reason, FinishReason::Length);
    assert!(matches!(
        &response.message.content[0],
        ContentBlock::InvalidToolCall { error, .. } if error == "truncated"
    ));

    let script = vec![
        sse_head(),
        event(
            json!({"type": "response.output_item.added", "output_index": 0,
            "item": {"type": "message", "content": []}}),
        ),
        event(
            json!({"type": "response.output_text.delta", "output_index": 0,
            "content_index": 0, "delta": "Hel"}),
        ),
        event(
            json!({"type": "response.failed", "response": {"status": "failed",
            "error": {"code": "server_error", "message": "boom"}}}),
        ),
    ];
    let (base, _s) = serve(script).await;
    let (result, deltas) = stream_at(&base, &ModelRequest::new(vec![Message::user("hi")])).await;
    let err = result.unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::Server);
    assert!(err.emitted_deltas, "deltas were sent before the failure");
    assert_eq!(deltas.len(), 1);

    let script = vec![
        sse_head(),
        event(json!({"type": "error", "code": "rate_limit_exceeded", "message": "slow down"})),
    ];
    let (base, _s) = serve(script).await;
    let (result, _) = stream_at(&base, &ModelRequest::new(vec![Message::user("hi")])).await;
    assert_eq!(result.unwrap_err().kind, ModelErrorKind::RateLimited);
}

#[tokio::test]
async fn a_streamed_refusal_is_a_content_filter() {
    let script = vec![
        sse_head(),
        event(
            json!({"type": "response.output_item.added", "output_index": 0,
            "item": {"type": "message", "content": []}}),
        ),
        event(json!({"type": "response.refusal.delta", "output_index": 0,
            "content_index": 0, "delta": "No."})),
        event(json!({"type": "response.completed", "response": {"status": "completed"}})),
    ];
    let (base, _s) = serve(script).await;
    let (result, _) = stream_at(&base, &ModelRequest::new(vec![Message::user("hi")])).await;
    let response = result.unwrap();
    assert_eq!(response.finish_reason, FinishReason::ContentFilter);
    match &response.message.content[0] {
        ContentBlock::Text { text, extras, .. } => {
            assert_eq!(text, "No.");
            assert_eq!(extras["openai"]["refusal"], true);
        }
        other => panic!("{other:?}"),
    }
}

// ---------- the preset ----------

#[tokio::test]
async fn the_openai_preset_speaks_responses_and_chat_stays_available() {
    let endpoint = preset("openai")
        .unwrap()
        .endpoint(Some("sk-test".into()))
        .unwrap();
    assert!(matches!(
        endpoint.protocol,
        criogen_providers::Protocol::OpenAiResponses(_)
    ));
    assert_eq!(endpoint.capabilities.reasoning, Some(true));
    assert!(matches!(
        Endpoint::openai_chat("k").protocol,
        criogen_providers::Protocol::OpenAiChat(_)
    ));

    let (base, server) = serve(vec![json_response(
        200,
        r#"{"status": "completed", "output": [{"type": "message",
            "content": [{"type": "output_text", "text": "Hi"}]}]}"#,
    )])
    .await;
    let model =
        criogen_providers::ProviderModel::new(endpoint.with_base_url(&base), "gpt-test").unwrap();
    let response = model
        .invoke(&ModelRequest::new(vec![Message::user("hi")]))
        .await
        .unwrap();
    assert_eq!(response.message.text(), "Hi");
    assert_eq!(
        response.message.metadata.provider.as_deref(),
        Some("openai")
    );
    let received = server.await.unwrap();
    assert!(received.head.starts_with("POST /v1/responses"));
}
