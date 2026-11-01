//! Tool features (design 0002 §3–§5, 0003 §5): per-tool concurrency, safe
//! retries, result truncation, state updates and app context. Paused clock.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use criogen_agent::{Agent, Limits, StopReason, ToolError, ToolOutput, tool_fn};
use criogen_core::{EventKind, RetryPolicy, event_channel};
use criogen_providers::{
    FakeModel, FakeReply, Message, Role, ToolAnnotations, ToolSpec, ToolStatus,
};
use serde_json::{Value, json};
use tokio::time::Instant;

const SECOND: Duration = Duration::from_secs(1);

fn fake(replies: Vec<FakeReply>) -> FakeModel {
    let mut model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO);
    for reply in replies {
        model = model.then(reply);
    }
    model
}

fn spec(name: &str) -> ToolSpec {
    ToolSpec::new(name, "", json!({"type": "object"}))
}

fn tool_messages(messages: &[Message]) -> Vec<&Message> {
    messages.iter().filter(|m| m.role == Role::Tool).collect()
}

fn sleeper(name: &'static str, mut spec: ToolSpec) -> impl criogen_agent::Tool {
    spec.name = name.into();
    tool_fn(spec, |_, _| async {
        tokio::time::sleep(2 * SECOND).await;
        Ok::<_, ToolError>("done")
    })
}

#[tokio::test(start_paused = true)]
async fn max_concurrency_limits_one_tool_only() {
    let mut serial = spec("serial");
    serial.max_concurrency = Some(1);
    let agent = Agent::builder(fake(vec![
        FakeReply::tool_calls([
            ("serial", json!({})),
            ("serial", json!({})),
            ("free", json!({})),
            ("free", json!({})),
        ]),
        FakeReply::text("ok"),
    ]))
    .tool(sleeper("serial", serial))
    .tool(sleeper("free", spec("free")))
    .build()
    .unwrap();
    let started = Instant::now();
    agent.run("x").await.unwrap();
    // The two `serial` calls take turns (4 s); the `free` ones overlap them.
    assert_eq!(started.elapsed(), 4 * SECOND);
}

fn flaky_tool(
    name: &str,
    annotations: ToolAnnotations,
    fails: usize,
    calls: &Arc<AtomicUsize>,
) -> impl criogen_agent::Tool {
    let calls = calls.clone();
    tool_fn(spec(name).with_annotations(annotations), move |_, _| {
        let calls = calls.clone();
        async move {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if n < fails {
                return Err(ToolError::new("index busy").retryable());
            }
            Ok::<_, ToolError>("found it")
        }
    })
}

fn quick_retry() -> RetryPolicy {
    RetryPolicy::new()
        .max_attempts(3)
        .backoff(SECOND, 2.0, Duration::from_secs(10))
        .jitter(false)
}

#[tokio::test(start_paused = true)]
async fn read_only_tools_are_retried_and_others_are_not() {
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::builder(fake(vec![
        FakeReply::tool_call("search", json!({})),
        FakeReply::text("ok"),
    ]))
    .tool(flaky_tool(
        "search",
        ToolAnnotations::read_only(),
        2,
        &calls,
    ))
    .tool_retry(quick_retry())
    .build()
    .unwrap();
    let started = Instant::now();
    let result = agent.run("x").await.unwrap();
    assert_eq!(started.elapsed(), 3 * SECOND, "1 s + 2 s backoff");
    let message = tool_messages(&result.messages)[0];
    assert_eq!(message.status, Some(ToolStatus::Success));
    assert_eq!(message.metadata.attempts, Some(3));
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    // The same failure from a tool with side effects is not retried.
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::builder(fake(vec![
        FakeReply::tool_call("send", json!({})),
        FakeReply::text("ok"),
    ]))
    .tool(flaky_tool("send", ToolAnnotations::default(), 2, &calls))
    .tool_retry(quick_retry())
    .build()
    .unwrap();
    let result = agent.run("x").await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let message = tool_messages(&result.messages)[0];
    assert_eq!(message.status, Some(ToolStatus::Error));
    assert!(message.error.as_ref().unwrap().retryable);
}

