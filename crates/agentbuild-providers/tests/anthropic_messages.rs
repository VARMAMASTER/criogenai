//! The Anthropic Messages protocol: golden request bodies (one per mapping
//! row of research litellm-providers §10.6), response decoding, streaming
//! (§10.7) and error classification, against a scripted local server.

#![cfg(feature = "http")]

#[path = "support/server.rs"]
mod server;

use std::time::Duration;

use agentbuild_providers::{
    AnthropicOptions, BlockDelta, ContentBlock, Dispatch, Endpoint, FinishReason, HttpConfig,
    MediaSource, Message, MessageDelta, Model, ModelError, ModelErrorKind, ModelRequest,
    ModelResponse, OutputSpec, OutputStrategy, ProviderModel, Reasoning, ReasoningEffort, Role,
    Settings, ToolChoice, ToolKind, ToolResultError, ToolSpec, ToolStatus, Usage, preview_request,
};
use serde_json::{Map, Value, json};
use server::{Step, json_response, serve, sse_head};

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

fn endpoint() -> Endpoint {
    Endpoint::anthropic("sk-ant-test")
}

fn body_for(request: &ModelRequest) -> (Value, Vec<String>) {
    let (body, warnings) = preview_request(&endpoint(), "claude-x", request, false).unwrap();
    (body, warnings.into_iter().map(|w| w.code).collect())
}

fn encode_error(request: &ModelRequest) -> ModelError {
    preview_request(&endpoint(), "claude-x", request, false).unwrap_err()
}

fn block(value: Value) -> ContentBlock {
    serde_json::from_value(value).unwrap()
}

fn search_tool() -> ToolSpec {
    ToolSpec::new(
        "search",
        "Search the docs",
        json!({"type": "object", "properties": {"q": {"type": "string"}}}),
    )
}

fn cached() -> Value {
    json!({"cache_control": {"type": "ephemeral"}})
}

// ---------- encode ----------

#[test]
fn conversation_with_reasoning_tools_and_cached_system_prompt() {
    let assistant = Message::with_content(
        Role::Assistant,
        vec![
            block(json!({"type": "reasoning", "text": "Need docs.", "signature": "sig1"})),
            ContentBlock::text("Let me look."),
            ContentBlock::tool_call("c1", "search", args(json!({"q": "rust"}))),
            ContentBlock::tool_call("c2", "search", args(json!({"q": "tokio"}))),
        ],
    );
    let request = ModelRequest::new(vec![
        Message::with_content(
            Role::System,
            vec![ContentBlock::text("Be brief.").with_extra("anthropic", cached())],
        ),
        Message::user("Find docs"),
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
        Message::tool_error(
            "c2",
            "search",
            ToolResultError::new("timeout", "took too long"),
        ),
        Message::user("Summarize."),
    ])
    .with_tools(vec![search_tool()]);
    let (body, warnings) = body_for(&request);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        body,
        json!({
            "model": "claude-x",
            "max_tokens": 8192,
            "system": [{"type": "text", "text": "Be brief.", "cache_control": {"type": "ephemeral"}}],
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "Find docs"}]},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "Need docs.", "signature": "sig1"},
                    {"type": "text", "text": "Let me look."},
                    {"type": "tool_use", "id": "c1", "name": "search", "input": {"q": "rust"}},
                    {"type": "tool_use", "id": "c2", "name": "search", "input": {"q": "tokio"}},
                ]},
                // Both results and the next user message: one user turn,
                // results first.
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "c1", "content": [
                        {"type": "text", "text": "2 hits"},
                        {"type": "text", "text": "{\"top\":\"doc.rust-lang.org\"}"},
                    ]},
                    {"type": "tool_result", "tool_use_id": "c2", "is_error": true, "content": [
                        {"type": "text", "text": "Error: took too long"},
                    ]},
                    {"type": "text", "text": "Summarize."},
                ]},
            ],
            "tools": [{
                "name": "search",
                "description": "Search the docs",
                "input_schema": {"type": "object", "properties": {"q": {"type": "string"}}},
            }],
        })
    );
}

#[test]
fn a_system_message_after_the_start_is_rejected_before_sending() {
    let request = ModelRequest::new(vec![
        Message::system("a"),
        Message::system("b"),
        Message::user("hi"),
        Message::system("late"),
    ]);
    let err = encode_error(&request);
    assert_eq!(err.kind, ModelErrorKind::InvalidRequest);
    assert!(
        err.message
            .contains("only takes system messages at the start")
    );
    // Several leading system messages are fine.
    let (body, _) = body_for(&ModelRequest::new(vec![
        Message::system("a"),
        Message::system("b"),
        Message::user("hi"),
    ]));
    assert_eq!(body["system"].as_array().unwrap().len(), 2);
}

