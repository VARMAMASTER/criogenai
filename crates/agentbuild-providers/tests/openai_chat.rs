//! The OpenAI chat-completions protocol: golden request bodies (one per
//! mapping row of research litellm-providers §10.5), response decoding, and
//! end-to-end calls against a scripted local server (streaming included).

#![cfg(feature = "http")]

#[path = "support/server.rs"]
mod server;

use std::time::Duration;

use agentbuild_providers::{
    BlockDelta, ContentBlock, Dispatch, Endpoint, FinishReason, HttpConfig, MediaSource, Message,
    MessageDelta, Model, ModelErrorKind, ModelRequest, OutputSpec, OutputStrategy, ProviderModel,
    Reasoning, ReasoningEffort, Role, Settings, ToolChoice, ToolKind, ToolResultError, ToolSpec,
    ToolStatus, preview_request, transcript,
};
use serde_json::{Map, Value, json};
use server::{Step, json_response, serve, sse_data, sse_head};

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

fn compat() -> Endpoint {
    Endpoint::openai_compatible("local", "http://localhost:9/v1", None)
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

// ---------- encode ----------

#[test]
fn conversation_with_tool_calls_and_results() {
    let assistant = Message::with_content(
        Role::Assistant,
        vec![
            ContentBlock::text("Let me look."),
            ContentBlock::tool_call("c1", "search", args(json!({"q": "rust"}))),
        ],
    );
    let request = ModelRequest::new(vec![
        Message::system("Be brief."),
        Message::user("Find rust docs"),
        assistant,
        Message::tool_result(
            "c1",
            "search",
            ToolStatus::Success,
            vec![
                ContentBlock::text("2 hits"),
                ContentBlock::json(json!({"top": "doc.rust-lang.org"})),
            ],
        ),
    ])
    .with_tools(vec![search_tool()]);
    let (body, warnings) = body_for(&compat(), &request);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        body,
        json!({
            "model": "m",
            "messages": [
                {"role": "system", "content": "Be brief."},
                {"role": "user", "content": "Find rust docs"},
                {"role": "assistant", "content": "Let me look.", "tool_calls": [
                    {"id": "c1", "type": "function", "function": {"name": "search", "arguments": "{\"q\":\"rust\"}"}}
                ]},
                {"role": "tool", "tool_call_id": "c1", "content": "2 hits\n{\"top\":\"doc.rust-lang.org\"}"}
            ],
            "tools": [{"type": "function", "function": {
                "name": "search", "description": "Search the docs",
                "parameters": {"type": "object", "properties": {"q": {"type": "string"}}}
            }}]
        })
    );
}

#[test]
fn assistant_with_only_tool_calls_has_null_content() {
    let request = ModelRequest::new(vec![Message::with_content(
        Role::Assistant,
        vec![ContentBlock::tool_call("c1", "search", Map::new())],
    )]);
    let (body, _) = body_for(&compat(), &request);
    assert_eq!(body["messages"][0]["content"], Value::Null);
}

#[test]
fn user_media_parts() {
    let mut message = Message::with_content(
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
    message.name = Some("ada".into());
    let (body, warnings) = body_for(&compat(), &ModelRequest::new(vec![message]));
    assert!(warnings.is_empty());
    assert_eq!(
        body["messages"][0],
        json!({"role": "user", "name": "ada", "content": [
            {"type": "text", "text": "What is this?"},
            {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,AAAA"}},
            {"type": "image_url", "image_url": {"url": "https://example.com/cat.png"}}
        ]})
    );
}

#[test]
fn unsupported_user_content_is_dropped_or_rejected() {
    let file_by_url = ContentBlock::file(
        MediaSource::Url {
            url: "https://x/doc.pdf".into(),
        },
        None,
    );
    let mixed = Message::with_content(
        Role::User,
        vec![ContentBlock::text("read this"), file_by_url.clone()],
    );
    let (body, warnings) = body_for(&compat(), &ModelRequest::new(vec![mixed]));
    assert_eq!(warnings, ["content_dropped"]);
    assert_eq!(body["messages"][0]["content"], "read this");

    let only = Message::with_content(Role::User, vec![file_by_url]);
    let err = preview_request(&compat(), "m", &ModelRequest::new(vec![only]), false).unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::InvalidRequest);
}

#[test]
fn tool_result_images_move_to_a_user_message_after_the_tool_run() {
    let assistant = Message::with_content(
        Role::Assistant,
        vec![
            ContentBlock::tool_call("c1", "shot", Map::new()),
            ContentBlock::tool_call("c2", "shot", Map::new()),
        ],
    );
    let image = ContentBlock::image(
        MediaSource::Base64 {
            data: "QQ==".into(),
        },
        Some("image/png".into()),
    );
    let request = ModelRequest::new(vec![
        assistant,
        Message::tool_result("c1", "shot", ToolStatus::Success, vec![image]),
        Message::tool_result("c2", "shot", ToolStatus::Success, vec![]),
    ]);
    let (body, warnings) = body_for(&compat(), &request);
    assert_eq!(warnings, ["tool_result_images_moved"]);
    let roles: Vec<&str> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["assistant", "tool", "tool", "user"]);
    assert_eq!(body["messages"][1]["content"], "(no output)");
    assert_eq!(
        body["messages"][3]["content"][1]["image_url"]["url"],
        "data:image/png;base64,QQ=="
    );
}