#[tokio::test(start_paused = true)]
async fn idempotent_tools_are_retried_after_a_timeout() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut slow_once = spec("fetch").with_annotations(ToolAnnotations::idempotent());
    slow_once.timeout_ms = Some(1_000);
    let tool = {
        let calls = calls.clone();
        tool_fn(slow_once, move |_, _| {
            let calls = calls.clone();
            async move {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    tokio::time::sleep(10 * SECOND).await;
                }
                Ok::<_, ToolError>("page")
            }
        })
    };
    let agent = Agent::builder(fake(vec![
        FakeReply::tool_call("fetch", json!({})),
        FakeReply::text("ok"),
    ]))
    .tool(tool)
    .tool_retry(quick_retry())
    .build()
    .unwrap();
    let started = Instant::now();
    let result = agent.run("x").await.unwrap();
    assert_eq!(started.elapsed(), 2 * SECOND, "1 s timeout + 1 s backoff");
    assert_eq!(tool_messages(&result.messages)[0].text(), "page");
}

#[tokio::test(start_paused = true)]
async fn oversized_results_are_cut_and_kept_in_the_artifact() {
    let mut big = spec("dump");
    big.max_result_chars = Some(10);
    let tool = tool_fn(big, |_, _| async {
        Ok::<_, ToolError>("0123456789abcdefghij")
    });
    let agent = Agent::builder(fake(vec![
        FakeReply::tool_call("dump", json!({})),
        FakeReply::text("ok"),
    ]))
    .tool(tool)
    .build()
    .unwrap();
    let (sender, mut receiver) = event_channel(256);
    let run = agent.run_with("x", agent.run_options().events(sender));
    let read = async {
        let mut warnings = Vec::new();
        while let Some(event) = receiver.recv().await {
            if let EventKind::Custom { name, payload } = event.kind
                && name == "warning"
            {
                warnings.push(payload["code"].as_str().unwrap_or_default().to_string());
            }
        }
        warnings
    };
    let (result, warnings) = tokio::join!(run, read);
    let result = result.unwrap();
    let message = tool_messages(&result.messages)[0];
    assert_eq!(
        message.text(),
        "0123456789\n[truncated: 10 more characters]"
    );
    assert!(message.metadata.truncated);
    assert_eq!(
        message.artifact.as_ref().unwrap()[0]["text"],
        "0123456789abcdefghij",
        "the full result stays with the app"
    );
    assert_eq!(warnings, ["tool_result_truncated"]);
}

#[tokio::test(start_paused = true)]
async fn the_default_result_limit_applies_to_every_tool() {
    let tool = tool_fn(spec("dump"), |_, _| async {
        Ok::<_, ToolError>("x".repeat(50))
    });
    let agent = Agent::builder(fake(vec![
        FakeReply::tool_call("dump", json!({})),
        FakeReply::text("ok"),
    ]))
    .tool(tool)
    .limits(Limits::default().max_result_chars(20))
    .build()
    .unwrap();
    let result = agent.run("x").await.unwrap();
    assert!(
        tool_messages(&result.messages)[0]
            .text()
            .ends_with("[truncated: 30 more characters]")
    );
}

fn todo_tool(name: &'static str, item: &'static str) -> impl criogen_agent::Tool {
    tool_fn(spec(name), move |_, _| async move {
        Ok::<_, ToolError>(
            ToolOutput::Text(format!("added {item}"))
                .with_state("todo", json!([item]))
                .with_artifact(json!({"db_row": 7})),
        )
    })
}

#[tokio::test(start_paused = true)]
async fn tool_state_updates_are_merged_in_call_order() {
    let agent = Agent::builder(fake(vec![
        FakeReply::tool_calls([("add_b", json!({})), ("add_a", json!({}))]),
        FakeReply::text("ok"),
    ]))
    .tool(todo_tool("add_a", "a"))
    .tool(todo_tool("add_b", "b"))
    .build()
    .unwrap();
    let result = agent.run("x").await.unwrap();
    assert_eq!(result.run.state.get("todo").unwrap(), &json!(["b", "a"]));
    let message = tool_messages(&result.messages)[0];
    assert_eq!(message.artifact, Some(json!({"db_row": 7})));
    assert_eq!(message.text(), "added b");
}

