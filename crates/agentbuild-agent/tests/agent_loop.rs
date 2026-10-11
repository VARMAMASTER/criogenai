//! The agent's tool-calling graph (design 0004 §4), driven by `FakeModel` on
//! Tokio's paused clock, so every timing assertion is exact.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use agentbuild_agent::{Agent, Limits, StopReason, ToolError, tool_fn};
use agentbuild_core::{CancelToken, EventKind, RetryPolicy, RunOptions, event_channel};
use agentbuild_providers::{
    ContentBlock, FakeModel, FakeReply, Message, ModelError, ModelErrorKind, Role, ToolSpec,
    ToolStatus, transcript,
};
use serde_json::{Value, json};
use tokio::time::Instant;

const SECOND: Duration = Duration::from_secs(1);

/// A model that answers every call after exactly `secs` seconds.
fn model(secs: u64) -> FakeModel {
    let delay = Duration::from_secs(secs);
    FakeModel::with_delay(delay, delay)
}

/// `weather` sleeps `wait` seconds (from the args, default 0), then answers.
fn weather() -> impl agentbuild_agent::Tool {
    tool_fn(weather_spec_with_wait(), |args, _ctx| async move {
        let wait = args.get("wait").and_then(Value::as_u64).unwrap_or(0);
        tokio::time::sleep(Duration::from_secs(wait)).await;
        let city = args["city"].as_str().unwrap_or_default().to_string();
        let celsius = if city == "Paris" { 21 } else { 9 };
        Ok::<_, ToolError>(json!({"city": city, "celsius": celsius}))
    })
}

fn weather_spec_with_wait() -> ToolSpec {
    ToolSpec::new(
        "weather",
        "Current temperature for a city",
        json!({
            "type": "object",
            "properties": {"city": {"type": "string"}, "wait": {"type": "integer"}},
            "required": ["city"],
            "additionalProperties": false
        }),
    )
}

fn roles(messages: &[Message]) -> Vec<&'static str> {
    messages.iter().map(|m| m.role.as_str()).collect()
}

fn statuses(messages: &[Message]) -> Vec<(String, ToolStatus)> {
    messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| (m.call_id.clone().unwrap(), m.status.unwrap()))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn one_tool_round_trip() {
    let model = model(2)
        .then(FakeReply::tool_call("weather", json!({"city": "Paris"})))
        .then(FakeReply::text("It is 21 °C in Paris."));
    let agent = Agent::builder(model)
        .instructions("Answer briefly.")
        .tool(weather())
        .build()
        .unwrap();

    let started = Instant::now();
    let result = agent.run("Weather in Paris?").await.unwrap();
    assert_eq!(started.elapsed(), 4 * SECOND, "two model calls of 2 s");
    assert_eq!(result.text(), Some("It is 21 °C in Paris."));
    assert_eq!(result.stop_reason, Some(StopReason::FinalAnswer));
    assert_eq!((result.model_calls, result.tool_calls), (2, 1));
    assert_eq!(
        roles(&result.messages),
        ["user", "assistant", "tool", "assistant"]
    );
    assert!(transcript::check(&result.messages).is_empty());
    let tool_message = &result.messages[2];
    assert_eq!(tool_message.status, Some(ToolStatus::Success));
    assert!(matches!(
        &tool_message.content[0],
        ContentBlock::Json { value, .. } if value["celsius"] == 21
    ));
    let steps: Vec<Vec<String>> = result.run.steps.iter().map(|s| s.nodes.clone()).collect();
    assert_eq!(steps, [["model"], ["tools"], ["model"]]);
    assert!(result.usage.input_tokens > 0);
}

#[tokio::test(start_paused = true)]
async fn instructions_and_tools_reach_the_model_but_not_the_transcript() {
    let fake = Arc::new(model(0).then(FakeReply::text("hi")));
    let agent = Agent::builder_shared(fake.clone())
        .instructions("Be kind.")
        .tool(weather())
        .build()
        .unwrap();
    let result = agent.run("hello").await.unwrap();
    let request = &fake.requests()[0];
    assert_eq!(request.messages[0].role, Role::System);
    assert_eq!(request.messages[0].text(), "Be kind.");
    assert_eq!(request.tools[0].name, "weather");
    assert_eq!(roles(&result.messages), ["user", "assistant"]);
}