#[test]
fn images_and_documents_by_bytes_url_and_file_id() {
    let user = Message::with_content(
        Role::User,
        vec![
            ContentBlock::text("Compare these."),
            ContentBlock::image(MediaSource::Base64 { data: "AAA".into() }, None),
            ContentBlock::image(
                MediaSource::Url {
                    url: "https://x.test/a.jpg".into(),
                },
                Some("image/jpeg".into()),
            ),
            ContentBlock::file(
                MediaSource::Base64 {
                    data: "JVBE".into(),
                },
                Some("application/pdf".into()),
            ),
            ContentBlock::file(
                MediaSource::FileId {
                    file_id: "file_1".into(),
                },
                None,
            ),
            ContentBlock::audio(
                MediaSource::Base64 {
                    data: "UklG".into(),
                },
                None,
            ),
        ],
    );
    let (body, warnings) = body_for(&ModelRequest::new(vec![user]));
    assert_eq!(warnings, ["content_dropped"], "audio is not supported");
    assert_eq!(
        body["messages"][0]["content"],
        json!([
            {"type": "text", "text": "Compare these."},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAA"}},
            {"type": "image", "source": {"type": "url", "url": "https://x.test/a.jpg"}},
            {"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": "JVBE"}},
            {"type": "document", "source": {"type": "file", "file_id": "file_1"}},
        ])
    );
}

#[test]
fn tool_results_carry_images() {
    let request = ModelRequest::new(vec![
        Message::user("Screenshot please"),
        Message::with_content(
            Role::Assistant,
            vec![ContentBlock::tool_call("c1", "shot", Map::new())],
        ),
        Message::tool_result(
            "c1",
            "shot",
            ToolStatus::Success,
            vec![ContentBlock::image(
                MediaSource::Base64 {
                    data: "iVBO".into(),
                },
                Some("image/png".into()),
            )],
        ),
    ]);
    let (body, warnings) = body_for(&request);
    assert!(warnings.is_empty());
    assert_eq!(
        body["messages"][2]["content"][0]["content"],
        json!([{"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBO"}}])
    );
}

#[test]
fn reasoning_and_provider_blocks_round_trip_only_when_they_can() {
    let assistant = Message::with_content(
        Role::Assistant,
        vec![
            block(json!({"type": "reasoning", "redacted": "ENC"})),
            block(json!({"type": "reasoning", "text": "from another provider"})),
            ContentBlock::provider(
                "anthropic",
                json!({"type": "server_tool_use", "id": "s1", "name": "web_search", "input": {"query": "x"}}),
            ),
            ContentBlock::provider("openai", json!({"type": "something"})),
            ContentBlock::text(""),
            ContentBlock::text("done"),
        ],
    );
    let (body, warnings) = body_for(&ModelRequest::new(vec![Message::user("q"), assistant]));
    assert_eq!(
        body["messages"][1]["content"],
        json!([
            {"type": "redacted_thinking", "data": "ENC"},
            {"type": "server_tool_use", "id": "s1", "name": "web_search", "input": {"query": "x"}},
            {"type": "text", "text": "done"},
        ])
    );
    assert_eq!(warnings, ["content_dropped", "reasoning_dropped"]);
}

#[test]
fn thinking_without_text_is_still_sent_back() {
    let assistant = Message::with_content(
        Role::Assistant,
        vec![
            block(json!({"type": "reasoning", "signature": "sig"})),
            ContentBlock::text("ok"),
        ],
    );
    let (body, _) = body_for(&ModelRequest::new(vec![Message::user("q"), assistant]));
    assert_eq!(
        body["messages"][1]["content"][0],
        json!({"type": "thinking", "thinking": "", "signature": "sig"})
    );
}

#[test]
fn an_invalid_tool_call_is_sent_as_a_call_so_its_result_stays_paired() {
    let assistant = Message::with_content(
        Role::Assistant,
        vec![
            ContentBlock::invalid_tool_call("c1", Some("search".into()), "{\"q\": ", "bad JSON"),
            ContentBlock::invalid_tool_call("c2", None, "{\"q\": 1}", "no name"),
        ],
    );
    let (body, _) = body_for(&ModelRequest::new(vec![Message::user("q"), assistant]));
    assert_eq!(
        body["messages"][1]["content"],
        json!([
            {"type": "tool_use", "id": "c1", "name": "search", "input": {}},
            {"type": "tool_use", "id": "c2", "name": "unknown", "input": {"q": 1}},
        ])
    );
}