#[test]
fn invalid_calls_and_error_results_keep_their_pairing() {
    let assistant = Message::with_content(
        Role::Assistant,
        vec![ContentBlock::invalid_tool_call(
            "c1",
            Some("search".into()),
            "{\"q\": ",
            "truncated",
        )],
    );
    let repaired = transcript::repair(vec![Message::user("go"), assistant]).messages;
    let mut error_without_prefix = Message::tool_result("c9", "x", ToolStatus::Error, vec![]);
    error_without_prefix.error = Some(ToolResultError::new("timeout", "took too long"));
    let mut messages = repaired;
    messages.push(Message::with_content(
        Role::Assistant,
        vec![ContentBlock::tool_call("c9", "x", Map::new())],
    ));
    messages.push(error_without_prefix);
    let (body, _) = body_for(&compat(), &ModelRequest::new(messages));
    let m = &body["messages"];
    assert_eq!(m[1]["tool_calls"][0]["function"]["arguments"], "{\"q\": ");
    assert_eq!(m[2]["tool_call_id"], "c1");
    assert_eq!(
        m[2]["content"],
        "Error: Arguments could not be parsed: truncated"
    );
    assert_eq!(m[4]["content"], "Error: took too long");
}

#[test]
fn settings_choice_and_quirks() {
    let mut request = ModelRequest::new(vec![Message::user("hi")]).with_tools(vec![search_tool()]);
    request.tool_choice = ToolChoice::Tool("search".into());
    let mut settings = Settings::default();
    settings.max_tokens = Some(256);
    settings.temperature = Some(0.2);
    settings.stop = vec!["END".into()];
    settings.parallel_tool_calls = Some(false);
    settings.reasoning = Some(Reasoning::Effort {
        effort: ReasoningEffort::Low,
    });
    request.settings = settings;

    let (openai, warnings) = body_for(&Endpoint::openai("k"), &request);
    assert!(warnings.is_empty());
    assert_eq!(openai["max_completion_tokens"], 256);
    assert_eq!(openai["parallel_tool_calls"], false);
    assert_eq!(openai["reasoning_effort"], "low");
    assert_eq!(
        openai["tool_choice"],
        json!({"type": "function", "function": {"name": "search"}})
    );
    assert_eq!(openai["stop"], json!(["END"]));

    let (local, warnings) = body_for(&compat(), &request);
    assert_eq!(warnings, ["reasoning_setting_dropped"]);
    assert_eq!(local["max_tokens"], 256);
    assert!(local.get("max_completion_tokens").is_none());
    assert!(local.get("parallel_tool_calls").is_none());
    assert!(local.get("reasoning_effort").is_none());
}

#[test]
fn tool_choice_is_sent_only_with_tools() {
    let mut request = ModelRequest::new(vec![Message::user("hi")]);
    request.tool_choice = ToolChoice::Required;
    let (body, _) = body_for(&compat(), &request);
    assert!(body.get("tool_choice").is_none() && body.get("tools").is_none());
}

