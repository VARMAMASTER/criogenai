//! `FakeModel` behaviour and a hand-driven tool loop, on Tokio's paused clock.

use std::sync::Arc;
use std::time::Duration;

use agentbuild_providers::{
    ContentBlock, FakeModel, FakeReply, FinishReason, Message, Model, ModelErrorKind, ModelRequest,
    ToolSpec, ToolStatus, transcript, validate_tools,
};
use serde_json::json;
use tokio::time::Instant;

fn ask(text: &str) -> ModelRequest {
    ModelRequest::new(vec![Message::user(text)])
}

#[tokio::test(start_paused = true)]
async fn replies_after_a_fixed_two_to_three_second_delay() {
    let model = FakeModel::new();
    for prompt in ["a", "summarise the news", "write a poem"] {
        let started = Instant::now();
        let response = model.invoke(&ask(prompt)).await.unwrap();
        let took = started.elapsed();
        assert_eq!(took, model.delay_for(prompt));
        assert!((Duration::from_secs(2)..=Duration::from_secs(3)).contains(&took));
        assert_eq!(
            response.message.text(),
            format!("[fake-llm] reply to: {prompt}")
        );
        assert_eq!(response.finish_reason, FinishReason::Stop);
        assert!(response.usage.input_tokens > 0 && response.usage.output_tokens > 0);
        assert_eq!(response.message.metadata.usage, response.usage);
        assert!(response.message.validate().is_ok());
    }
    assert_eq!(model.calls(), 3);
}

#[test]
fn delay_depends_on_the_prompt_and_is_reproducible() {
    let model = FakeModel::new();
    let prompts = ["a", "b", "summarise the news", "write a poem", "plan"];
    let delays: std::collections::HashSet<Duration> =
        prompts.iter().map(|p| model.delay_for(p)).collect();
    assert!(delays.len() > 1, "delays should vary by prompt: {delays:?}");
    assert_eq!(FakeModel::new().delay_for("plan"), model.delay_for("plan"));
}

#[tokio::test(start_paused = true)]
async fn parallel_calls_overlap() {
    let model = Arc::new(FakeModel::new());
    let prompts = ["one", "two", "three", "four"];
    let started = Instant::now();
    let calls = prompts.map(|p| {
        let model = model.clone();
        tokio::spawn(async move { model.invoke(&ask(p)).await })
    });
    for call in calls {
        call.await.unwrap().unwrap();
    }
    let slowest = prompts.iter().map(|p| model.delay_for(p)).max().unwrap();
    assert_eq!(started.elapsed(), slowest);
}

#[tokio::test(start_paused = true)]
async fn flaky_calls_are_transient_and_markers_are_not() {
    let model = FakeModel::new().flaky_first(2).failing_on("BOOM");
    for call in 1..=2 {
        let err = model.invoke(&ask("hi")).await.unwrap_err();
        assert_eq!(err.kind, ModelErrorKind::RateLimited);
        assert!(err.kind.is_transient());
        assert!(err.message.contains(&format!("call {call}")));
    }
    assert!(model.invoke(&ask("hi")).await.is_ok());
    let err = model.invoke(&ask("BOOM now")).await.unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::InvalidRequest);
    assert!(!err.kind.is_transient());
    assert_eq!(err.provider.as_deref(), Some("fake"));
}