#[tokio::test(start_paused = true)]
async fn parallel_calls_run_concurrently_and_results_keep_call_order() {
    let script = || {
        model(0).then(FakeReply::tool_calls([
            ("weather", json!({"city": "A", "wait": 3})),
            ("weather", json!({"city": "B", "wait": 1})),
            ("weather", json!({"city": "C", "wait": 2})),
        ]))
    };
    let agent = Agent::builder(script()).tool(weather()).build().unwrap();
    let started = Instant::now();
    let result = agent.run("three cities").await.unwrap();
    assert_eq!(started.elapsed(), 3 * SECOND, "as long as the slowest call");
    let order: Vec<String> = statuses(&result.messages)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(
        order,
        ["call_1_0", "call_1_1", "call_1_2"],
        "call order, not finish order"
    );

    let serial = Agent::builder(script())
        .tool(weather())
        .limits(Limits::default().parallel_tool_calls(1))
        .build()
        .unwrap();
    let started = Instant::now();
    serial.run("three cities").await.unwrap();
    assert_eq!(started.elapsed(), 6 * SECOND, "one at a time");
}

#[tokio::test(start_paused = true)]
async fn unknown_tools_and_bad_arguments_go_back_to_the_model() {
    let fake = Arc::new(
        model(0)
            .then(FakeReply::tool_calls([
                ("forecast", json!({"city": "Oslo"})),
                ("weather", json!({"city": 5, "units": "F"})),
            ]))
            .then(FakeReply::text("Sorry, let me fix that.")),
    );
    let agent = Agent::builder_shared(fake.clone())
        .tool(weather())
        .build()
        .unwrap();
    let result = agent.run("x").await.unwrap();
    let errors: Vec<(String, String)> = result
        .messages
        .iter()
        .filter_map(|m| m.error.as_ref())
        .map(|e| (e.code.clone(), e.message.clone()))
        .collect();
    assert_eq!(
        errors,
        [
            (
                "unknown_tool".to_string(),
                "Unknown tool `forecast`. Available tools: weather".to_string()
            ),
            (
                "invalid_arguments".to_string(),
                "Invalid arguments: city: expected string, got 5; units: unknown argument"
                    .to_string()
            ),
        ]
    );
    assert_eq!(result.tool_calls, 0, "nothing was executed");
    // The second model call saw both errors.
    let second = &fake.requests()[1];
    assert_eq!(
        second.messages.iter().filter(|m| m.error.is_some()).count(),
        2
    );
}

#[tokio::test(start_paused = true)]
async fn invalid_tool_calls_get_an_error_result_and_the_loop_continues() {
    let broken = Message::with_content(
        Role::Assistant,
        vec![ContentBlock::invalid_tool_call(
            "c1",
            Some("weather".into()),
            "{\"city\": ",
            "truncated",
        )],
    );
    let agent = Agent::builder(
        model(0)
            .then(FakeReply::message(broken))
            .then(FakeReply::text("done")),
    )
    .tool(weather())
    .build()
    .unwrap();
    let result = agent.run("x").await.unwrap();
    assert_eq!(
        statuses(&result.messages),
        [("c1".into(), ToolStatus::Error)]
    );
    assert_eq!(result.text(), Some("done"));
    assert!(transcript::check(&result.messages).is_empty());
}