#[test]
fn streaming_flags_structured_output_and_extras() {
    let mut request = ModelRequest::new(vec![Message::user("hi")]);
    request.output = Some(OutputSpec::new(json!({"type": "object"})));
    request.settings.temperature = Some(1.0);
    request
        .extras
        .insert("local".into(), json!({"temperature": 0.0, "top_k": 5}));
    let (body, _) = preview_request(&compat(), "m", &request, true).unwrap();
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"], json!({"include_usage": true}));
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    assert_eq!(body["temperature"], 0.0, "extras win");
    assert_eq!(body["top_k"], 5);

    request.output.as_mut().unwrap().strategy = Some(OutputStrategy::Tool);
    let (body, _) = preview_request(&compat(), "m", &request, false).unwrap();
    assert!(body.get("response_format").is_none());
}

#[test]
fn reasoning_and_hosted_tools_are_dropped_with_warnings() {
    let assistant = Message::with_content(
        Role::Assistant,
        vec![
            ContentBlock::reasoning("thinking"),
            ContentBlock::text("ok"),
        ],
    );
    let mut hosted = search_tool();
    hosted.kind = ToolKind::Hosted;
    let request = ModelRequest::new(vec![Message::user("hi"), assistant]).with_tools(vec![hosted]);
    let (body, warnings) = body_for(&compat(), &request);
    assert_eq!(warnings, ["reasoning_dropped", "hosted_tool_dropped"]);
    assert!(body.get("tools").is_none());
    assert_eq!(
        body["messages"][1],
        json!({"role": "assistant", "content": "ok"})
    );
}

// ---------- end to end (decode included) ----------

fn model_at(base: &str, key: Option<&str>) -> ProviderModel {
    let endpoint = Endpoint::openai_compatible("local", base, key.map(str::to_string));
    let config = HttpConfig::default()
        .connect_timeout(Duration::from_secs(2))
        .idle_timeout(Duration::from_secs(2))
        .total_timeout(Duration::from_secs(5));
    ProviderModel::with_config(endpoint, "test-model", config).unwrap()
}

#[tokio::test]
async fn invoke_decodes_text_usage_and_sends_auth() {
    let (base, server) = serve(vec![json_response(
        200,
        r#"{"id": "chatcmpl-1", "model": "test-model-2026", "choices": [{"index": 0,
            "message": {"role": "assistant", "content": "Hello!"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 3,
                      "prompt_tokens_details": {"cached_tokens": 8},
                      "completion_tokens_details": {"reasoning_tokens": null}}}"#,
    )])
    .await;
    let model = model_at(&base, Some("sk-test"));
    let response = model
        .invoke(&ModelRequest::new(vec![Message::user("hi")]))
        .await
        .unwrap();
    assert_eq!(response.message.text(), "Hello!");
    assert_eq!(response.finish_reason, FinishReason::Stop);
    assert_eq!(
        (response.usage.input_tokens, response.usage.output_tokens),
        (12, 3)
    );
    assert_eq!(response.usage.cache_read_tokens, 8);
    let meta = &response.message.metadata;
    assert_eq!(meta.provider.as_deref(), Some("local"));
    assert_eq!(meta.model.as_deref(), Some("test-model-2026"));
    assert_eq!(meta.raw_finish_reason.as_deref(), Some("stop"));
    assert_eq!(meta.other["provider_message_id"], "chatcmpl-1");

    let received = server.await.unwrap();
    assert!(received.head.starts_with("POST /v1/chat/completions"));
    assert_eq!(
        received.header("authorization").as_deref(),
        Some("Bearer sk-test")
    );
    assert_eq!(received.json()["model"], "test-model");
}