#[test]
fn an_empty_user_message_is_rejected_and_an_empty_assistant_one_dropped() {
    let audio_only = Message::with_content(
        Role::User,
        vec![ContentBlock::audio(
            MediaSource::Base64 { data: "x".into() },
            None,
        )],
    );
    assert_eq!(
        encode_error(&ModelRequest::new(vec![audio_only])).kind,
        ModelErrorKind::InvalidRequest
    );
    let empty = Message::with_content(Role::Assistant, vec![ContentBlock::text("")]);
    let (body, warnings) = body_for(&ModelRequest::new(vec![
        Message::user("a"),
        empty,
        Message::user("b"),
    ]));
    assert_eq!(warnings, ["message_dropped"]);
    assert_eq!(
        body["messages"],
        json!([{"role": "user", "content": [
            {"type": "text", "text": "a"}, {"type": "text", "text": "b"},
        ]}])
    );
}

#[test]
fn tool_choice_and_parallel_calls() {
    let with = |choice: ToolChoice, parallel: Option<bool>| {
        let mut request =
            ModelRequest::new(vec![Message::user("q")]).with_tools(vec![search_tool()]);
        request.tool_choice = choice;
        request.settings.parallel_tool_calls = parallel;
        body_for(&request).0.get("tool_choice").cloned()
    };
    assert_eq!(with(ToolChoice::Auto, None), None);
    assert_eq!(with(ToolChoice::Auto, Some(true)), None);
    assert_eq!(
        with(ToolChoice::Auto, Some(false)),
        Some(json!({"type": "auto", "disable_parallel_tool_use": true}))
    );
    assert_eq!(
        with(ToolChoice::None, Some(false)),
        Some(json!({"type": "none"}))
    );
    assert_eq!(
        with(ToolChoice::Required, None),
        Some(json!({"type": "any"}))
    );
    assert_eq!(
        with(ToolChoice::Tool("search".into()), Some(false)),
        Some(json!({"type": "tool", "name": "search", "disable_parallel_tool_use": true}))
    );
    // No tools: no tool_choice.
    let mut request = ModelRequest::new(vec![Message::user("q")]);
    request.tool_choice = ToolChoice::Required;
    assert!(body_for(&request).0.get("tool_choice").is_none());
}

#[test]
fn hosted_tools_come_from_extras_and_hosted_specs_are_dropped() {
    let mut hosted = ToolSpec::new("web_search", "", json!({"type": "object"}));
    hosted.kind = ToolKind::Hosted;
    let mut request =
        ModelRequest::new(vec![Message::user("q")]).with_tools(vec![search_tool(), hosted]);
    request.extras.insert(
        "anthropic".into(),
        json!({
            "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 3}],
            "betas": ["some-beta-2026-01-01"],
            "top_k": 5,
        }),
    );
    let (body, warnings) = body_for(&request);
    assert_eq!(warnings, ["hosted_tool_dropped"]);
    let names: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["search", "web_search"]);
    assert_eq!(body["tools"][1]["type"], "web_search_20250305");
    assert_eq!(body["top_k"], 5);
    assert!(
        body.get("betas").is_none(),
        "betas are a header, not a field"
    );
}

#[test]
fn settings_map_to_their_fields() {
    let mut settings = Settings::default();
    settings.max_tokens = Some(500);
    settings.temperature = Some(0.2);
    settings.stop = vec!["END".into()];
    let (body, _) = body_for(&ModelRequest::new(vec![Message::user("q")]).with_settings(settings));
    assert_eq!(body["max_tokens"], 500);
    assert_eq!(body["temperature"], 0.2);
    assert_eq!(body["stop_sequences"], json!(["END"]));
    assert!(body.get("thinking").is_none());
    assert!(body.get("stream").is_none());

    let (streamed, _) = preview_request(
        &endpoint(),
        "m",
        &ModelRequest::new(vec![Message::user("q")]),
        true,
    )
    .unwrap();
    assert_eq!(streamed["stream"], true);

    let options = AnthropicOptions::default().default_max_tokens(1000);
    let small = endpoint().with_anthropic_options(options);
    let (body, _) = preview_request(
        &small,
        "m",
        &ModelRequest::new(vec![Message::user("q")]),
        false,
    )
    .unwrap();
    assert_eq!(body["max_tokens"], 1000);
}

fn reasoning(reasoning: Reasoning, max_tokens: Option<u32>) -> ModelRequest {
    let mut settings = Settings::default();
    settings.reasoning = Some(reasoning);
    settings.max_tokens = max_tokens;
    ModelRequest::new(vec![Message::user("q")]).with_settings(settings)
}

#[test]
fn reasoning_effort_uses_adaptive_thinking() {
    let (body, warnings) = body_for(&reasoning(
        Reasoning::Effort {
            effort: ReasoningEffort::High,
        },
        None,
    ));
    assert!(warnings.is_empty());
    assert_eq!(body["thinking"], json!({"type": "adaptive"}));
    assert_eq!(body["output_config"], json!({"effort": "high"}));
    // Room for the reasoning on top of the default.
    assert_eq!(body["max_tokens"], 8192 + 24_576);

    let (low, _) = body_for(&reasoning(
        Reasoning::Effort {
            effort: ReasoningEffort::Low,
        },
        Some(3000),
    ));
    assert_eq!(low["output_config"]["effort"], "low");
    assert_eq!(low["max_tokens"], 3000);
}