#[tokio::test(start_paused = true)]
async fn tool_errors_and_timeouts_are_results_for_the_model() {
    let flaky = tool_fn(
        ToolSpec::new("flaky", "", json!({"type": "object"})),
        |_, _| async { Err::<Value, _>(ToolError::new("index offline").retryable()) },
    );
    let mut slow_spec = ToolSpec::new("slow", "", json!({"type": "object"}));
    slow_spec.timeout_ms = Some(500);
    let slow = tool_fn(slow_spec, |_, _| async {
        tokio::time::sleep(Duration::from_secs(10)).await;
        Ok::<_, ToolError>("late")
    });
    let agent = Agent::builder(
        model(0)
            .then(FakeReply::tool_calls([
                ("flaky", json!({})),
                ("slow", json!({})),
            ]))
            .then(FakeReply::text("ok")),
    )
    .tool(flaky)
    .tool(slow)
    .build()
    .unwrap();
    let started = Instant::now();
    let result = agent.run("x").await.unwrap();
    assert_eq!(started.elapsed(), Duration::from_millis(500));
    let errors: Vec<(&str, &str, bool)> = result
        .messages
        .iter()
        .filter_map(|m| m.error.as_ref())
        .map(|e| (e.code.as_str(), e.message.as_str(), e.retryable))
        .collect();
    assert_eq!(
        errors,
        [
            ("tool_error", "index offline", true),
            ("timeout", "The tool did not finish within 500 ms", true),
        ]
    );
    assert_eq!(result.messages[2].text(), "Error: index offline");
    assert_eq!(result.tool_calls, 2);
}

/// The 0003 E2 scenario: a tool sends an email, another tool has a bug.
#[tokio::test(start_paused = true)]
async fn failed_tools_step_is_retried_without_resending_the_email() {
    let sent = Arc::new(AtomicUsize::new(0));
    let bug = Arc::new(AtomicBool::new(true));
    let send_email = {
        let sent = sent.clone();
        tool_fn(
            ToolSpec::new("send_email", "", json!({"type": "object"})),
            move |_, _| {
                let sent = sent.clone();
                async move {
                    sent.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, ToolError>("sent")
                }
            },
        )
    };
    let lookup = {
        let bug = bug.clone();
        tool_fn(
            ToolSpec::new("lookup", "", json!({"type": "object"})),
            move |_, _| {
                let bug = bug.clone();
                async move {
                    tokio::time::sleep(SECOND).await; // after the email went out
                    if bug.load(Ordering::SeqCst) {
                        return Err(ToolError::fatal("KeyError: 'customer'"));
                    }
                    Ok::<_, ToolError>("found")
                }
            },
        )
    };
    let agent = Agent::builder(
        model(0)
            .then(FakeReply::tool_calls([
                ("send_email", json!({})),
                ("lookup", json!({})),
            ]))
            .then(FakeReply::text("Done.")),
    )
    .tool(send_email)
    .tool(lookup)
    .build()
    .unwrap();

    let err = agent.run("email and look up").await.unwrap_err();
    assert_eq!(err.code, "tool_failed");
    assert_eq!(err.details.as_ref().unwrap()["tool"], "lookup");
    assert_eq!(
        err.details.as_ref().unwrap()["completed_calls"],
        json!(["call_1_0"])
    );
    assert_eq!(sent.load(Ordering::SeqCst), 1);

    bug.store(false, Ordering::SeqCst);
    let snapshot = err.snapshot().expect("failures carry a snapshot").clone();
    let result = agent.retry(&snapshot).await.unwrap();
    assert_eq!(
        sent.load(Ordering::SeqCst),
        1,
        "the email was not sent twice"
    );
    assert_eq!(result.text(), Some("Done."));
    assert_eq!(
        statuses(&result.messages),
        [
            ("call_1_0".into(), ToolStatus::Success),
            ("call_1_1".into(), ToolStatus::Success)
        ]
    );
    assert_eq!(result.tool_calls, 2);
}

#[tokio::test(start_paused = true)]
async fn model_call_limit_stops_a_runaway_loop() {
    let mut fake = model(0);
    for _ in 0..10 {
        fake = fake.then(FakeReply::tool_call("weather", json!({"city": "Oslo"})));
    }
    let agent = Agent::builder(fake)
        .tool(weather())
        .limits(Limits::default().model_calls(3))
        .build()
        .unwrap();
    let err = agent.run("loop").await.unwrap_err();
    assert_eq!(err.code, "limit_exceeded");
    assert_eq!(err.details.unwrap()["limit"], "model_calls");
    // The three allowed turns are committed.
    let state = &err.run_error.unwrap().state;
    assert_eq!(state.get("agent").unwrap()["model_calls"], 3);
}