#[tokio::test]
async fn invoke_decodes_tool_calls_refusals_and_reasoning() {
    let (base, _server) = serve(vec![json_response(
        200,
        r#"{"choices": [{"message": {"role": "assistant", "content": null,
            "reasoning_content": "Need the weather.",
            "tool_calls": [
              {"id": "a", "type": "function", "function": {"name": "weather", "arguments": "{\"city\":\"Oslo\"}"}},
              {"id": "b", "type": "function", "function": {"name": "weather", "arguments": {"city": "Rome"}}},
              {"id": "c", "type": "function", "function": {"name": "weather", "arguments": "{\"city\":"}}
            ]}, "finish_reason": "tool_calls"}]}"#,
    )])
    .await;
    let response = model_at(&base, None)
        .invoke(&ModelRequest::new(vec![Message::user("weather?")]))
        .await
        .unwrap();
    let blocks = &response.message.content;
    assert_eq!(blocks[0].type_name(), "reasoning", "reasoning comes first");
    let cities: Vec<&str> = response
        .message
        .tool_calls()
        .map(|c| c.args["city"].as_str().unwrap())
        .collect();
    assert_eq!(cities, ["Oslo", "Rome"]);
    assert_eq!(blocks[3].type_name(), "invalid_tool_call");
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);

    let (base, _server) = serve(vec![json_response(
        200,
        r#"{"choices": [{"message": {"role": "assistant", "content": null, "refusal": "I can't help with that."}, "finish_reason": "stop"}]}"#,
    )])
    .await;
    let response = model_at(&base, None)
        .invoke(&ModelRequest::new(vec![Message::user("x")]))
        .await
        .unwrap();
    assert_eq!(response.finish_reason, FinishReason::ContentFilter);
    assert!(matches!(
        &response.message.content[0],
        ContentBlock::Text { extras, .. } if extras["openai"]["refusal"] == true
    ));
}

#[tokio::test]
async fn http_errors_are_classified_and_the_key_redacted() {
    let (base, _server) = serve(vec![json_response(
        401,
        r#"{"error": {"message": "Incorrect API key provided: sk-live-ABCDEFGHIJKLMNOP", "type": "invalid_request_error", "code": "invalid_api_key"}}"#,
    )])
    .await;
    let err = model_at(&base, Some("sk-live-ABCDEFGHIJKLMNOP"))
        .invoke(&ModelRequest::new(vec![Message::user("hi")]))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::Auth);
    assert_eq!(err.provider.as_deref(), Some("local"));
    assert_eq!(err.model.as_deref(), Some("test-model"));
    assert!(!err.message.contains("ABCDEFGH"), "{}", err.message);
}

#[tokio::test]
async fn bad_bodies_are_invalid_responses() {
    for body in ["not json", r#"{"choices": []}"#] {
        let (base, _server) = serve(vec![json_response(200, body)]).await;
        let err = model_at(&base, None)
            .invoke(&ModelRequest::new(vec![Message::user("hi")]))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ModelErrorKind::InvalidResponse, "{body}");
    }
    let (base, _server) = serve(vec![json_response(
        200,
        r#"{"error": {"message": "overloaded", "type": "overloaded_error"}}"#,
    )])
    .await;
    let err = model_at(&base, None)
        .invoke(&ModelRequest::new(vec![Message::user("hi")]))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::Overloaded);
}

async fn stream(
    base: &str,
    request: &ModelRequest,
) -> (
    Result<agentbuild_providers::ModelResponse, agentbuild_providers::ModelError>,
    Vec<MessageDelta>,
) {
    let model = model_at(base, None);
    let mut deltas = Vec::new();
    let mut sink = |delta: MessageDelta| deltas.push(delta);
    let result = model.stream(request, &mut sink).await;
    (result, deltas)
}

#[tokio::test]
async fn streamed_text_with_usage_matches_the_deltas() {
    let (base, server) = serve(vec![
        sse_head(),
        sse_data(r#"{"id":"c1","model":"m-1","choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}"#),
        sse_data(r#"{"id":"c1","choices":[{"index":0,"delta":{"content":"Hel"}}]}"#),
        Step::Wait(Duration::from_millis(30)),
        sse_data(r#"{"id":"c1","choices":[{"index":0,"delta":{"content":"lo"},"finish_reason":null}]}"#),
        sse_data(r#"{"id":"c1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#),
        sse_data(r#"{"id":"c1","choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2}}"#),
        Step::send("data: [DONE]\n\n"),
    ])
    .await;
    let request = ModelRequest::new(vec![Message::user("hi")]);
    let (result, deltas) = stream(&base, &request).await;
    let response = result.unwrap();
    let streamed: String = deltas
        .iter()
        .map(|d| match &d.delta {
            BlockDelta::Text { text } => text.as_str(),
            _ => "",
        })
        .collect();
    assert_eq!(streamed, "Hello");
    assert_eq!(response.message.text(), "Hello");
    assert!(deltas.iter().all(|d| d.message_id == response.message.id));
    assert_eq!(response.usage.output_tokens, 2);
    assert_eq!(response.message.metadata.model.as_deref(), Some("m-1"));
    let sent = server.await.unwrap().json();
    assert_eq!(sent["stream"], true);
}

#[tokio::test]
async fn streamed_parallel_tool_calls_are_assembled() {
    let (base, _server) = serve(vec![
        sse_head(),
        sse_data(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"weather","arguments":""}}]}}]}"#),
        sse_data(r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","type":"function","function":{"name":"weather","arguments":"{\"ci"}}]}}]}"#),
        sse_data(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\": \"Oslo\"}"}}]}}]}"#),
        sse_data(r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"ty\": \"Rome\"}"}}]}}]}"#),
        sse_data(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#),
        Step::send("data: [DONE]\n\n"),
    ])
    .await;
    let (result, _) = stream(&base, &ModelRequest::new(vec![Message::user("w")])).await;
    let response = result.unwrap();
    let calls: Vec<(&str, &str)> = response
        .message
        .tool_calls()
        .map(|c| (c.id, c.args["city"].as_str().unwrap()))
        .collect();
    assert_eq!(calls, [("call_a", "Oslo"), ("call_b", "Rome")]);
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
}

