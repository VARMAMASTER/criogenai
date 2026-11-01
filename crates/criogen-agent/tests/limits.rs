//! Budgets and what happens when they run out (design 0004 §5).

use std::time::Duration;

use criogen_agent::{Agent, Limits, OnLimit, StopReason, ToolError, tool_fn};
use criogen_providers::{ContentBlock, FakeModel, FakeReply, Message, Role, ToolSpec, transcript};
use serde_json::{Map, json};

fn looping_model(turns: usize) -> FakeModel {
    let mut model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO);
    for i in 0..turns {
        let message = Message::with_content(
            Role::Assistant,
            vec![
                ContentBlock::text(format!("Still checking ({i})...")),
                ContentBlock::tool_call(format!("c{i}"), "check", Map::new()),
            ],
        );
        model = model.then(FakeReply::message(message));
    }
    model
}

fn check_tool() -> impl criogen_agent::Tool {
    tool_fn(
        ToolSpec::new("check", "", json!({"type": "object"})),
        |_, _| async { Ok::<_, ToolError>("not yet") },
    )
}

#[tokio::test(start_paused = true)]
async fn model_call_limit_can_end_gracefully() {
    let agent = Agent::builder(looping_model(10))
        .tool(check_tool())
        .limits(Limits::default().model_calls(3).on_limit(OnLimit::End))
        .build()
        .unwrap();
    let result = agent.run("loop").await.unwrap();
    assert_eq!(result.stop_reason, Some(StopReason::Limit));
    assert_eq!(result.limit.as_deref(), Some("model_calls"));
    assert_eq!(result.model_calls, 3);
    assert_eq!(
        result.text(),
        Some("Still checking (2)..."),
        "the last assistant text"
    );
    assert!(
        transcript::check(&result.messages).is_empty(),
        "valid transcript"
    );
}

#[tokio::test(start_paused = true)]
async fn token_budget_is_checked_before_each_call() {
    // Each simulated call reports a few tokens; a budget of 1 allows one call.
    let build = |on_limit| {
        Agent::builder(looping_model(10))
            .tool(check_tool())
            .limits(Limits::default().tokens(1).on_limit(on_limit))
            .build()
            .unwrap()
    };
    let err = build(OnLimit::Error).run("loop").await.unwrap_err();
    assert_eq!(err.code, "limit_exceeded");
    assert_eq!(err.details.as_ref().unwrap()["limit"], "tokens");
    assert_eq!(err.details.as_ref().unwrap()["value"], 1);

    let result = build(OnLimit::End).run("loop").await.unwrap();
    assert_eq!(result.limit.as_deref(), Some("tokens"));
    assert_eq!(result.model_calls, 1);
    assert!(result.usage.input_tokens + result.usage.output_tokens >= 1);
}

#[tokio::test(start_paused = true)]
async fn a_spent_tool_budget_stops_at_the_next_model_call() {
    let agent = Agent::builder(looping_model(10))
        .tool(check_tool())
        .limits(Limits::default().tool_calls(2))
        .build()
        .unwrap();
    let err = agent.run("loop").await.unwrap_err();
    assert_eq!(err.code, "limit_exceeded");
    assert_eq!(err.details.unwrap()["limit"], "tool_calls");
    let state = &err.run_error.as_ref().unwrap().state;
    assert_eq!(state.get("agent").unwrap()["tool_calls"], 2);
}

#[tokio::test(start_paused = true)]
async fn no_limit_is_hit_when_the_answer_comes_in_time() {
    let model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO).then(FakeReply::text("done"));
    let agent = Agent::builder(model)
        .limits(
            Limits::default()
                .model_calls(1)
                .tokens(1_000)
                .on_limit(OnLimit::End),
        )
        .build()
        .unwrap();
    let result = agent.run("hi").await.unwrap();
    assert_eq!(result.stop_reason, Some(StopReason::FinalAnswer));
    assert_eq!(result.limit, None);
}
