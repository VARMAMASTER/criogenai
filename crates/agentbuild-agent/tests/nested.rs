//! Agents as workflow nodes and as tools (design 0004 §1).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use agentbuild_agent::{Agent, Approval, Limits, ToolError, tool_fn};
use agentbuild_core::{END, State, Update, WorkflowBuilder, node_fn};
use agentbuild_providers::{
    FakeModel, FakeReply, Message, ModelError, ModelErrorKind, Role, ToolSpec, ToolStatus,
};
use serde_json::{Value, json};

fn fake(replies: Vec<FakeReply>) -> FakeModel {
    let mut model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO);
    for reply in replies {
        model = model.then(reply);
    }
    model
}

fn counting_tool(name: &'static str, runs: &Arc<AtomicUsize>) -> impl agentbuild_agent::Tool {
    let runs = runs.clone();
    tool_fn(
        ToolSpec::new(name, "", json!({"type": "object"})),
        move |_, _| {
            let runs = runs.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ToolError>(format!("{name} done"))
            }
        },
    )
}

#[tokio::test(start_paused = true)]
async fn an_agent_is_a_node_of_a_larger_workflow() {
    let agent = Agent::builder(fake(vec![FakeReply::text(
        "Paris is the capital of France.",
    )]))
    .build()
    .unwrap();
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "ask",
            node_fn(|_| async { Ok(Update::new().set("chat", "What is the capital of France?")) }),
        )
        .add_node(
            "expert",
            agent.as_node().messages_key("chat").output_key("answer"),
        )
        .add_node(
            "shout",
            node_fn(|state| async move {
                let answer = state
                    .get("answer")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                Ok(Update::new().set("loud", answer.to_uppercase()))
            }),
        )
        .set_entry("ask")
        .add_edge("ask", "expert")
        .add_edge("expert", "shout")
        .add_edge("shout", END);
    let out = builder.build().unwrap().run(State::new()).await.unwrap();
    assert_eq!(
        out.state.get("loud").unwrap(),
        "PARIS IS THE CAPITAL OF FRANCE."
    );
    let chat: Vec<Message> =
        serde_json::from_value(out.state.get("chat").unwrap().clone()).unwrap();
    assert_eq!(chat.len(), 2, "the question and the answer");
}

