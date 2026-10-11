//! Human approval of tool calls (design 0004 §7), with `FakeModel` on the
//! paused clock.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentbuild_agent::{Agent, Approval, ApprovalRule, Decision, DecisionKind, ToolError, tool_fn};
use agentbuild_core::Snapshot;
use agentbuild_providers::{FakeModel, FakeReply, Message, Role, ToolSpec, ToolStatus, transcript};
use serde_json::{Map, Value, json};

/// Every executed call, as `name(args)`.
type Log = Arc<Mutex<Vec<String>>>;

fn recording_tool(name: &'static str, log: &Log) -> impl agentbuild_agent::Tool {
    let log = log.clone();
    tool_fn(
        ToolSpec::new(
            name,
            "",
            json!({
                "type": "object",
                "properties": {"to": {"type": "string"}, "amount": {"type": "number"}},
                "additionalProperties": false
            }),
        ),
        move |args, _ctx| {
            let log = log.clone();
            async move {
                log.lock()
                    .unwrap()
                    .push(format!("{name}({})", Value::Object(args.clone())));
                Ok::<_, ToolError>(format!("{name} done"))
            }
        },
    )
}

fn model(replies: Vec<FakeReply>) -> FakeModel {
    let mut model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO);
    for reply in replies {
        model = model.then(reply);
    }
    model
}

/// The model asks to send an email and look something up, then answers.
fn email_agent(log: &Log, approval: Approval) -> Agent {
    Agent::builder(model(vec![
        FakeReply::tool_calls([
            ("send_email", json!({"to": "ceo@example.com"})),
            ("lookup", json!({})),
        ]),
        FakeReply::text("Done."),
    ]))
    .tool(recording_tool("send_email", log))
    .tool(recording_tool("lookup", log))
    .approval(approval)
    .build()
    .unwrap()
}

fn decisions(pairs: &[(&str, Decision)]) -> Vec<(String, Decision)> {
    pairs
        .iter()
        .map(|(id, d)| (id.to_string(), d.clone()))
        .collect()
}

fn tool_results(messages: &[Message]) -> Vec<(String, ToolStatus, String)> {
    messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| (m.tool.clone().unwrap(), m.status.unwrap(), m.text()))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_gated_call_suspends_the_whole_batch_before_anything_runs() {
    let log = Log::default();
    let agent = email_agent(
        &log,
        Approval::new().rule(
            ApprovalRule::new("send_email").describe(|_, args| format!("Email {}", args["to"])),
        ),
    );
    let paused = agent.run("email the CEO").await.unwrap();
    assert!(paused.is_suspended());
    assert!(
        log.lock().unwrap().is_empty(),
        "nothing ran, not even `lookup`"
    );
    let requests = paused.approval_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].call_id, "call_1_0");
    assert_eq!(requests[0].tool, "send_email");
    assert_eq!(requests[0].args["to"], "ceo@example.com");
    assert_eq!(
        requests[0].description.as_deref(),
        Some("Email \"ceo@example.com\"")
    );
    assert_eq!(requests[0].allowed, DecisionKind::ALL);
    assert_eq!(paused.model_calls, 1);

    let done = agent
        .resume(
            paused.snapshot().unwrap(),
            decisions(&[("call_1_0", Decision::approve())]),
        )
        .await
        .unwrap();
    assert_eq!(done.text(), Some("Done."));
    assert_eq!(
        *log.lock().unwrap(),
        [r#"send_email({"to":"ceo@example.com"})"#, "lookup({})"]
    );
    assert!(transcript::check(&done.messages).is_empty());
}