#[tokio::test]
async fn ollama_style_whole_tool_call_in_one_chunk() {
    let (base, _server) = serve(vec![
        sse_head(),
        sse_data(r#"{"choices":[{"delta":{"role":"assistant","content":"","tool_calls":[{"id":"call_x","function":{"name":"add","arguments":{"a":1,"b":2}}}]}}]}"#),
        sse_data(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#),
        Step::send("data: [DONE]\n\n"),
    ])
    .await;
    let (result, _) = stream(&base, &ModelRequest::new(vec![Message::user("1+2")])).await;
    let response = result.unwrap();
    let call = response.message.tool_calls().next().unwrap();
    assert_eq!((call.id, call.args["b"].as_i64()), ("call_x", Some(2)));
}

#[tokio::test]
async fn length_cut_while_calling_a_tool_is_truncated() {
    let (base, _server) = serve(vec![
        sse_head(),
        sse_data(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"write","arguments":"{\"text\": \"a long"}}]}}]}"#),
        sse_data(r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#),
        Step::send("data: [DONE]\n\n"),
    ])
    .await;
    let (result, _) = stream(&base, &ModelRequest::new(vec![Message::user("w")])).await;
    let response = result.unwrap();
    assert!(matches!(
        &response.message.content[0],
        ContentBlock::InvalidToolCall { error, .. } if error == "truncated"
    ));
    assert_eq!(response.finish_reason, FinishReason::Length);
}

#[tokio::test]
async fn stream_cut_before_finish_is_a_streaming_server_error() {
    let (base, _server) = serve(vec![
        sse_head(),
        sse_data(r#"{"choices":[{"delta":{"content":"partial"}}]}"#),
    ])
    .await;
    let (result, deltas) = stream(&base, &ModelRequest::new(vec![Message::user("w")])).await;
    let err = result.unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::Server);
    assert_eq!(err.dispatch, Dispatch::Streaming);
    assert!(err.emitted_deltas, "the UI must discard the partial text");
    assert_eq!(deltas.len(), 1);
}