#[tokio::test(start_paused = true)]
async fn approval_inside_an_agent_node_suspends_the_workflow() {
    let refunds = Arc::new(AtomicUsize::new(0));
    let agent = Agent::builder(fake(vec![
        FakeReply::tool_call("refund", json!({})),
        FakeReply::text("Refunded."),
    ]))
    .tool(counting_tool("refund", &refunds))
    .approval(Approval::new().tool("refund"))
    .build()
    .unwrap();
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("support", agent.as_node().output_key("reply"))
        .set_entry("support")
        .add_edge("support", END);
    let workflow = builder.build().unwrap();

    let input = State::new().with("messages", "Please refund order 7");
    let paused = workflow.run(input).await.unwrap();
    assert!(paused.is_suspended());
    let interrupt = &paused.interrupts()[0];
    assert_eq!(interrupt.node, "support");
    assert_eq!(interrupt.payload["kind"], "agent_approval");
    let call_id = interrupt.payload["requests"][0]["call_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(refunds.load(Ordering::SeqCst), 0);

    // The workflow snapshot is plain JSON; the agent's own snapshot rides
    // inside it.
    let stored = serde_json::to_string(paused.snapshot().unwrap()).unwrap();
    let snapshot = serde_json::from_str(&stored).unwrap();
    let done = workflow
        .resume(
            &snapshot,
            [("support", json!({call_id: {"type": "approve"}}))],
        )
        .await
        .unwrap();
    assert_eq!(done.state.get("reply").unwrap(), "Refunded.");
    assert_eq!(refunds.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn retrying_the_workflow_retries_the_agent_without_repeats() {
    let emails = Arc::new(AtomicUsize::new(0));
    let bug = Arc::new(AtomicBool::new(true));
    let lookup = {
        let bug = bug.clone();
        tool_fn(
            ToolSpec::new("lookup", "", json!({"type": "object"})),
            move |_, _| {
                let bug = bug.clone();
                async move {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    if bug.load(Ordering::SeqCst) {
                        return Err(ToolError::fatal("index corrupt"));
                    }
                    Ok::<_, ToolError>("found")
                }
            },
        )
    };
    let agent = Agent::builder(fake(vec![
        FakeReply::tool_calls([("send_email", json!({})), ("lookup", json!({}))]),
        FakeReply::text("All done."),
    ]))
    .tool(counting_tool("send_email", &emails))
    .tool(lookup)
    .build()
    .unwrap();
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("worker", agent.as_node().output_key("result"))
        .set_entry("worker")
        .add_edge("worker", END);
    let workflow = builder.build().unwrap();

    let err = workflow
        .run(State::new().with("messages", "email and look up"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "node_failed");
    assert_eq!(emails.load(Ordering::SeqCst), 1);
    bug.store(false, Ordering::SeqCst);
    let done = workflow.retry(err.snapshot().unwrap()).await.unwrap();
    assert_eq!(done.state.get("result").unwrap(), "All done.");
    assert_eq!(
        emails.load(Ordering::SeqCst),
        1,
        "the email was not sent twice"
    );
    // The agent continued its failed turn (not a fresh start): both calls
    // and their results are in the transcript.
    let transcript: Vec<Message> =
        serde_json::from_value(done.state.get("messages").unwrap().clone()).unwrap();
    let roles: Vec<&str> = transcript.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, ["user", "assistant", "tool", "tool", "assistant"]);
}

fn researcher(replies: Vec<FakeReply>, limits: Limits) -> Agent {
    Agent::builder(fake(replies))
        .instructions("You research things.")
        .tool(tool_fn(
            ToolSpec::new("search", "", json!({"type": "object"})),
            |_, _| async { Ok::<_, ToolError>("3 results") },
        ))
        .limits(limits)
        .build()
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn an_agent_is_a_tool_of_another_agent() {
    let sub = researcher(
        vec![
            FakeReply::tool_call("search", json!({})),
            FakeReply::text("Rust is fast."),
        ],
        Limits::default(),
    );
    let lead_model = Arc::new(fake(vec![
        FakeReply::tool_call("research", json!({"input": "Why is Rust fast?"})),
        FakeReply::text("Summary: Rust is fast."),
    ]));
    let lead = Agent::builder_shared(lead_model.clone())
        .tool(sub.as_tool("research", "Researches a question"))
        .build()
        .unwrap();
    let result = lead.run("Tell me about Rust").await.unwrap();
    assert_eq!(result.text(), Some("Summary: Rust is fast."));
    let tool_result = result
        .messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .unwrap();
    assert_eq!(tool_result.text(), "Rust is fast.");
    assert_eq!(
        lead_model.requests()[0].tools[0].input_schema["required"],
        json!(["input"])
    );
}

#[tokio::test(start_paused = true)]
async fn a_sub_agent_limit_is_a_tool_error_the_lead_can_handle() {
    let looping: Vec<FakeReply> = (0..5)
        .map(|_| FakeReply::tool_call("search", json!({})))
        .collect();
    let sub = researcher(looping, Limits::default().model_calls(2));
    let lead = Agent::builder(fake(vec![
        FakeReply::tool_call("research", json!({"input": "x"})),
        FakeReply::text("I could not finish the research."),
    ]))
    .tool(sub.as_tool("research", ""))
    .build()
    .unwrap();
    let result = lead.run("go").await.unwrap();
    let tool_result = result
        .messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .unwrap();
    assert_eq!(tool_result.status, Some(ToolStatus::Error));
    assert!(tool_result.text().contains("sub-agent stopped"));
    assert_eq!(result.text(), Some("I could not finish the research."));
}

#[tokio::test(start_paused = true)]
async fn a_sub_agent_auth_failure_fails_the_lead() {
    let sub = researcher(
        vec![FakeReply::Error(ModelError::new(
            ModelErrorKind::Auth,
            "bad key",
        ))],
        Limits::default(),
    );
    let lead = Agent::builder(fake(vec![FakeReply::tool_call(
        "research",
        json!({"input": "x"}),
    )]))
    .tool(sub.as_tool("research", ""))
    .build()
    .unwrap();
    let err = lead.run("go").await.unwrap_err();
    assert_eq!(err.code, "tool_failed");
    assert!(err.message.contains("bad key"));
}

#[tokio::test(start_paused = true)]
async fn nesting_is_limited() {
    // lead → middle → leaf; the leaf allows only one level above it.
    let leaf_model = Arc::new(fake(vec![FakeReply::text("leaf answer")]));
    let leaf = Agent::builder_shared(leaf_model.clone())
        .limits(Limits::default().nesting(1))
        .build()
        .unwrap();
    let middle = Agent::builder(fake(vec![
        FakeReply::tool_call("leaf", json!({"input": "x"})),
        FakeReply::text("middle gave up"),
    ]))
    .tool(leaf.as_tool("leaf", ""))
    .build()
    .unwrap();
    let lead = Agent::builder(fake(vec![
        FakeReply::tool_call("middle", json!({"input": "x"})),
        FakeReply::text("lead done"),
    ]))
    .tool(middle.as_tool("middle", ""))
    .build()
    .unwrap();
    let result = lead.run("go").await.unwrap();
    assert_eq!(result.text(), Some("lead done"));
    let middle_result = result
        .messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .unwrap();
    assert_eq!(middle_result.text(), "middle gave up");
    assert_eq!(
        leaf_model.calls(),
        0,
        "the leaf was refused before calling its model"
    );

    // Directly under the lead, the same leaf is within its limit.
    let lead = Agent::builder(fake(vec![
        FakeReply::tool_call("leaf", json!({"input": "x"})),
        FakeReply::text("ok"),
    ]))
    .tool(leaf.as_tool("leaf", ""))
    .build()
    .unwrap();
    let result = lead.run("go").await.unwrap();
    assert_eq!(
        result
            .messages
            .iter()
            .find(|m| m.role == Role::Tool)
            .unwrap()
            .text(),
        "leaf answer"
    );
}