#[tokio::test(start_paused = true)]
async fn conflicting_or_reserved_state_writes_fail() {
    let status = |name: &'static str, value: &'static str| {
        tool_fn(spec(name), move |_, _| async move {
            Ok::<_, ToolError>(ToolOutput::Text("ok".into()).with_state("status", json!(value)))
        })
    };
    let agent = Agent::builder(fake(vec![FakeReply::tool_calls([
        ("one", json!({})),
        ("two", json!({})),
    ])]))
    .tool(status("one", "open"))
    .tool(status("two", "closed"))
    .build()
    .unwrap();
    let err = agent.run("x").await.unwrap_err();
    assert_eq!(err.code, "update_conflict");

    let sneaky = tool_fn(spec("sneaky"), |_, _| async {
        Ok::<_, ToolError>(ToolOutput::Text("ok".into()).with_state("messages", json!([])))
    });
    let agent = Agent::builder(fake(vec![FakeReply::tool_call("sneaky", json!({}))]))
        .tool(sneaky)
        .build()
        .unwrap();
    let err = agent.run("x").await.unwrap_err();
    assert_eq!(err.code, "tool_failed");
    assert!(err.message.contains("`messages`"));
}

struct Database {
    patients: Vec<&'static str>,
}

#[tokio::test(start_paused = true)]
async fn tools_reach_app_services_through_the_context() {
    let lookup = tool_fn(spec("count_patients"), |_, ctx| async move {
        let db = ctx
            .context::<Database>()
            .ok_or_else(|| ToolError::fatal("no database configured"))?;
        Ok::<_, ToolError>(json!(db.patients.len()))
    });
    let agent = Agent::builder(fake(vec![
        FakeReply::tool_call("count_patients", json!({})),
        FakeReply::text("ok"),
    ]))
    .tool(lookup)
    .context(Database {
        patients: vec!["P-1", "P-2"],
    })
    .build()
    .unwrap();
    let result = agent.run("how many?").await.unwrap();
    assert!(matches!(
        &tool_messages(&result.messages)[0].content[0],
        criogen_providers::ContentBlock::Json { value, .. } if value == &json!(2)
    ));
}

#[tokio::test(start_paused = true)]
async fn a_retried_step_keeps_saved_state_and_direct_results() {
    let bug = Arc::new(AtomicBool::new(true));
    let adds = Arc::new(AtomicUsize::new(0));
    let add = {
        let adds = adds.clone();
        tool_fn(spec("add"), move |_, _| {
            let adds = adds.clone();
            async move {
                adds.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ToolError>(
                    ToolOutput::Text("added".into()).with_state("todo", json!(["x"])),
                )
            }
        })
    };
    let mut direct = spec("finish");
    direct.return_direct = true;
    let finish = {
        let bug = bug.clone();
        tool_fn(direct, move |_, _| {
            let bug = bug.clone();
            async move {
                tokio::time::sleep(SECOND).await;
                if bug.load(Ordering::SeqCst) {
                    return Err(ToolError::fatal("crash"));
                }
                Ok::<_, ToolError>(json!({"done": true}))
            }
        })
    };
    let agent = Agent::builder(fake(vec![FakeReply::tool_calls([
        ("add", json!({})),
        ("finish", json!({})),
    ])]))
    .tool(add)
    .tool(finish)
    .build()
    .unwrap();
    let err = agent.run("x").await.unwrap_err();
    bug.store(false, Ordering::SeqCst);
    let result = agent.retry(err.snapshot().unwrap()).await.unwrap();
    assert_eq!(adds.load(Ordering::SeqCst), 1, "`add` was not run again");
    assert_eq!(
        result.run.state.get("todo").unwrap(),
        &json!(["x"]),
        "its state change kept"
    );
    assert_eq!(result.stop_reason, Some(StopReason::ReturnDirect));
    assert_eq!(result.output, Some(json!({"done": true})));
}

#[test]
fn rich_outputs_keep_their_value() {
    let output = ToolOutput::Json(json!({"a": 1})).with_artifact(Value::Null);
    assert!(matches!(output, ToolOutput::Rich { .. }));
}