#[test]
fn a_reasoning_budget_uses_enabled_thinking() {
    let (body, _) = body_for(&reasoning(
        Reasoning::Budget {
            budget_tokens: 4000,
        },
        None,
    ));
    assert_eq!(
        body["thinking"],
        json!({"type": "enabled", "budget_tokens": 4000})
    );
    assert_eq!(body["max_tokens"], 8192 + 4000);
    assert!(body.get("output_config").is_none());

    let err = encode_error(&reasoning(
        Reasoning::Budget {
            budget_tokens: 4000,
        },
        Some(4000),
    ));
    assert_eq!(err.kind, ModelErrorKind::InvalidRequest);
    assert!(err.message.contains("larger than the reasoning budget"));
}

#[test]
fn native_output_shares_output_config_with_effort() {
    let schema = json!({"type": "object", "properties": {"a": {"type": "integer"}}});
    let mut request = reasoning(
        Reasoning::Effort {
            effort: ReasoningEffort::Medium,
        },
        None,
    );
    request.output = Some(OutputSpec::new(schema.clone()));
    let (body, _) = body_for(&request);
    assert_eq!(
        body["output_config"],
        json!({"effort": "medium", "format": {"type": "json_schema", "schema": schema}})
    );

    let mut tool = ModelRequest::new(vec![Message::user("q")]);
    tool.output = Some(OutputSpec::new(schema).with_strategy(OutputStrategy::Tool));
    assert!(body_for(&tool).0.get("output_config").is_none());
}

#[test]
fn extras_win_over_mapped_fields() {
    let mut request = reasoning(
        Reasoning::Effort {
            effort: ReasoningEffort::Low,
        },
        None,
    );
    request.extras.insert(
        "anthropic".into(),
        json!({"thinking": {"type": "adaptive", "display": "omitted"}, "metadata": {"user_id": "u1"}}),
    );
    let (body, _) = body_for(&request);
    assert_eq!(body["thinking"]["display"], "omitted");
    assert_eq!(body["metadata"]["user_id"], "u1");
}

#[test]
fn the_endpoint_preset_declares_what_anthropic_supports() {
    let caps = endpoint().capabilities;
    assert_eq!(caps.tools, Some(true));
    assert_eq!(caps.native_structured_output, Some(true));
    assert_eq!(caps.reasoning, Some(true));
    let compatible = Endpoint::anthropic_compatible("proxy", "http://p/v1/", None);
    assert_eq!(compatible.base_url, "http://p/v1");
    assert_eq!(compatible.capabilities.tools, None);
}

// ---------- end to end (decode included) ----------

fn model_at(base: &str) -> ProviderModel {
    let options = AnthropicOptions::default().beta("beta-a");
    let endpoint = Endpoint::anthropic_compatible("claude", base, Some("sk-ant-secret".into()))
        .with_anthropic_options(options);
    let config = HttpConfig::default()
        .connect_timeout(Duration::from_secs(2))
        .idle_timeout(Duration::from_secs(2))
        .total_timeout(Duration::from_secs(5));
    ProviderModel::with_config(endpoint, "claude-x", config).unwrap()
}

const FULL_RESPONSE: &str = r#"{
  "id": "msg_01", "type": "message", "role": "assistant", "model": "claude-x-2026",
  "content": [
    {"type": "thinking", "thinking": "Check the weather.", "signature": "SIG"},
    {"type": "redacted_thinking", "data": "ENC"},
    {"type": "text", "text": "Per the docs, ", "citations": [{"type": "char_location", "cited_text": "x"}]},
    {"type": "server_tool_use", "id": "srv_1", "name": "web_search", "input": {"query": "oslo"}},
    {"type": "web_search_tool_result", "tool_use_id": "srv_1", "content": []},
    {"type": "tool_use", "id": "toolu_1", "name": "weather", "input": {"city": "Oslo"}}
  ],
  "stop_reason": "tool_use", "stop_sequence": null,
  "usage": {"input_tokens": 10, "cache_read_input_tokens": 80, "cache_creation_input_tokens": 5,
            "output_tokens": 40, "output_tokens_details": {"thinking_tokens": 12}}
}"#;