#[tokio::test]
async fn in_band_error_mid_stream_is_classified() {
    let (base, _server) = serve(vec![
        sse_head(),
        sse_data(r#"{"choices":[{"delta":{"content":"Hi"}}]}"#),
        sse_data(r#"{"error":{"message":"Rate limit reached","type":"rate_limit_error"}}"#),
    ])
    .await;
    let (result, _) = stream(&base, &ModelRequest::new(vec![Message::user("w")])).await;
    let err = result.unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::RateLimited);
    assert_eq!(err.dispatch, Dispatch::Streaming);
    assert!(err.emitted_deltas);
    assert_eq!(err.provider.as_deref(), Some("local"));
}

#[tokio::test]
async fn presets_point_at_the_right_servers() {
    assert_eq!(Endpoint::ollama().base_url, "http://localhost:11434/v1");
    let gemini = Endpoint::gemini("key");
    assert_eq!(
        (gemini.name.as_str(), gemini.base_url.as_str()),
        (
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta/openai"
        )
    );
    assert_eq!(gemini.api_key.as_deref(), Some("key"));
    assert_eq!(Endpoint::ollama().api_key, None);
    assert_eq!(Endpoint::lmstudio().base_url, "http://localhost:1234/v1");
    assert_eq!(Endpoint::openai("k").base_url, "https://api.openai.com/v1");
    assert_eq!(
        Endpoint::openrouter("k").base_url,
        "https://openrouter.ai/api/v1"
    );
    let trailing = Endpoint::vllm("http://gpu:8000/v1/", None);
    assert_eq!(trailing.base_url, "http://gpu:8000/v1");
}

fn reasoning_of(message: &Message) -> Option<String> {
    message.content.iter().find_map(|block| match block {
        ContentBlock::Reasoning { text, .. } => text.clone(),
        _ => None,
    })
}

#[tokio::test]
async fn think_tags_become_reasoning_in_whole_and_streamed_answers() {
    // Whole answer (DeepSeek-R1 on Ollama 0.6 puts its reasoning inline).
    let (base, _server) = serve(vec![json_response(
        200,
        r#"{"choices": [{"message": {"role": "assistant",
            "content": "<think>\nThe user wants 2+2.\n</think>\n\nIt is 4."},
            "finish_reason": "stop"}]}"#,
    )])
    .await;
    let response = model_at(&base, None)
        .invoke(&ModelRequest::new(vec![Message::user("2+2?")]))
        .await
        .unwrap();
    assert_eq!(response.message.text(), "It is 4.");
    assert_eq!(
        reasoning_of(&response.message).as_deref(),
        Some("\nThe user wants 2+2.\n")
    );

    // Streamed, with the tags cut across chunks.
    let pieces = [
        "<th",
        "ink>\nThe user",
        " wants 2+2.\n</",
        "think",
        ">\n\nIt",
        " is 4.",
    ];
    let mut steps = vec![sse_head()];
    for piece in pieces {
        steps.push(sse_data(
            &json!({"choices": [{"index": 0, "delta": {"content": piece}}]}).to_string(),
        ));
    }
    steps.push(sse_data(
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
    ));
    steps.push(Step::send("data: [DONE]\n\n"));
    let (base, _server) = serve(steps).await;
    let request = ModelRequest::new(vec![Message::user("2+2?")]);
    let (result, deltas) = stream(&base, &request).await;
    let response = result.unwrap();
    let streamed_text: String = deltas
        .iter()
        .map(|d| match &d.delta {
            BlockDelta::Text { text } => text.as_str(),
            _ => "",
        })
        .collect();
    let streamed_reasoning: String = deltas
        .iter()
        .map(|d| match &d.delta {
            BlockDelta::Reasoning {
                text: Some(text), ..
            } => text.as_str(),
            _ => "",
        })
        .collect();
    assert_eq!(streamed_text, "It is 4.", "no tag reaches the answer");
    assert_eq!(streamed_reasoning, "\nThe user wants 2+2.\n");
    assert_eq!(response.message.text(), "It is 4.");
    assert_eq!(
        reasoning_of(&response.message).as_deref(),
        Some("\nThe user wants 2+2.\n")
    );
    // The reasoning block comes first, then the answer.
    assert!(matches!(
        response.message.content[0],
        ContentBlock::Reasoning { .. }
    ));
}

#[tokio::test]
async fn think_tags_can_be_left_in_the_text() {
    let (base, _server) = serve(vec![json_response(
        200,
        r#"{"choices": [{"message": {"role": "assistant",
            "content": "<think>x</think>Answer"}, "finish_reason": "stop"}]}"#,
    )])
    .await;
    let mut quirks = agentbuild_providers::OpenAiQuirks::default();
    quirks.think_tags = false;
    let mut endpoint = Endpoint::openai_compatible("local", base, None);
    endpoint.protocol = agentbuild_providers::Protocol::OpenAiChat(quirks);
    let model = ProviderModel::new(endpoint, "m").unwrap();
    let response = model
        .invoke(&ModelRequest::new(vec![Message::user("hi")]))
        .await
        .unwrap();
    assert_eq!(response.message.text(), "<think>x</think>Answer");
    assert_eq!(reasoning_of(&response.message), None);
}

// ---------- Gemini thought signatures (design 0006 §14) ----------