#[tokio::test(start_paused = true)]
async fn per_turn_and_per_run_tool_limits_skip_the_excess() {
    let calls = (0..3).map(|_| ("weather", json!({"city": "Oslo"})));
    let agent = Agent::builder(
        model(0)
            .then(FakeReply::tool_calls(calls))
            .then(FakeReply::text("ok")),
    )
    .tool(weather())
    .limits(Limits::default().tool_calls_per_turn(2))
    .build()
    .unwrap();
    let result = agent.run("x").await.unwrap();
    let got: Vec<ToolStatus> = statuses(&result.messages)
        .into_iter()
        .map(|(_, s)| s)
        .collect();
    assert_eq!(
        got,
        [
            ToolStatus::Success,
            ToolStatus::Success,
            ToolStatus::Skipped
        ]
    );
    assert_eq!(result.tool_calls, 2);

    let calls = (0..2).map(|_| ("weather", json!({"city": "Oslo"})));
    let agent = Agent::builder(
        model(0)
            .then(FakeReply::tool_calls(calls))
            .then(FakeReply::text("ok")),
    )
    .tool(weather())
    .limits(Limits::default().tool_calls(1))
    .build()
    .unwrap();
    let result = agent.run("x").await.unwrap();
    let skipped = result
        .messages
        .iter()
        .find(|m| m.status == Some(ToolStatus::Skipped))
        .unwrap();
    assert_eq!(
        skipped.text(),
        "Skipped: the run's tool call limit (1) was reached"
    );
}

#[tokio::test(start_paused = true)]
async fn return_direct_tool_ends_the_run_with_its_result() {
    let mut spec = ToolSpec::new("lookup_order", "", json!({"type": "object"}));
    spec.return_direct = true;
    let lookup = tool_fn(spec, |_, _| async {
        Ok::<_, ToolError>(json!({"order": 42, "status": "shipped"}))
    });
    let fake = Arc::new(model(1).then(FakeReply::tool_call("lookup_order", json!({}))));
    let agent = Agent::builder_shared(fake.clone())
        .tool(lookup)
        .build()
        .unwrap();
    let result = agent.run("where is my order").await.unwrap();
    assert_eq!(result.stop_reason, Some(StopReason::ReturnDirect));
    assert_eq!(
        result.output,
        Some(json!({"order": 42, "status": "shipped"}))
    );
    assert_eq!(fake.calls(), 1, "no second model call");
}

#[tokio::test(start_paused = true)]
async fn transient_model_errors_are_retried_with_backoff() {
    let fake = Arc::new(model(1).flaky_first(2).then(FakeReply::text("finally")));
    let retry = RetryPolicy::new()
        .max_attempts(3)
        .backoff(SECOND, 2.0, Duration::from_secs(10))
        .jitter(false);
    let agent = Agent::builder_shared(fake.clone())
        .model_retry(retry)
        .build()
        .unwrap();
    let started = Instant::now();
    let result = agent.run("hi").await.unwrap();
    // 1 s call, 1 s backoff, 1 s call, 2 s backoff, 1 s call.
    assert_eq!(started.elapsed(), 6 * SECOND);
    assert_eq!(result.text(), Some("finally"));
    assert_eq!(
        result.model_calls, 1,
        "attempts are not separate model calls"
    );
    assert_eq!(fake.calls(), 3);
}

#[tokio::test(start_paused = true)]
async fn fatal_model_errors_fail_the_run_with_the_classified_error() {
    let error = ModelError::new(ModelErrorKind::ContextOverflow, "prompt is too long");
    let fake = Arc::new(model(0).then(FakeReply::Error(error)));
    let agent = Agent::builder_shared(fake.clone()).build().unwrap();
    let err = agent.run("hi").await.unwrap_err();
    assert_eq!(err.code, "model_failed");
    assert_eq!(
        err.details.unwrap()["model_error"]["kind"],
        "context_overflow"
    );
    assert_eq!(fake.calls(), 1, "not retried");
}