#[tokio::test]
async fn invoke_decodes_every_block_type_in_order_and_sends_headers() {
    let (base, server) = serve(vec![json_response(200, FULL_RESPONSE)]).await;
    let mut request = ModelRequest::new(vec![Message::user("Weather in Oslo?")]);
    request
        .extras
        .insert("anthropic".into(), json!({"betas": ["beta-b", "beta-a"]}));
    let response = model_at(&base).invoke(&request).await.unwrap();

    let content = &response.message.content;
    let types: Vec<&str> = content.iter().map(ContentBlock::type_name).collect();
    assert_eq!(
        types,
        [
            "reasoning",
            "reasoning",
            "text",
            "provider",
            "provider",
            "tool_call"
        ]
    );
    let reasoning = serde_json::to_value(&content[0]).unwrap();
    assert_eq!(reasoning["text"], "Check the weather.");
    assert_eq!(reasoning["signature"], "SIG");
    assert_eq!(
        serde_json::to_value(&content[1]).unwrap()["redacted"],
        "ENC"
    );
    assert_eq!(
        content[2].extras().unwrap()["anthropic"]["citations"][0]["cited_text"],
        "x"
    );
    let call = response.message.tool_calls().next().unwrap();
    assert_eq!((call.id, call.name), ("toolu_1", "weather"));
    assert_eq!(call.args["city"], "Oslo");
    assert_eq!(
        response.message.tool_calls().count(),
        1,
        "hosted calls never run"
    );
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);

    let mut usage = Usage::new(95, 40);
    usage.cache_read_tokens = 80;
    usage.cache_write_tokens = 5;
    usage.reasoning_tokens = 12;
    assert_eq!(response.usage, usage);
    let meta = &response.message.metadata;
    assert_eq!(meta.model.as_deref(), Some("claude-x-2026"));
    assert_eq!(meta.provider.as_deref(), Some("claude"));
    assert_eq!(meta.raw_finish_reason.as_deref(), Some("tool_use"));
    assert_eq!(meta.other["provider_message_id"], "msg_01");
    assert!(response.message.validate().is_ok());

    let sent = server.await.unwrap();
    assert!(sent.head.starts_with("POST /v1/messages "), "{}", sent.head);
    assert_eq!(sent.header("x-api-key").as_deref(), Some("sk-ant-secret"));
    assert_eq!(
        sent.header("anthropic-version").as_deref(),
        Some("2023-06-01")
    );
    assert_eq!(
        sent.header("anthropic-beta").as_deref(),
        Some("beta-a,beta-b")
    );
    assert!(sent.header("authorization").is_none());
    assert_eq!(sent.json()["model"], "claude-x");
}

#[tokio::test]
async fn a_decoded_answer_is_sent_back_unchanged() {
    let (base, _server) = serve(vec![json_response(200, FULL_RESPONSE)]).await;
    let response = model_at(&base)
        .invoke(&ModelRequest::new(vec![Message::user("q")]))
        .await
        .unwrap();
    let next = ModelRequest::new(vec![
        Message::user("q"),
        response.message,
        Message::tool_result(
            "toolu_1",
            "weather",
            ToolStatus::Success,
            vec![ContentBlock::text("21 C")],
        ),
    ]);
    let (body, warnings) = body_for(&next);
    assert!(warnings.is_empty(), "{warnings:?}");
    let original: Value = serde_json::from_str(FULL_RESPONSE).unwrap();
    assert_eq!(body["messages"][1]["content"], original["content"]);
}

#[tokio::test]
async fn stop_reasons_and_the_stop_sequence() {
    for (raw, reason) in [
        ("end_turn", FinishReason::Stop),
        ("stop_sequence", FinishReason::Stop),
        ("max_tokens", FinishReason::Length),
        ("model_context_window_exceeded", FinishReason::Length),
        ("refusal", FinishReason::ContentFilter),
        ("pause_turn", FinishReason::Other),
    ] {
        let body = json!({
            "id": "m", "model": "x", "content": [{"type": "text", "text": "hi"}],
            "stop_reason": raw, "stop_sequence": (raw == "stop_sequence").then_some("END"),
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })
        .to_string();
        let (base, _server) = serve(vec![json_response(200, &body)]).await;
        let response = model_at(&base)
            .invoke(&ModelRequest::new(vec![Message::user("q")]))
            .await
            .unwrap();
        assert_eq!(response.finish_reason, reason, "{raw}");
        let sequence = response.message.metadata.other.get("stop_sequence");
        assert_eq!(sequence.is_some(), raw == "stop_sequence", "{raw}");
    }
}

