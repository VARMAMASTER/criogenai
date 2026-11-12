//! A tool-calling agent over the real Anthropic wire format: `ProviderModel`
//! talks to a scripted local server (no network, no real model), so the
//! whole path runs: encode, HTTP, SSE, decode, tools, and the next request.

#![cfg(feature = "http")]

#[path = "../../criogenai-providers/tests/support/server.rs"]
mod server;

use std::time::Duration;

use criogenai_agent::{Agent, StopReason, ToolError, tool_fn};
use criogenai_providers::{Endpoint, HttpConfig, ProviderModel, Role, ToolSpec};
use serde_json::{Value, json};
use server::{Step, serve_each, sse_head};

fn event(data: Value) -> Step {
    let name = data["type"].as_str().unwrap().to_string();
    Step::send(format!("event: {name}\ndata: {data}\n\n"))
}

fn message_start(input_tokens: u64) -> Step {
    event(json!({"type": "message_start", "message": {
        "id": "msg", "type": "message", "role": "assistant", "model": "claude-test",
        "content": [], "usage": {"input_tokens": input_tokens, "output_tokens": 1}
    }}))
}

fn block(index: u32, start: Value, deltas: Vec<Value>) -> Vec<Step> {
    let mut steps = vec![event(
        json!({"type": "content_block_start", "index": index, "content_block": start}),
    )];
    steps.extend(deltas.into_iter().map(|delta| {
        event(json!({"type": "content_block_delta", "index": index, "delta": delta}))
    }));
    steps.push(event(json!({"type": "content_block_stop", "index": index})));
    steps
}

fn finish(stop_reason: &str, output_tokens: u64) -> Vec<Step> {
    vec![
        event(json!({"type": "message_delta",
            "delta": {"stop_reason": stop_reason}, "usage": {"output_tokens": output_tokens}})),
        event(json!({"type": "message_stop"})),
    ]
}

fn tool_use(index: u32, id: &str, city: &str) -> Vec<Step> {
    block(
        index,
        json!({"type": "tool_use", "id": id, "name": "weather", "input": {}}),
        vec![
            json!({"type": "input_json_delta", "partial_json": "{\"city\": "}),
            json!({"type": "input_json_delta", "partial_json": format!("\"{city}\"}}")}),
        ],
    )
}

#[tokio::test]
async fn a_weather_agent_runs_parallel_tools_over_the_anthropic_wire() {
    // Turn 1: reasoning, a sentence, two tool calls.
    let mut first = vec![sse_head(), message_start(100)];
    first.extend(block(
        0,
        json!({"type": "thinking", "thinking": "", "signature": ""}),
        vec![
            json!({"type": "thinking_delta", "thinking": "Two cities."}),
            json!({"type": "signature_delta", "signature": "SIG"}),
        ],
    ));
    first.extend(block(
        1,
        json!({"type": "text", "text": ""}),
        vec![json!({"type": "text_delta", "text": "Checking both."})],
    ));
    first.extend(tool_use(2, "toolu_oslo", "Oslo"));
    first.extend(tool_use(3, "toolu_rome", "Rome"));
    first.extend(finish("tool_use", 40));
    // Turn 2: the answer.
    let mut second = vec![sse_head(), message_start(180)];
    second.extend(block(
        0,
        json!({"type": "text", "text": ""}),
        vec![json!({"type": "text_delta", "text": "Oslo 4 °C, Rome 21 °C."})],
    ));
    second.extend(finish("end_turn", 12));
    let (base, server) = serve_each(vec![first, second]).await;

    let endpoint = Endpoint::anthropic_compatible("claude", base, Some("sk-ant-test".into()));
    let config = HttpConfig::default()
        .connect_timeout(Duration::from_secs(2))
        .total_timeout(Duration::from_secs(10));
    let model = ProviderModel::with_config(endpoint, "claude-test", config).unwrap();
    let weather = tool_fn(
        ToolSpec::new(
            "weather",
            "Current weather",
            json!({"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}),
        ),
        |args, _| async move {
            let celsius = if args["city"] == "Oslo" { 4 } else { 21 };
            Ok::<_, ToolError>(json!({"celsius": celsius}))
        },
    );
    let agent = Agent::builder(model)
        .instructions("Be brief.")
        .tool(weather)
        .build()
        .unwrap();

    let result = agent.run("Weather in Oslo and Rome?").await.unwrap();
    assert_eq!(result.text(), Some("Oslo 4 °C, Rome 21 °C."));
    assert_eq!(result.stop_reason, Some(StopReason::FinalAnswer));
    assert_eq!((result.model_calls, result.tool_calls), (2, 2));
    assert_eq!(result.usage.input_tokens, 280);
    assert_eq!(result.usage.output_tokens, 52);
    let roles: Vec<Role> = result.messages.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        [
            Role::User,
            Role::Assistant,
            Role::Tool,
            Role::Tool,
            Role::Assistant
        ]
    );

    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert!(request.head.starts_with("POST /v1/messages "));
        assert_eq!(request.header("x-api-key").as_deref(), Some("sk-ant-test"));
        assert_eq!(request.json()["stream"], true);
    }
    let first_body = requests[0].json();
    assert_eq!(
        first_body["system"],
        json!([{"type": "text", "text": "Be brief."}])
    );
    assert_eq!(first_body["tools"][0]["name"], "weather");

    // The second request sends the first answer back unchanged (signed
    // reasoning included) and both results in one user turn, in call order.
    let second_body = requests[1].json();
    assert_eq!(
        second_body["messages"],
        json!([
            {"role": "user", "content": [{"type": "text", "text": "Weather in Oslo and Rome?"}]},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "Two cities.", "signature": "SIG"},
                {"type": "text", "text": "Checking both."},
                {"type": "tool_use", "id": "toolu_oslo", "name": "weather", "input": {"city": "Oslo"}},
                {"type": "tool_use", "id": "toolu_rome", "name": "weather", "input": {"city": "Rome"}},
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_oslo",
                 "content": [{"type": "text", "text": "{\"celsius\":4}"}]},
                {"type": "tool_result", "tool_use_id": "toolu_rome",
                 "content": [{"type": "text", "text": "{\"celsius\":21}"}]},
            ]},
        ])
    );
}
