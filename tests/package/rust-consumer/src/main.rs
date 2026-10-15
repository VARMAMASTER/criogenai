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
    anthropic_request_body();
    agent_with_tools().await;
    telemetry_never_breaks_a_run().await;
    approval_before_a_tool_runs().await;
    agents_as_nodes_and_tools().await;
    threads_keep_conversations().await;
    sqlite_threads_survive_a_restart().await;
    long_term_memory_per_user().await;
    postgres_threads_across_processes().await;
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

/// The Anthropic protocol: the exact body, without sending it.
fn anthropic_request_body() {
    use agentbuild_providers::{
        AnthropicOptions, ContentBlock, Endpoint, Reasoning, ReasoningEffort, Role, Settings,
        preview_request,
    };
    let endpoint = Endpoint::anthropic("sk-ant-unused")
        .with_anthropic_options(AnthropicOptions::default().default_max_tokens(2000));
    let system = Message::with_content(
        Role::System,
        vec![
            ContentBlock::text("You are a careful assistant.")
                .with_extra("anthropic", json!({"cache_control": {"type": "ephemeral"}})),
        ],
    );
    let mut settings = Settings::default();
    settings.reasoning = Some(Reasoning::Effort {
        effort: ReasoningEffort::Low,
    });
    let request =
        ModelRequest::new(vec![system, Message::user("hi")]).with_settings(settings);
    let (body, warnings) = preview_request(&endpoint, "claude-test", &request, true).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(body["thinking"]["type"], "adaptive");
    assert_eq!(body["output_config"]["effort"], "low");
    assert_eq!(body["max_tokens"], 2000 + 2048);
    assert_eq!(body["stream"], true);
    println!("  anthropic request body (cache, adaptive thinking): ok");
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

/// The telemetry crate: an exporter whose backend is down must not affect
/// the run.
async fn telemetry_never_breaks_a_run() {
    use agentbuild_telemetry::{Langfuse, OtlpExporter};

    let config = Langfuse::config("pk-lf-x", "sk-lf-x", "http://127.0.0.1:9")
        .batching(100, Duration::from_millis(10));
    let exporter = OtlpExporter::start(config).expect("exporter");
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("a", node_fn(|_| async { Ok(Update::new().set("ok", true)) }))
        .set_entry("a")
        .add_edge("a", END);
    let out = builder
        .build()
        .unwrap()
        .run_with(State::new(), RunOptions::default().observe(exporter.observer()))
        .await
        .expect("run unaffected by telemetry");
    assert_eq!(out.state.get("ok").unwrap(), true);
    exporter.shutdown().await;
    assert!(exporter.stats().failed > 0, "{:?}", exporter.stats());
    println!("  telemetry export (unreachable backend): ok");
}

/// Human approval: the run suspends before a gated tool runs.
async fn approval_before_a_tool_runs() {
    use agentbuild_agent::{Agent, Approval, Decision, ToolError, tool_fn};

    let model = FakeModel::with_delay(Duration::from_millis(1), Duration::from_millis(1))
        .then(FakeReply::tool_call("refund", json!({"amount": 50})))
        .then(FakeReply::text("Refund handled."));
    let refunds = Arc::new(AtomicUsize::new(0));
    let refund = {
        let refunds = refunds.clone();
        tool_fn(
            ToolSpec::new("refund", "Refund", json!({"type": "object", "properties": {"amount": {"type": "number"}}})),
            move |_, _| {
                let refunds = refunds.clone();
                async move {
                    refunds.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, ToolError>("refunded")
                }
            },
        )
    };
    let agent = Agent::builder(model)
        .tool(refund)
        .approval(Approval::new().tool("refund"))
        .build()
        .expect("valid agent");
    let paused = agent.run("refund me").await.expect("run");
    assert!(paused.is_suspended());
    assert_eq!(refunds.load(Ordering::SeqCst), 0);
    let request = &paused.approval_requests()[0];
    let done = agent
        .resume(paused.snapshot().unwrap(), [(request.call_id.clone(), Decision::approve())])
        .await
        .expect("resume");
    assert_eq!(done.text(), Some("Refund handled."));
    assert_eq!(refunds.load(Ordering::SeqCst), 1);
    println!("  human approval (suspend, decide, resume): ok");
}

/// An agent as a workflow node, and as another agent's tool.
async fn agents_as_nodes_and_tools() {
    use agentbuild_agent::Agent;

    let quick = |replies: Vec<FakeReply>| {
        let mut model = FakeModel::with_delay(Duration::from_millis(1), Duration::from_millis(1));
        for reply in replies {
            model = model.then(reply);
        }
        model
    };
    let expert = Agent::builder(quick(vec![FakeReply::text("42")])).build().unwrap();
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("expert", expert.as_node().output_key("answer"))
        .set_entry("expert")
        .add_edge("expert", END);
    let out = builder
        .build()
        .unwrap()
        .run(State::new().with("messages", "What is six times seven?"))
        .await
        .expect("workflow with an agent node");
    assert_eq!(out.state.get("answer").unwrap(), "42");

    let sub = Agent::builder(quick(vec![FakeReply::text("sub-answer")])).build().unwrap();
    let lead = Agent::builder(quick(vec![
        FakeReply::tool_call("ask_sub", json!({"input": "help"})),
        FakeReply::text("lead-answer"),
    ]))
    .tool(sub.as_tool("ask_sub", "Ask the sub-agent"))
    .build()
    .unwrap();
    let mut stream = lead.stream("go");
    let mut texts = Vec::new();
    while let Some(event) = stream.next().await {
        if let agentbuild_agent::AgentEvent::Text { text, .. } = event {
            texts.push(text);
        }
    }
    let result = stream.finish().await.expect("lead agent");
    assert_eq!(result.text(), Some("lead-answer"));
    assert_eq!(texts, ["lead-answer"]);
    println!("  agents as workflow nodes and as tools: ok");
}

/// Threads: a conversation and a pending approval kept in storage, used by
/// a second agent instance (another process, in real life).
async fn threads_keep_conversations() {
    use agentbuild_agent::{Agent, Approval, ToolError, tool_fn};
    use agentbuild_storage::{CheckpointStatus, OnBusy, Source, Storage};

    let storage = Storage::in_memory().on_busy(OnBusy::Reject);
    let refund = || {
        tool_fn(
            ToolSpec::new("refund", "Refund", json!({"type": "object"})),
            |_, _| async { Ok::<_, ToolError>("refunded") },
        )
    };
    let fast = || FakeModel::with_delay(Duration::from_millis(1), Duration::from_millis(1));
    let first = Agent::builder(
        fast()
            .then(FakeReply::text("Hello Ada"))
            .then(FakeReply::tool_call("refund", json!({}))),
    )
    .tool(refund())
    .approval(Approval::new().tool("refund"))
    .storage(storage.clone())
    .build()
    .expect("valid agent");
    let chat = first.thread("user-42");
    chat.send("I am Ada").await.expect("send");
    let paused = chat.send("Refund my order").await.expect("send");
    assert!(paused.needs_approval());
    assert_eq!(chat.send("hello?").await.unwrap_err().code, "thread_suspended");

    let second = Agent::builder(fast().then(FakeReply::text("Refunded, Ada.")))
        .tool(refund())
        .approval(Approval::new().tool("refund"))
        .storage(storage)
        .build()
        .expect("valid agent");
    let chat = second.thread("user-42");
    let done = chat.approve().await.expect("approve");
    assert_eq!(done.text(), Some("Refunded, Ada."));
    assert_eq!(done.result.messages.len(), 6);
    let history = chat.history(20).await.expect("history");
    let kinds: Vec<(Source, CheckpointStatus)> =
        history.iter().map(|c| (c.source, c.status)).collect();
    // Newest first; each call starts with an `input` checkpoint.
    assert_eq!(kinds[0], (Source::Step, CheckpointStatus::Completed));
    assert!(kinds.contains(&(Source::Suspend, CheckpointStatus::Suspended)));
    assert_eq!(
        kinds.last(),
        Some(&(Source::Input, CheckpointStatus::InProgress))
    );
    // Back to the end of the first call.
    let first_turn = history
        .iter()
        .rev()
        .find(|c| c.status == CheckpointStatus::Completed)
        .expect("a completed call");
    chat.rewind(&first_turn.id).await.expect("rewind");
    assert_eq!(chat.messages().await.expect("messages").len(), 2);
    println!("  threads (memory, approval across instances, rewind): ok");
}

async fn sqlite_threads_survive_a_restart() {
    use agentbuild_agent::Agent;
    use agentbuild_storage::{SqliteCheckpointer, Storage};

    let path = std::env::temp_dir().join(format!("agentbuild-package-{}.db", std::process::id()));
    let fast = || FakeModel::with_delay(Duration::from_millis(1), Duration::from_millis(1));
    {
        let storage = Storage::connect(&format!("sqlite://{}", path.display()))
            .await
            .expect("open");
        let agent = Agent::builder(fast().then(FakeReply::text("Hello Ada")))
            .storage(storage)
            .build()
            .expect("valid agent");
        agent.thread("t").send("I am Ada").await.expect("send");
    }
    let storage = Storage::new(SqliteCheckpointer::open(&path).await.expect("reopen"));
    let agent = Agent::builder(fast().then(FakeReply::text("Your name is Ada")))
        .storage(storage)
        .build()
        .expect("valid agent");
    let reply = agent.thread("t").send("Who am I?").await.expect("send");
    assert_eq!(reply.result.messages.len(), 4);
    assert_eq!(reply.text(), Some("Your name is Ada"));
    drop(agent);
    for suffix in ["", "-wal", "-shm"] {
        let mut file = path.clone().into_os_string();
        file.push(suffix);
        let _ = std::fs::remove_file(file);
    }
    println!("  threads in SQLite survive a restart: ok");
}

async fn long_term_memory_per_user() {
    use agentbuild_agent::{Agent, AgentInput, Memory, REMEMBER_TOOL};
    use agentbuild_storage::{Namespace, SearchQuery, SqliteMemoryStore, Storage};

    let storage = Storage::in_memory()
        .with_memory(SqliteMemoryStore::connect("sqlite::memory:").await.expect("open"));
    let users = Namespace::parse("users").expect("valid");
    let model = Arc::new(
        FakeModel::with_delay(Duration::ZERO, Duration::ZERO)
            .then(FakeReply::tool_call(
                REMEMBER_TOOL,
                json!({"key": "diet", "value": "vegetarian"}),
            ))
            .then(FakeReply::text("Noted."))
            .then(FakeReply::text("Try the lentil soup.")),
    );
    let agent = Agent::builder_shared(model.clone())
        .storage(storage.clone())
        .memory(Memory::by_state("user_id", users.clone()))
        .build()
        .expect("valid agent");
    agent
        .run(AgentInput::from("I am vegetarian").with("user_id", "42"))
        .await
        .expect("run");
    agent
        .run(AgentInput::from("What should I eat for my diet?").with("user_id", "42"))
        .await
        .expect("run");
    let recalled = &model.requests()[2].messages[0];
    assert!(recalled.text().contains("- diet: vegetarian"));
    // No user, no memory: the run fails instead of sharing one.
    assert_eq!(agent.run("hi").await.unwrap_err().code, "invalid_state");

    let found = storage
        .memory()
        .search(&users, &SearchQuery::new().text("vegetarian"))
        .await
        .expect("search");
    assert_eq!(found.len(), 1);
    assert_eq!(
        storage
            .memory()
            .delete_namespace(&users.child("42").expect("valid"))
            .await
            .expect("delete"),
        1
    );
    println!("  long-term memory per user (recall, tools, SQLite store): ok");
}

/// Two processes (two `Storage::connect`) on one Postgres thread: the second
/// send waits its turn. Needs a server (`scripts/with-postgres.sh`).
async fn postgres_threads_across_processes() {
    use agentbuild_agent::Agent;
    use agentbuild_storage::{PostgresCheckpointer, PostgresOptions, Storage};

    // Always compiled; run when a server is configured.
    let _options = PostgresOptions::new().max_connections(4).lock_connections(8);
    let Ok(url) = std::env::var("AGENTBUILD_TEST_POSTGRES_URL") else {
        println!("  Postgres threads across processes: skipped (no AGENTBUILD_TEST_POSTGRES_URL)");
        return;
    };
    let thread = format!("package-{}", std::process::id());
    let process = |reply: &'static str| {
        let url = url.clone();
        async move {
            let model = FakeModel::with_delay(Duration::from_millis(200), Duration::from_millis(200))
                .then(FakeReply::text(reply));
            Agent::builder(model)
                .storage(Storage::connect(&url).await.expect("connect"))
                .build()
                .expect("valid agent")
        }
    };
    let (a, b) = (process("one").await, process("two").await);
    let first = {
        let chat = a.thread(&thread);
        tokio::spawn(async move { chat.send("first").await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let second = b.thread(&thread).send("second").await.expect("waits its turn");
    first.await.expect("joined").expect("first send");
    assert_eq!(second.result.messages.len(), 4);
    assert!(b.thread(&thread).delete().await.expect("delete"));
    let store = PostgresCheckpointer::connect(&url).await.expect("connect");
    assert_eq!(store.schema_version().await.expect("version"), 2);
    store.close().await;
    println!("  Postgres threads across processes: ok");
}
