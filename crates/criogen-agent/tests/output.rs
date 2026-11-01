//! Structured output (design 0004 §6), with `FakeModel` on the paused clock.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use criogen_agent::{Agent, Approval, FINAL_OUTPUT_TOOL, Limits, StopReason, ToolError, tool_fn};
use criogen_providers::{
    FakeModel, FakeReply, OutputStrategy, Role, ToolSpec, ToolStatus, transcript,
};
use serde_json::{Value, json};

fn person() -> Value {
    json!({
        "type": "object",
        "properties": {"name": {"type": "string"}, "age": {"type": "integer"}},
        "required": ["name", "age"],
        "additionalProperties": false
    })
}

fn fake(replies: Vec<FakeReply>) -> Arc<FakeModel> {
    let mut model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO);
    for reply in replies {
        model = model.then(reply);
    }
    Arc::new(model)
}

fn counter_tool(name: &'static str, runs: &Arc<AtomicUsize>) -> impl criogen_agent::Tool {
    let runs = runs.clone();
    tool_fn(
        ToolSpec::new(name, "", json!({"type": "object"})),
        move |_, _| {
            let runs = runs.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ToolError>("Ada, 36, mathematician")
            }
        },
    )
}

fn errors(messages: &[criogen_providers::Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|m| m.error.as_ref().map(|e| e.message.clone()))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn the_tool_strategy_ends_with_validated_output() {
    let runs = Arc::new(AtomicUsize::new(0));
    let model = fake(vec![
        FakeReply::tool_call("lookup", json!({})),
        FakeReply::tool_call(FINAL_OUTPUT_TOOL, json!({"name": "Ada", "age": 36})),
    ]);
    let agent = Agent::builder_shared(model.clone())
        .tool(counter_tool("lookup", &runs))
        .output(person())
        .build()
        .unwrap();
    let result = agent.run("Who was Ada Lovelace?").await.unwrap();
    assert_eq!(result.stop_reason, Some(StopReason::Output));
    assert_eq!(result.output, Some(json!({"name": "Ada", "age": 36})));
    assert_eq!(result.model_calls, 2);
    assert!(transcript::check(&result.messages).is_empty());
    let request = &model.requests()[0];
    let tool_names: Vec<&str> = request.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(tool_names, ["lookup", FINAL_OUTPUT_TOOL]);
    assert_eq!(request.tools[1].input_schema, person());
    assert!(request.output.is_none(), "tool strategy: no JSON mode");
}

#[tokio::test(start_paused = true)]
async fn invalid_output_goes_back_to_the_model() {
    let model = fake(vec![
        FakeReply::tool_call(FINAL_OUTPUT_TOOL, json!({"name": "Ada", "age": "36"})),
        FakeReply::tool_call(FINAL_OUTPUT_TOOL, json!({"name": "Ada", "age": 36})),
    ]);
    let agent = Agent::builder_shared(model.clone())
        .output(person())
        .build()
        .unwrap();
    let result = agent.run("x").await.unwrap();
    assert_eq!(result.output, Some(json!({"name": "Ada", "age": 36})));
    assert_eq!(
        errors(&result.messages),
        ["Invalid final output: age: expected integer, got \"36\""]
    );
    // The second request carried the error back to the model.
    assert_eq!(
        model.requests()[1]
            .messages
            .iter()
            .filter(|m| m.error.is_some())
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn other_calls_in_the_final_turn_are_skipped() {
    let runs = Arc::new(AtomicUsize::new(0));
    for final_args in [json!({"name": "Ada", "age": 36}), json!({"name": "Ada"})] {
        let valid = final_args.get("age").is_some();
        let model = fake(vec![
            FakeReply::tool_calls([("send_email", json!({})), (FINAL_OUTPUT_TOOL, final_args)]),
            FakeReply::tool_call(FINAL_OUTPUT_TOOL, json!({"name": "Ada", "age": 36})),
        ]);
        let agent = Agent::builder_shared(model)
            .tool(counter_tool("send_email", &runs))
            .output(person())
            .build()
            .unwrap();
        let result = agent.run("x").await.unwrap();
        let first_turn: Vec<ToolStatus> = result
            .messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .take(2)
            .map(|m| m.status.unwrap())
            .collect();
        let expected_final = if valid {
            ToolStatus::Success
        } else {
            ToolStatus::Error
        };
        assert_eq!(first_turn, [ToolStatus::Skipped, expected_final]);
        assert_eq!(result.stop_reason, Some(StopReason::Output));
    }
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "no side effects after the final answer"
    );
}

#[tokio::test(start_paused = true)]
async fn a_plain_text_answer_gets_a_reminder() {
    let model = fake(vec![
        FakeReply::text("Ada Lovelace, 36."),
        FakeReply::tool_call(FINAL_OUTPUT_TOOL, json!({"name": "Ada", "age": 36})),
    ]);
    let agent = Agent::builder_shared(model.clone())
        .output(person())
        .build()
        .unwrap();
    let result = agent.run("x").await.unwrap();
    assert_eq!(result.stop_reason, Some(StopReason::Output));
    let reminder = &model.requests()[1].messages.last().unwrap().clone();
    assert_eq!(reminder.role, Role::User);
    assert_eq!(
        reminder.text(),
        "Give your final answer by calling the `final_output` tool."
    );
}

#[tokio::test(start_paused = true)]
async fn exhausted_retries_fail_with_output_invalid() {
    let model = fake(vec![
        FakeReply::text("no"),
        FakeReply::text("still no"),
        FakeReply::tool_call(FINAL_OUTPUT_TOOL, json!({"age": 1})),
    ]);
    let agent = Agent::builder_shared(model)
        .output(person())
        .limits(Limits::default().output_retries(2))
        .build()
        .unwrap();
    let err = agent.run("x").await.unwrap_err();
    assert_eq!(err.code, "output_invalid");
    let details = err.details.unwrap();
    assert!(
        details["problem"]
            .as_str()
            .unwrap()
            .contains("name: required")
    );
    assert_eq!(details["raw"], r#"{"age":1}"#);
}

#[tokio::test(start_paused = true)]
async fn the_native_strategy_parses_the_answer_text() {
    let model = fake(vec![
        FakeReply::text("Here you go: Ada"),
        FakeReply::text("```json\n{\"name\": \"Ada\", \"age\": 36}\n```"),
    ]);
    let agent = Agent::builder_shared(model.clone())
        .output_with(person(), OutputStrategy::Native)
        .build()
        .unwrap();
    let result = agent.run("x").await.unwrap();
    assert_eq!(result.output, Some(json!({"name": "Ada", "age": 36})));
    let first = &model.requests()[0];
    assert!(first.tools.is_empty(), "no final_output tool in JSON mode");
    let output = first.output.as_ref().unwrap();
    assert_eq!(output.schema, person());
    assert_eq!(output.strategy, Some(OutputStrategy::Native));
    let reminder = model.requests()[1].messages.last().unwrap().text();
    assert!(reminder.starts_with(
        "Your answer does not match the required format: the answer is not valid JSON"
    ));
}

#[tokio::test(start_paused = true)]
async fn non_object_schemas_are_wrapped_for_the_tool() {
    let tags = json!({"type": "array", "items": {"type": "string"}, "maxItems": 3});
    let model = fake(vec![FakeReply::tool_call(
        FINAL_OUTPUT_TOOL,
        json!({"value": ["rust", "agents"]}),
    )]);
    let agent = Agent::builder_shared(model.clone())
        .output(tags.clone())
        .build()
        .unwrap();
    let result = agent.run("tags?").await.unwrap();
    assert_eq!(result.output, Some(json!(["rust", "agents"])));
    assert_eq!(
        model.requests()[0].tools[0].input_schema["properties"]["value"],
        tags
    );
}

#[tokio::test(start_paused = true)]
async fn final_output_is_never_gated_by_approval() {
    let runs = Arc::new(AtomicUsize::new(0));
    let model = fake(vec![FakeReply::tool_call(
        FINAL_OUTPUT_TOOL,
        json!({"name": "Ada", "age": 36}),
    )]);
    let agent = Agent::builder_shared(model)
        .tool(counter_tool("send_email", &runs))
        .approval(Approval::new().tool("*"))
        .output(person())
        .build()
        .unwrap();
    let result = agent.run("x").await.unwrap();
    assert!(!result.is_suspended());
    assert_eq!(result.stop_reason, Some(StopReason::Output));
}

#[test]
fn output_configuration_is_checked_at_build_time() {
    let model = fake(vec![]);
    let err = Agent::builder_shared(model.clone())
        .output(json!("not a schema"))
        .build()
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "the output schema must be a JSON Schema object"
    );
    let runs = Arc::new(AtomicUsize::new(0));
    let reserved = Agent::builder_shared(model)
        .tool(counter_tool("final_output", &runs))
        .build()
        .unwrap_err();
    assert!(reserved.to_string().contains("reserved"));
}