#[tokio::test(start_paused = true)]
async fn events_show_the_loop_as_it_happens() {
    let agent = Agent::builder(
        model(1)
            .then(FakeReply::tool_call("weather", json!({"city": "Paris"})))
            .then(FakeReply::text("21 °C")),
    )
    .tool(weather())
    .build()
    .unwrap();
    let (sender, mut receiver) = event_channel(256);
    let run = agent.run_with("weather?", agent.run_options().events(sender));
    let read = async {
        let mut names = Vec::new();
        while let Some(event) = receiver.recv().await {
            if let EventKind::Custom { name, .. } = &event.kind {
                names.push(format!("{}:{name}", event.node.as_deref().unwrap_or("-")));
            }
        }
        names
    };
    let (result, names) = tokio::join!(run, read);
    result.unwrap();
    assert_eq!(
        names,
        [
            "model:message_completed",
            "tools:tool_call_started",
            "tools:tool_call_finished",
            "model:message_delta",
            "model:message_completed",
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn dangling_tool_calls_are_repaired_before_the_model_is_called() {
    let dangling = Message::with_content(
        Role::Assistant,
        vec![ContentBlock::tool_call(
            "old",
            "weather",
            Default::default(),
        )],
    );
    let fake = Arc::new(model(0).then(FakeReply::text("fresh start")));
    let agent = Agent::builder_shared(fake.clone())
        .tool(weather())
        .build()
        .unwrap();
    let input = vec![
        Message::user("first question"),
        dangling,
        Message::user("second question"),
    ];
    let result = agent.run(input).await.unwrap();
    assert_eq!(
        roles(&result.messages),
        ["user", "assistant", "tool", "user", "assistant"]
    );
    assert_eq!(result.messages[2].status, Some(ToolStatus::Cancelled));
    assert!(transcript::check(&fake.requests()[0].messages).is_empty());
}

#[tokio::test(start_paused = true)]
async fn cancelling_during_tools_fails_cleanly_and_can_be_retried() {
    let agent = Agent::builder(
        model(1)
            .then(FakeReply::tool_call(
                "weather",
                json!({"city": "Oslo", "wait": 30}),
            ))
            .then(FakeReply::text("cold")),
    )
    .tool(weather())
    .build()
    .unwrap();
    let token = CancelToken::new();
    let canceller = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(5 * SECOND).await;
        canceller.cancel();
    });
    let options = RunOptions::default()
        .cancel_token(token)
        .cancel_grace(Duration::from_millis(100));
    let err = agent.run_with("weather", options).await.unwrap_err();
    assert_eq!(err.code, "cancelled");
    let snapshot = err.snapshot().unwrap();
    assert_eq!(snapshot.frontier, ["tools"]);

    let result = agent.retry(snapshot).await.unwrap();
    assert_eq!(result.text(), Some("cold"));
    assert!(transcript::check(&result.messages).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_refusal_ends_the_run_even_with_tool_calls() {
    let mut refusal = Message::with_content(
        Role::Assistant,
        vec![
            ContentBlock::text("I can't help with that."),
            ContentBlock::tool_call("c1", "weather", Default::default()),
        ],
    );
    refusal.metadata.finish_reason = Some(agentbuild_providers::FinishReason::ContentFilter);
    let ran = Arc::new(AtomicUsize::new(0));
    let counting = {
        let ran = ran.clone();
        tool_fn(weather_spec_with_wait(), move |_, _| {
            let ran = ran.clone();
            async move {
                ran.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ToolError>("x")
            }
        })
    };
    let agent = Agent::builder(model(0).then(FakeReply::message(refusal)))
        .tool(counting)
        .build()
        .unwrap();
    let result = agent.run("something bad").await.unwrap();
    assert_eq!(result.stop_reason, Some(StopReason::ContentFilter));
    assert_eq!(result.text(), Some("I can't help with that."));
    assert_eq!(ran.load(Ordering::SeqCst), 0, "no tools after a refusal");
}

#[tokio::test]
async fn invalid_configuration_is_rejected_at_build_time() {
    let duplicate = Agent::builder(model(0))
        .tool(weather())
        .tool(weather())
        .build()
        .unwrap_err();
    assert!(duplicate.to_string().contains("more than once"));
    let zero = Agent::builder(model(0))
        .limits(Limits::default().model_calls(0))
        .build()
        .unwrap_err();
    assert_eq!(zero.to_string(), "limit `model_calls` must be at least 1");
}