#[tokio::test(start_paused = true)]
async fn scripted_tool_loop_keeps_the_transcript_valid() {
    let tools = vec![ToolSpec::new(
        "weather",
        "Current weather for a city",
        json!({"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}),
    )];
    validate_tools(&tools).unwrap();
    let model = FakeModel::with_delay(Duration::from_millis(100), Duration::from_millis(100))
        .then(FakeReply::tool_calls([
            ("weather", json!({"city": "Paris"})),
            ("weather", json!({"city": "Oslo"})),
        ]))
        .then(FakeReply::text("Paris is warmer than Oslo."));

    let mut transcript = vec![Message::user("Which is warmer, Paris or Oslo?")];
    let mut turns = 0;
    loop {
        turns += 1;
        let request = ModelRequest::new(transcript.clone()).with_tools(tools.clone());
        let response = model.invoke(&request).await.unwrap();
        let calls: Vec<(String, String)> = response
            .message
            .tool_calls()
            .map(|c| {
                (
                    c.id.to_string(),
                    c.args["city"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        transcript.push(response.message);
        if calls.is_empty() {
            assert_eq!(response.finish_reason, FinishReason::Stop);
            break;
        }
        assert_eq!(response.finish_reason, FinishReason::ToolCalls);
        for (id, city) in calls {
            let temperature = if city == "Paris" { 21 } else { 9 };
            transcript.push(Message::tool_result(
                id,
                "weather",
                ToolStatus::Success,
                vec![ContentBlock::json(
                    json!({"city": city, "celsius": temperature}),
                )],
            ));
        }
        assert!(transcript::check(&transcript).is_empty());
    }
    assert_eq!(turns, 2);
    assert_eq!(
        transcript.last().unwrap().text(),
        "Paris is warmer than Oslo."
    );
    assert!(transcript.iter().all(|m| m.validate().is_ok()));
    // The model saw the tool results on its second call.
    let second = &model.requests()[1];
    assert_eq!(second.messages.len(), 4);
    assert_eq!(second.tools.len(), 1);

    // The whole conversation survives JSON (it is what the SDKs exchange).
    let json = serde_json::to_string(&transcript).unwrap();
    let back: Vec<Message> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, transcript);
}

#[tokio::test(start_paused = true)]
async fn interrupted_tool_loop_is_repaired_before_the_next_call() {
    let model =
        FakeModel::with_delay(Duration::ZERO, Duration::ZERO).then(FakeReply::tool_calls([
            ("search", json!({"q": "a"})),
            ("search", json!({"q": "b"})),
        ]));
    let mut transcript = vec![Message::user("search twice")];
    let response = model
        .invoke(&ModelRequest::new(transcript.clone()))
        .await
        .unwrap();
    let first_call = response.message.tool_calls().next().unwrap().id.to_string();
    transcript.push(response.message);
    // Only the first call finished before the run was cancelled.
    transcript.push(Message::tool_result(
        first_call,
        "search",
        ToolStatus::Success,
        vec![ContentBlock::text("found a")],
    ));
    assert_eq!(transcript::check(&transcript).len(), 1);

    let repaired = transcript::repair(transcript);
    let last = repaired.messages.last().unwrap();
    assert_eq!(last.status, Some(ToolStatus::Cancelled));
    assert!(transcript::check(&repaired.messages).is_empty());
    assert!(
        model
            .invoke(&ModelRequest::new(repaired.messages))
            .await
            .is_ok()
    );
}

#[tokio::test(start_paused = true)]
async fn by_turn_replies_follow_each_conversation_and_need_no_log() {
    let model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO)
        .by_turn()
        .without_request_log()
        .then(FakeReply::tool_call("lookup", json!({"id": 1})))
        .then(FakeReply::text("done"));
    // Two conversations at turn 0, then one at turn 1, then past the script.
    for _ in 0..2 {
        let first = model.invoke(&ask("hi")).await.unwrap();
        assert_eq!(first.finish_reason, FinishReason::ToolCalls);
    }
    let mut turn1 = vec![Message::user("hi"), Message::assistant("thinking")];
    let second = model
        .invoke(&ModelRequest::new(turn1.clone()))
        .await
        .unwrap();
    assert_eq!(second.message.text(), "done");
    turn1.push(Message::assistant("more"));
    let past = model.invoke(&ModelRequest::new(turn1)).await.unwrap();
    assert_eq!(past.message.text(), "done", "the last reply repeats");
    assert_eq!(model.calls(), 4);
    assert!(model.requests().is_empty());
}

#[tokio::test(start_paused = true)]
async fn scripted_errors_are_returned_as_given() {
    let error = agentbuild_providers::ModelError::new(ModelErrorKind::ContextOverflow, "too long");
    let model =
        FakeModel::with_delay(Duration::ZERO, Duration::ZERO).then(FakeReply::Error(error.clone()));
    assert_eq!(model.invoke(&ask("x")).await.unwrap_err(), error);
    assert!(model.invoke(&ask("x")).await.is_ok(), "script exhausted");
}

#[test]
fn models_are_usable_as_trait_objects() {
    let model: Arc<dyn Model> = Arc::new(FakeModel::new());
    assert_eq!((model.provider(), model.model()), ("fake", "fake"));
    assert_eq!(model.capabilities().tools, Some(true));
}

#[test]
fn error_messages_with_non_ascii_urls_do_not_panic() {
    // A hostile or buggy upstream controls this text (security audit).
    for text in [
        "see https://x.example/é",
        "see https://x.example/世界",
        "key sk-abcdéfghijklmnopqrstuvé",
        "https://日本語.example/路径?q=é.",
    ] {
        let error = agentbuild_providers::ModelError::new(
            agentbuild_providers::ModelErrorKind::Server,
            text,
        );
        assert!(!error.message.is_empty());
    }
}