#[tokio::test(start_paused = true)]
async fn edit_runs_the_changed_call_and_tells_the_model() {
    let log = Log::default();
    let agent = email_agent(&log, Approval::new().tool("send_email"));
    let paused = agent.run("email").await.unwrap();
    let edited = Map::from_iter([("to".to_string(), json!("cfo@example.com"))]);
    let done = agent
        .resume(
            paused.snapshot().unwrap(),
            decisions(&[("call_1_0", Decision::edit(edited))]),
        )
        .await
        .unwrap();
    assert_eq!(
        log.lock().unwrap()[0],
        r#"send_email({"to":"cfo@example.com"})"#
    );
    let results = tool_results(&done.messages);
    assert_eq!(results[0].1, ToolStatus::Success);
    assert!(
        results[0]
            .2
            .starts_with("Note: a reviewer changed this call before it ran. It ran as send_email({\"to\":\"cfo@example.com\"}).")
    );
    // The original call stays in the transcript, unchanged.
    let call = done.messages[1].tool_calls().next().unwrap();
    assert_eq!(call.args["to"], "ceo@example.com");
}

#[tokio::test(start_paused = true)]
async fn reject_and_respond_do_not_run_the_tool() {
    let log = Log::default();
    let agent = Agent::builder(model(vec![
        FakeReply::tool_calls([
            ("send_email", json!({"to": "a@x"})),
            ("pay", json!({"amount": 900})),
        ]),
        FakeReply::text("ok"),
    ]))
    .tool(recording_tool("send_email", &log))
    .tool(recording_tool("pay", &log))
    .approval(Approval::new().tool("send_email").tool("pay"))
    .build()
    .unwrap();
    let paused = agent.run("go").await.unwrap();
    assert_eq!(paused.approval_requests().len(), 2);
    let done = agent
        .resume(
            paused.snapshot().unwrap(),
            decisions(&[
                ("call_1_0", Decision::Reject { message: None }),
                (
                    "call_1_1",
                    Decision::respond("Payment already made by phone."),
                ),
            ]),
        )
        .await
        .unwrap();
    assert!(log.lock().unwrap().is_empty(), "neither tool ran");
    assert_eq!(
        tool_results(&done.messages),
        [
            (
                "send_email".into(),
                ToolStatus::Rejected,
                "The reviewer rejected this call. Do not retry it.".into()
            ),
            (
                "pay".into(),
                ToolStatus::Success,
                "Payment already made by phone.".into()
            ),
        ]
    );
    assert_eq!(done.tool_calls, 0);
}

#[tokio::test(start_paused = true)]
async fn only_calls_matching_the_rule_and_predicate_are_gated() {
    let log = Log::default();
    let approval = Approval::new().rule(
        ApprovalRule::new("pay")
            .when(|_, args| args["amount"].as_f64().unwrap_or(0.0) > 100.0)
            .decisions([DecisionKind::Approve, DecisionKind::Reject]),
    );
    let small = Agent::builder(model(vec![
        FakeReply::tool_call("pay", json!({"amount": 20})),
        FakeReply::text("paid"),
    ]))
    .tool(recording_tool("pay", &log))
    .approval(approval.clone())
    .build()
    .unwrap();
    let done = small.run("pay 20").await.unwrap();
    assert!(!done.is_suspended(), "small payments need no approval");
    assert_eq!(done.text(), Some("paid"));

    let large = Agent::builder(model(vec![
        FakeReply::tool_call("pay", json!({"amount": 500})),
        FakeReply::text("paid"),
    ]))
    .tool(recording_tool("pay", &log))
    .approval(approval)
    .build()
    .unwrap();
    let paused = large.run("pay 500").await.unwrap();
    assert_eq!(
        paused.approval_requests()[0].allowed,
        [DecisionKind::Approve, DecisionKind::Reject]
    );
}