#[tokio::test]
async fn a_tool_call_cut_by_the_token_limit_is_truncated() {
    let body = r#"{"id": "m", "model": "x", "content": [
        {"type": "tool_use", "id": "a", "name": "t", "input": {"x": 1}},
        {"type": "tool_use", "id": "b", "name": "t", "input": {}}
    ], "stop_reason": "max_tokens", "usage": {"input_tokens": 1, "output_tokens": 1}}"#;
    let (base, _server) = serve(vec![json_response(200, body)]).await;
    let response = model_at(&base)
        .invoke(&ModelRequest::new(vec![Message::user("q")]))
        .await
        .unwrap();
    let content = &response.message.content;
    assert_eq!(content[0].type_name(), "tool_call");
    let cut = serde_json::to_value(&content[1]).unwrap();
    assert_eq!(cut["type"], "invalid_tool_call");
    assert_eq!(cut["error"], "truncated");
}

#[tokio::test]
async fn http_errors_are_classified_and_the_key_never_leaks() {
    let cases = [
        (
            529,
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
            ModelErrorKind::Overloaded,
        ),
        (
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 210000 tokens > 200000 maximum"}}"#,
            ModelErrorKind::ContextOverflow,
        ),
        (
            401,
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key sk-ant-secret"}}"#,
            ModelErrorKind::Auth,
        ),
        (
            500,
            r#"{"type":"error","error":{"type":"api_error","message":"Internal server error"}}"#,
            ModelErrorKind::Server,
        ),
    ];
    for (status, body, kind) in cases {
        let (base, _server) = serve(vec![json_response(status, body)]).await;
        let err = model_at(&base)
            .invoke(&ModelRequest::new(vec![Message::user("q")]))
            .await
            .unwrap_err();
        assert_eq!(err.kind, kind, "{status}");
        assert_eq!(err.provider.as_deref(), Some("claude"));
        assert!(!err.message.contains("sk-ant-secret"), "{}", err.message);
    }
}

// ---------- streaming ----------

/// One SSE event the way Anthropic sends it: an `event:` line naming the
/// type, then the JSON data.
fn event(data: Value) -> Step {
    let name = data["type"].as_str().unwrap().to_string();
    Step::send(format!("event: {name}\ndata: {data}\n\n"))
}

fn start(index: u32, block: Value) -> Step {
    event(json!({"type": "content_block_start", "index": index, "content_block": block}))
}

fn delta(index: u32, delta: Value) -> Step {
    event(json!({"type": "content_block_delta", "index": index, "delta": delta}))
}

fn stop(index: u32) -> Step {
    event(json!({"type": "content_block_stop", "index": index}))
}

fn message_start(usage: Value) -> Step {
    event(json!({"type": "message_start", "message": {
        "id": "msg_s", "type": "message", "role": "assistant", "model": "claude-x-2026",
        "content": [], "stop_reason": null, "usage": usage
    }}))
}

fn message_delta(stop_reason: &str, usage: Value) -> Step {
    event(json!({"type": "message_delta",
        "delta": {"stop_reason": stop_reason, "stop_sequence": null}, "usage": usage}))
}

fn message_stop() -> Step {
    event(json!({"type": "message_stop"}))
}

async fn stream(
    base: &str,
    request: &ModelRequest,
) -> (Result<ModelResponse, ModelError>, Vec<MessageDelta>) {
    let model = model_at(base);
    let mut deltas = Vec::new();
    let mut sink = |delta: MessageDelta| deltas.push(delta);
    let result = model.stream(request, &mut sink).await;
    (result, deltas)
}