fn extras_of(block: &ContentBlock) -> Option<&Value> {
    match block {
        ContentBlock::ToolCall { extras, .. } => extras.get("local"),
        _ => None,
    }
}

#[tokio::test]
async fn tool_call_extra_content_is_kept_whole_and_streamed() {
    let signed = r#"{"google": {"thought_signature": "c2lnbmF0dXJl"}}"#;
    let (base, _server) = serve(vec![json_response(
        200,
        &format!(
            r#"{{"choices": [{{"message": {{"role": "assistant", "content": null, "tool_calls": [
                {{"id": "a", "type": "function", "function": {{"name": "weather", "arguments": "{{}}"}},
                  "extra_content": {signed}}},
                {{"id": "b", "type": "function", "function": {{"name": "weather", "arguments": "{{}}"}}}}
            ]}}, "finish_reason": "tool_calls"}}]}}"#
        ),
    )])
    .await;
    let response = model_at(&base, None)
        .invoke(&ModelRequest::new(vec![Message::user("weather?")]))
        .await
        .unwrap();
    let blocks = &response.message.content;
    assert_eq!(
        extras_of(&blocks[0]).unwrap()["extra_content"]["google"]["thought_signature"],
        "c2lnbmF0dXJl"
    );
    assert!(
        extras_of(&blocks[1]).is_none(),
        "only the first parallel call is signed"
    );

    let (base, _server) = serve(vec![
        sse_head(),
        sse_data(&format!(
            r#"{{"choices":[{{"delta":{{"tool_calls":[{{"index":0,"id":"a","type":"function","function":{{"name":"weather","arguments":""}},"extra_content":{signed}}}]}}}}]}}"#
        )),
        sse_data(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"}}]}}]}"#),
        sse_data(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#),
        Step::send("data: [DONE]\n\n"),
    ])
    .await;
    let (result, _) = stream(&base, &ModelRequest::new(vec![Message::user("w")])).await;
    let response = result.unwrap();
    assert_eq!(
        extras_of(&response.message.content[0]).unwrap()["extra_content"]["google"]["thought_signature"],
        "c2lnbmF0dXJl"
    );
}

#[test]
fn signatures_go_back_to_their_endpoint_and_gemini_gets_a_placeholder() {
    let signed = |provider: &str| {
        let mut call = ContentBlock::tool_call("a", "weather", Map::new());
        if let ContentBlock::ToolCall { extras, .. } = &mut call {
            extras.insert(
                provider.into(),
                json!({"extra_content": {"google": {"thought_signature": "real"}}}),
            );
        }
        call
    };
    let conversation = |calls: Vec<ContentBlock>| {
        let ids: Vec<String> = calls
            .iter()
            .filter_map(|c| match c {
                ContentBlock::ToolCall { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        let mut messages = vec![
            Message::user("weather?"),
            Message::with_content(Role::Assistant, calls),
        ];
        for id in ids {
            messages.push(Message::tool_result(
                id,
                "weather",
                ToolStatus::Success,
                vec![ContentBlock::text("sunny")],
            ));
        }
        ModelRequest::new(messages)
    };
    let gemini = Endpoint::gemini("test-key");
    let sent = |endpoint: &Endpoint, request: &ModelRequest| {
        body_for(endpoint, request).0["messages"][1]["tool_calls"].clone()
    };

    // The real signature goes back to Gemini, as received.
    let calls = sent(&gemini, &conversation(vec![signed("gemini")]));
    assert_eq!(
        calls[0]["extra_content"]["google"]["thought_signature"],
        "real"
    );
    // Not to another endpoint, which gets no placeholder either.
    let calls = sent(&compat(), &conversation(vec![signed("gemini")]));
    assert!(calls[0].get("extra_content").is_none());

    // History from another model: the placeholder, on the first call only.
    let unsigned = conversation(vec![
        ContentBlock::tool_call("a", "weather", Map::new()),
        ContentBlock::tool_call("b", "weather", Map::new()),
    ]);
    let calls = sent(&gemini, &unsigned);
    assert_eq!(
        calls[0]["extra_content"]["google"]["thought_signature"],
        "skip_thought_signature_validator"
    );
    assert!(calls[1].get("extra_content").is_none());
    assert!(sent(&compat(), &unsigned)[0].get("extra_content").is_none());
}