#[tokio::test(start_paused = true)]
async fn invalid_decisions_are_rejected_and_the_snapshot_stays_usable() {
    let log = Log::default();
    let agent = Agent::builder(model(vec![
        FakeReply::tool_call("pay", json!({"amount": 500})),
        FakeReply::text("ok"),
    ]))
    .tool(recording_tool("pay", &log))
    .approval(
        Approval::new()
            .rule(ApprovalRule::new("pay").decisions([DecisionKind::Approve, DecisionKind::Edit])),
    )
    .build()
    .unwrap();
    let paused = agent.run("pay").await.unwrap();
    let snapshot = paused.snapshot().unwrap().clone();

    let cases = [
        (decisions(&[]), "no decision for call"),
        (
            decisions(&[("call_1_0", Decision::reject("no"))]),
            "is not allowed",
        ),
        (
            decisions(&[(
                "call_1_0",
                Decision::edit(Map::from_iter([("amount".into(), json!("lots"))])),
            )]),
            "amount: expected number",
        ),
        (
            decisions(&[
                ("call_1_0", Decision::approve()),
                ("call_9", Decision::approve()),
            ]),
            "not waiting for a decision",
        ),
    ];
    for (bad, expected) in cases {
        let err = agent.resume(&snapshot, bad).await.unwrap_err();
        assert_eq!(err.code, "resume_invalid");
        assert!(err.message.contains(expected), "{}", err.message);
        assert!(err.run_error.is_none(), "rejected before anything ran");
    }
    assert!(log.lock().unwrap().is_empty());
    let done = agent
        .resume(&snapshot, decisions(&[("call_1_0", Decision::approve())]))
        .await
        .unwrap();
    assert_eq!(done.text(), Some("ok"));
}

#[tokio::test(start_paused = true)]
async fn approval_survives_json_and_another_process() {
    let log = Log::default();
    let agent = email_agent(&log, Approval::new().tool("send_email"));
    let paused = agent.run("email").await.unwrap();
    let stored = serde_json::to_string(paused.snapshot().unwrap()).unwrap();

    // Later, elsewhere: an agent built the same way resumes the stored JSON.
    let other_log = Log::default();
    let other = Agent::builder(model(vec![FakeReply::text("Sent.")]))
        .tool(recording_tool("send_email", &other_log))
        .tool(recording_tool("lookup", &other_log))
        .approval(Approval::new().tool("send_email"))
        .build()
        .unwrap();
    let snapshot: Snapshot = serde_json::from_str(&stored).unwrap();
    let decisions_json = json!({"call_1_0": {"type": "approve"}});
    let parsed: Vec<(String, Decision)> =
        serde_json::from_value::<Map<String, Value>>(decisions_json)
            .unwrap()
            .into_iter()
            .map(|(id, d)| (id, serde_json::from_value(d).unwrap()))
            .collect();
    let done = other.resume(&snapshot, parsed).await.unwrap();
    assert_eq!(done.text(), Some("Sent."));
    assert_eq!(other_log.lock().unwrap().len(), 2);
    assert!(log.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn resume_needs_a_run_waiting_for_approval() {
    let log = Log::default();
    let agent = Agent::builder(model(vec![FakeReply::text("hi")]))
        .tool(recording_tool("send_email", &log))
        .approval(Approval::new().tool("send_email"))
        .build()
        .unwrap();
    let done = agent.run("hello").await.unwrap();
    assert!(!done.is_suspended());
    // A failure snapshot is not waiting for approval.
    let failing = Agent::builder(model(vec![FakeReply::Error(
        agentbuild_providers::ModelError::new(
            agentbuild_providers::ModelErrorKind::InvalidRequest,
            "bad",
        ),
    )]))
    .tool(recording_tool("send_email", &log))
    .approval(Approval::new().tool("send_email"))
    .build()
    .unwrap();
    let err = failing.run("x").await.unwrap_err();
    let resumed = failing
        .resume(err.snapshot().unwrap(), Vec::new())
        .await
        .unwrap_err();
    assert_eq!(resumed.code, "resume_invalid");
}

#[test]
fn approval_rules_are_checked_at_build_time() {
    let log = Log::default();
    let unknown = Agent::builder(model(vec![]))
        .tool(recording_tool("send_email", &log))
        .approval(Approval::new().tool("delete_*"))
        .build()
        .unwrap_err();
    assert!(unknown.to_string().contains("matches no tool"));
    let empty = Agent::builder(model(vec![]))
        .tool(recording_tool("send_email", &log))
        .approval(Approval::new().rule(ApprovalRule::new("send_email").decisions([])))
        .build()
        .unwrap_err();
    assert!(empty.to_string().contains("allows no decisions"));
}