#[tokio::test]
async fn a_streamed_answer_with_reasoning_citations_and_a_tool_call() {
    let (base, server) = serve(vec![
        sse_head(),
        message_start(
            json!({"input_tokens": 10, "cache_read_input_tokens": 90, "output_tokens": 1}),
        ),
        start(
            0,
            json!({"type": "thinking", "thinking": "", "signature": ""}),
        ),
        delta(0, json!({"type": "thinking_delta", "thinking": "Let me "})),
        delta(0, json!({"type": "thinking_delta", "thinking": "think."})),
        delta(0, json!({"type": "signature_delta", "signature": "SIG"})),
        stop(0),
        event(json!({"type": "ping"})),
        start(1, json!({"type": "text", "text": "", "citations": []})),
        delta(1, json!({"type": "text_delta", "text": "Oslo is "})),
        delta(
            1,
            json!({"type": "citations_delta", "citation": {"cited_text": "a"}}),
        ),
        Step::Wait(Duration::from_millis(20)),
        delta(1, json!({"type": "text_delta", "text": "cold."})),
        stop(1),
        start(
            2,
            json!({"type": "tool_use", "id": "toolu_1", "name": "weather", "input": {}}),
        ),
        delta(2, json!({"type": "input_json_delta", "partial_json": ""})),
        delta(
            2,
            json!({"type": "input_json_delta", "partial_json": "{\"city\": \"Os"}),
        ),
        delta(
            2,
            json!({"type": "input_json_delta", "partial_json": "lo\"}"}),
        ),
        stop(2),
        event(json!({"type": "some_future_event", "x": 1})),
        message_delta("tool_use", json!({"output_tokens": 55})),
        message_stop(),
    ])
    .await;
    let (result, deltas) = stream(&base, &ModelRequest::new(vec![Message::user("q")])).await;
    let response = result.unwrap();

    let streamed: String = deltas
        .iter()
        .filter_map(|d| match &d.delta {
            BlockDelta::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(streamed, "Oslo is cold.");
    assert!(deltas.iter().all(|d| d.message_id == response.message.id));

    let content = &response.message.content;
    let reasoning = serde_json::to_value(&content[0]).unwrap();
    assert_eq!(reasoning["text"], "Let me think.");
    assert_eq!(reasoning["signature"], "SIG");
    assert_eq!(content[1].type_name(), "text");
    assert_eq!(
        content[1].extras().unwrap()["anthropic"]["citations"],
        json!([{"cited_text": "a"}])
    );
    let call = response.message.tool_calls().next().unwrap();
    assert_eq!(
        (call.id, call.args["city"].as_str()),
        ("toolu_1", Some("Oslo"))
    );
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
    assert_eq!(response.usage.input_tokens, 100);
    assert_eq!(response.usage.cache_read_tokens, 90);
    assert_eq!(response.usage.output_tokens, 55);
    assert_eq!(
        response.message.metadata.model.as_deref(),
        Some("claude-x-2026")
    );
    assert!(response.warnings.is_empty(), "{:?}", response.warnings);

    let sent = server.await.unwrap();
    assert_eq!(sent.json()["stream"], true);
    assert_eq!(sent.header("accept").as_deref(), Some("text/event-stream"));
}

#[tokio::test]
async fn streamed_hosted_tool_blocks_are_kept_whole() {
    let (base, _server) = serve(vec![
        sse_head(),
        message_start(json!({"input_tokens": 5, "output_tokens": 1})),
        start(0, json!({"type": "server_tool_use", "id": "srv_1", "name": "web_search", "input": {}})),
        delta(0, json!({"type": "input_json_delta", "partial_json": "{\"query\": "})),
        delta(0, json!({"type": "input_json_delta", "partial_json": "\"oslo\"}"})),
        stop(0),
        start(1, json!({"type": "web_search_tool_result", "tool_use_id": "srv_1", "content": [{"type": "web_search_result", "url": "https://x.test"}]})),
        stop(1),
        start(2, json!({"type": "text", "text": "Found it."})),
        stop(2),
        message_delta("end_turn", json!({"output_tokens": 9})),
        message_stop(),
    ])
    .await;
    let (result, _) = stream(&base, &ModelRequest::new(vec![Message::user("q")])).await;
    let response = result.unwrap();
    let content: Vec<Value> = response
        .message
        .content
        .iter()
        .map(|b| serde_json::to_value(b).unwrap())
        .collect();
    assert_eq!(
        content[0]["data"],
        json!({"type": "server_tool_use", "id": "srv_1", "name": "web_search", "input": {"query": "oslo"}})
    );
    assert_eq!(content[1]["data"]["content"][0]["url"], "https://x.test");
    assert_eq!(response.message.text(), "Found it.");
    assert_eq!(response.message.tool_calls().count(), 0);
}

#[tokio::test]
async fn a_streamed_tool_call_cut_by_the_token_limit_is_truncated() {
    let (base, _server) = serve(vec![
        sse_head(),
        message_start(json!({"input_tokens": 5, "output_tokens": 1})),
        start(
            0,
            json!({"type": "tool_use", "id": "a", "name": "t", "input": {}}),
        ),
        delta(0, json!({"type": "input_json_delta", "partial_json": "{}"})),
        stop(0),
        start(
            1,
            json!({"type": "tool_use", "id": "b", "name": "t", "input": {}}),
        ),
        delta(
            1,
            json!({"type": "input_json_delta", "partial_json": "{\"long\": \"abc"}),
        ),
        stop(1),
        message_delta("max_tokens", json!({"output_tokens": 100})),
        message_stop(),
    ])
    .await;
    let (result, _) = stream(&base, &ModelRequest::new(vec![Message::user("q")])).await;
    let response = result.unwrap();
    assert_eq!(response.finish_reason, FinishReason::Length);
    let content = &response.message.content;
    assert_eq!(content[0].type_name(), "tool_call");
    let cut = serde_json::to_value(&content[1]).unwrap();
    assert_eq!(cut["error"], "truncated");
    assert_eq!(cut["raw_args"], "{\"long\": \"abc");
}

#[tokio::test]
async fn a_finished_tool_call_before_a_cut_text_block_stays_valid() {
    let (base, _server) = serve(vec![
        sse_head(),
        message_start(json!({"input_tokens": 5, "output_tokens": 1})),
        start(
            0,
            json!({"type": "tool_use", "id": "a", "name": "t", "input": {}}),
        ),
        delta(
            0,
            json!({"type": "input_json_delta", "partial_json": "{\"x\": 1}"}),
        ),
        stop(0),
        start(1, json!({"type": "text", "text": ""})),
        delta(1, json!({"type": "text_delta", "text": "and then"})),
        stop(1),
        message_delta("max_tokens", json!({"output_tokens": 100})),
        message_stop(),
    ])
    .await;
    let (result, _) = stream(&base, &ModelRequest::new(vec![Message::user("q")])).await;
    let response = result.unwrap();
    let call = response.message.tool_calls().next().unwrap();
    assert_eq!(call.args["x"], 1);
}

#[tokio::test]
async fn an_error_event_mid_stream_is_classified_as_streaming() {
    for (kind_on_wire, kind) in [
        ("overloaded_error", ModelErrorKind::Overloaded),
        ("api_error", ModelErrorKind::Server),
        ("rate_limit_error", ModelErrorKind::RateLimited),
    ] {
        let (base, _server) = serve(vec![
            sse_head(),
            message_start(json!({"input_tokens": 5, "output_tokens": 1})),
            start(0, json!({"type": "text", "text": ""})),
            delta(0, json!({"type": "text_delta", "text": "Hel"})),
            event(json!({"type": "error", "error": {"type": kind_on_wire, "message": "busy"}})),
        ])
        .await;
        let (result, deltas) = stream(&base, &ModelRequest::new(vec![Message::user("q")])).await;
        let err = result.unwrap_err();
        assert_eq!(err.kind, kind, "{kind_on_wire}");
        assert_eq!(err.dispatch, Dispatch::Streaming);
        assert!(err.emitted_deltas);
        assert_eq!(err.status, None);
        assert_eq!(err.provider_code.as_deref(), Some(kind_on_wire));
        assert_eq!(deltas.len(), 1);
    }
}

#[tokio::test]
async fn a_stream_that_stops_without_a_stop_reason_is_a_server_error() {
    let (base, _server) = serve(vec![
        sse_head(),
        message_start(json!({"input_tokens": 5, "output_tokens": 1})),
        start(0, json!({"type": "text", "text": ""})),
        delta(0, json!({"type": "text_delta", "text": "Hel"})),
    ])
    .await;
    let (result, _) = stream(&base, &ModelRequest::new(vec![Message::user("q")])).await;
    let err = result.unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::Server);
    assert_eq!(err.dispatch, Dispatch::Streaming);
}

#[test]
fn an_error_result_shows_its_error_even_with_other_content() {
    let mut result = Message::tool_result(
        "c1",
        "search",
        ToolStatus::Error,
        vec![ContentBlock::text("partial output")],
    );
    result.error = Some(ToolResultError::new("timeout", "took too long"));
    let request = ModelRequest::new(vec![
        Message::user("q"),
        Message::with_content(
            Role::Assistant,
            vec![ContentBlock::tool_call("c1", "search", Map::new())],
        ),
        result,
    ]);
    let (body, _) = body_for(&request);
    assert_eq!(
        body["messages"][2]["content"][0]["content"],
        json!([
            {"type": "text", "text": "Error: took too long"},
            {"type": "text", "text": "partial output"},
        ])
    );
}

#[tokio::test]
async fn a_streamed_stop_sequence_is_kept() {
    let (base, _server) = serve(vec![
        sse_head(),
        message_start(json!({"input_tokens": 5, "output_tokens": 1})),
        start(0, json!({"type": "text", "text": "one two"})),
        stop(0),
        event(json!({"type": "message_delta",
            "delta": {"stop_reason": "stop_sequence", "stop_sequence": "END"},
            "usage": {"output_tokens": 3}})),
        message_stop(),
    ])
    .await;
    let (result, _) = stream(&base, &ModelRequest::new(vec![Message::user("q")])).await;
    let response = result.unwrap();
    assert_eq!(response.finish_reason, FinishReason::Stop);
    assert_eq!(response.message.text(), "one two");
    assert_eq!(response.message.metadata.other["stop_sequence"], "END");
}

#[tokio::test]
async fn reading_ends_at_message_stop_even_if_the_connection_stays_open() {
    let (base, _server) = serve(vec![
        sse_head(),
        message_start(json!({"input_tokens": 5, "output_tokens": 1})),
        start(0, json!({"type": "text", "text": "done"})),
        stop(0),
        message_delta("end_turn", json!({"output_tokens": 1})),
        message_stop(),
        // Longer than the 2 s idle timeout: reading on would fail.
        Step::Wait(Duration::from_secs(4)),
    ])
    .await;
    let (result, _) = stream(&base, &ModelRequest::new(vec![Message::user("q")])).await;
    assert_eq!(result.unwrap().message.text(), "done");
}
