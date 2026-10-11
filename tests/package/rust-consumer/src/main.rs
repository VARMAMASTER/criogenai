//! Package smoke test: uses `agentbuild-core` from its packaged `.crate`, the
//! way a downstream project would, through the public API only.
//!
//! Run by `scripts/test-package.sh`. Every check panics on failure.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use agentbuild_core::{
    CancelToken, END, EventKind, NodeError, NodeOutput, NodePolicy, Reducer, RetryPolicy,
    RunOptions, SNAPSHOT_FORMAT, Snapshot, State, Update, ValidationIssue, WorkflowBuilder,
    node_fn, node_with_context, router_fn,
};
use agentbuild_providers::{
    FakeModel, FakeReply, HttpFailure, Message, Model, ModelErrorKind, ModelRequest, ToolSpec,
    ToolStatus, classify_http, transcript, validate_tools,
};
use serde_json::json;

#[tokio::main]
async fn main() {
    branching_with_reducers_and_events().await;
    suspend_and_resume_through_json().await;
    retry_a_failed_run_without_repeating_work().await;
    policies_and_cancellation().await;
    validation_reports_every_issue();
    model_inside_a_workflow_node().await;
    provider_errors_and_transcripts();
    agent_with_tools().await;
    println!("package smoke test: ok");
}

async fn branching_with_reducers_and_events() {
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "split",
            node_fn(|_| async { Ok(Update::new().set("log", json!(["split"]))) }),
        )
        .add_node(
            "left",
            node_fn(|_| async { Ok(Update::new().set("log", json!(["left"]))) }),
        )
        .add_node(
            "right",
            node_fn(|_| async { Ok(Update::new().set("log", json!(["right"]))) }),
        )
        .add_node("join", node_fn(|_| async { Ok(Update::new()) }))
        .reducer("log", Reducer::Append)
        .set_entry("split")
        .add_edge("split", "left")
        .add_edge("split", "right")
        .add_join(["left", "right"], "join")
        .add_router(
            "join",
            router_fn(|state| async move {
                let n = state.get("log").and_then(|v| v.as_array()).map_or(0, Vec::len);
                Ok(if n == 3 { "done" } else { "broken" })
            }),
            [("done", END), ("broken", END)],
        );
    let workflow = builder.build().expect("valid graph");

    let mut stream = workflow.stream(State::new());
    let mut types = Vec::new();
    while let Some(event) = stream.next().await {
        types.push(event.type_name());
    }
    let output = stream.finish().await.expect("run completes");
    assert_eq!(output.state.get("log").unwrap(), &json!(["split", "left", "right"]));
    assert_eq!(types.first(), Some(&"run_started"));
    assert_eq!(types.last(), Some(&"run_finished"));
    println!("  branching, joins, reducers, events: ok");
}

async fn suspend_and_resume_through_json() {
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "approve",
            node_with_context(|ctx, _| async move {
                Ok(match ctx.resume() {
                    None => NodeOutput::suspend(Update::new(), json!({"question": "ship it?"})),
                    Some(answer) => Update::new().set("approved", answer.clone()).into(),
                })
            }),
        )
        .set_entry("approve")
        .add_edge("approve", END);
    let workflow = builder.build().unwrap();

    let paused = workflow.run(State::new()).await.unwrap();
    assert!(paused.is_suspended());
    let text = serde_json::to_string(paused.snapshot().unwrap()).unwrap();
    let snapshot: Snapshot = serde_json::from_str(&text).unwrap();
    assert_eq!(snapshot.format, SNAPSHOT_FORMAT);
    let done = workflow
        .resume(&snapshot, [("approve", json!(true))])
        .await
        .unwrap();
    assert_eq!(done.state.get("approved").unwrap(), true);
    println!("  suspend / JSON snapshot / resume: ok");
}

async fn retry_a_failed_run_without_repeating_work() {
    let sends = Arc::new(AtomicUsize::new(0));
    let runs = Arc::new(AtomicUsize::new(0));
    let mut builder = WorkflowBuilder::new();
    let node = {
        let (sends, runs) = (sends.clone(), runs.clone());
        node_with_context(move |ctx, _| {
            let (sends, runs) = (sends.clone(), runs.clone());
            async move {
                let receipt: String = ctx
                    .memo()
                    .once("send", || async {
                        sends.fetch_add(1, Ordering::SeqCst);
                        Ok("receipt-1".to_string())
                    })
                    .await?;
                if runs.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(NodeError::new("crashed after sending"));
                }
                Ok(Update::new().set("receipt", receipt).into())
            }
        })
    };
    builder.add_node("notify", node).set_entry("notify").add_edge("notify", END);
    let workflow = builder.build().unwrap();

    let err = workflow.run(State::new()).await.unwrap_err();
    assert_eq!(err.code(), "node_failed");
    let text = serde_json::to_string(err.snapshot().unwrap()).unwrap();
    let snapshot: Snapshot = serde_json::from_str(&text).unwrap();
    let done = workflow.retry(&snapshot).await.unwrap();
    assert_eq!(done.state.get("receipt").unwrap(), "receipt-1");
    assert_eq!(sends.load(Ordering::SeqCst), 1, "side effect repeated");
    println!("  failure snapshot / retry / task memo: ok");
}

async fn policies_and_cancellation() {
    let calls = Arc::new(AtomicUsize::new(0));
    let flaky = {
        let calls = calls.clone();
        node_fn(move |_| {
            let calls = calls.clone();
            async move {
                if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                    return Err(NodeError::new("rate limited").retryable());
                }
                Ok(Update::new().set("ok", true))
            }
        })
    };
    let retry = RetryPolicy::new()
        .max_attempts(3)
        .backoff(Duration::from_millis(1), 2.0, Duration::from_millis(5));
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("flaky", flaky)
        .set_entry("flaky")
        .add_edge("flaky", END)
        .policy("flaky", NodePolicy::new().retry(retry));
    let out = builder.build().unwrap().run(State::new()).await.unwrap();
    assert_eq!(out.state.get("ok").unwrap(), true);

    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "slow",
            node_fn(|_| async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(Update::new())
            }),
        )
        .set_entry("slow")
        .add_edge("slow", END);
    let workflow = builder.build().unwrap();
    let token = CancelToken::new();
    let canceller = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        canceller.cancel();
    });
    let options = RunOptions::default()
        .cancel_token(token)
        .cancel_grace(Duration::from_millis(10));
    let err = workflow.run_with(State::new(), options).await.unwrap_err();
    assert_eq!(err.code(), "cancelled");

    let err = workflow
        .run_with(State::new(), RunOptions::default().deadline(Duration::from_millis(20)))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "deadline_exceeded");

    let mut stream =
        workflow.stream_with(State::new(), RunOptions::default().cancel_grace(Duration::ZERO));
    let first = stream.next().await.unwrap();
    assert!(matches!(first.kind, EventKind::RunStarted { .. }));
    stream.cancel();
    assert_eq!(stream.finish().await.unwrap_err().code(), "cancelled");
    println!("  retries, cancellation, deadlines: ok");
}

fn validation_reports_every_issue() {
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("a", node_fn(|_| async { Ok(Update::new()) }))
        .add_edge("a", "missing");
    let err = builder.build().unwrap_err();
    assert!(err.issues.contains(&ValidationIssue::MissingEntry));
    assert!(err.issues.len() >= 2, "{err}");
    println!("  validation: ok");
}

/// How the agent layer will combine the crates: a node calls a `Model`.
async fn model_inside_a_workflow_node() {
    let model = Arc::new(
        FakeModel::with_delay(Duration::from_millis(1), Duration::from_millis(5))
            .then(FakeReply::tool_call("lookup", json!({"id": 7}))),
    );
    let tools = vec![ToolSpec::new(
        "lookup",
        "Find a record",
        json!({"type": "object", "properties": {"id": {"type": "integer"}}}),
    )];
    validate_tools(&tools).expect("valid tools");

    let node = {
        let model = model.clone();
        node_fn(move |state| {
            let (model, tools) = (model.clone(), tools.clone());
            async move {
                let messages: Vec<Message> =
                    serde_json::from_value(state.get("messages").cloned().unwrap_or(json!([])))
                        .map_err(|e| NodeError::new(e.to_string()))?;
                let request = ModelRequest::new(messages).with_tools(tools);
                let response = model
                    .invoke(&request)
                    .await
                    .map_err(|e| NodeError::new(e.to_string()).with_kind(e.kind.code()))?;
                Ok(Update::new().set("messages", json!([response.message])))
            }
        })
    };
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("model", node)
        .reducer("messages", Reducer::UpsertById)
        .set_entry("model")
        .add_edge("model", END);
    let input = State::new().with("messages", json!([Message::user("find record 7")]));
    let out = builder.build().unwrap().run(input).await.unwrap();

    let messages: Vec<Message> =
        serde_json::from_value(out.state.get("messages").unwrap().clone()).unwrap();
    assert_eq!(messages.len(), 2);
    let call = messages[1].tool_calls().next().expect("a tool call");
    assert_eq!((call.name, call.args["id"].as_i64()), ("lookup", Some(7)));
    assert_eq!(model.calls(), 1);
    println!("  model call inside a workflow node: ok");
}

fn provider_errors_and_transcripts() {
    let body = r#"{"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}"#;
    let headers = vec![("retry-after".to_string(), "2".to_string())];
    let error = classify_http(
        &HttpFailure::new(529, &headers, body),
        std::time::SystemTime::now(),
    );
    assert_eq!(error.kind, ModelErrorKind::Overloaded);
    assert!(error.kind.is_transient());
    assert_eq!(error.retry_after(), Some(Duration::from_secs(2)));

    let mut args = serde_json::Map::new();
    args.insert("q".into(), json!("x"));
    let assistant = Message::with_content(
        agentbuild_providers::Role::Assistant,
        vec![agentbuild_providers::ContentBlock::tool_call("c1", "search", args)],
    );
    let repaired = transcript::repair(vec![Message::user("hi"), assistant]);
    assert_eq!(repaired.changes.len(), 1);
    assert_eq!(
        repaired.messages.last().unwrap().status,
        Some(ToolStatus::Cancelled)
    );
    println!("  provider error classification, transcript repair: ok");
}

/// The agent crate: a model ⇄ tools loop with a validated tool.
async fn agent_with_tools() {
    use agentbuild_agent::{Agent, ToolError, tool_fn};

    let model = FakeModel::with_delay(Duration::from_millis(1), Duration::from_millis(1))
        .then(FakeReply::tool_calls([
            ("double", json!({"n": 21})),
            ("double", json!({"n": "x"})),
        ]))
        .then(FakeReply::text("21 doubled is 42"));
    let double = tool_fn(
        ToolSpec::new(
            "double",
            "Double a number",
            json!({"type": "object", "properties": {"n": {"type": "integer"}}, "required": ["n"]}),
        ),
        |args, _ctx| async move { Ok::<_, ToolError>(json!(args["n"].as_i64().unwrap_or(0) * 2)) },
    );
    let agent = Agent::builder(model).tool(double).build().expect("valid agent");
    let result = agent.run("double 21").await.expect("agent run");
    assert_eq!(result.text(), Some("21 doubled is 42"));
    assert_eq!((result.model_calls, result.tool_calls), (2, 1));
    let statuses: Vec<Option<ToolStatus>> = result
        .messages
        .iter()
        .filter(|m| m.role == agentbuild_providers::Role::Tool)
        .map(|m| m.status)
        .collect();
    assert_eq!(statuses, [Some(ToolStatus::Success), Some(ToolStatus::Error)]);
    println!("  agent loop with tools and argument validation: ok");
}
