//! `thread.stream` (design 0008 §17): a thread operation's events as they
//! happen, the saved reply at the end, and a dropped stream that still
//! saves the thread.

use std::sync::Arc;
use std::time::Duration;

use agentbuild_agent::{Agent, AgentEvent, Approval, Decision, ToolError, tool_fn};
use agentbuild_providers::{FakeModel, FakeReply, ToolSpec, ToolStatus};
use agentbuild_storage::{AuditQuery, CheckpointStatus, Storage};
use serde_json::json;

fn instant(replies: Vec<FakeReply>) -> FakeModel {
    let mut model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO);
    for reply in replies {
        model = model.then(reply);
    }
    model
}

fn refund() -> impl agentbuild_agent::Tool {
    tool_fn(
        ToolSpec::new("refund", "", json!({"type": "object"})),
        |_args, _ctx| async { Ok::<_, ToolError>("refunded") },
    )
}

async fn collect(stream: &mut agentbuild_agent::ThreadStream) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        if !matches!(event, AgentEvent::Other(_)) {
            events.push(event);
        }
    }
    events
}

#[tokio::test]
async fn a_streamed_send_shows_the_text_and_saves_the_reply() {
    let agent = Agent::builder(instant(vec![
        FakeReply::text("Hello Ada"),
        FakeReply::text("Your name is Ada"),
    ]))
    .storage(Storage::in_memory())
    .build()
    .unwrap();
    let chat = agent.thread("t");
    let mut stream = chat.stream("I am Ada");
    let events = collect(&mut stream).await;
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello Ada");
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Message(_))));
    let reply = stream.finish().await.unwrap();
    assert_eq!(reply.text(), Some("Hello Ada"));
    // Saved like `send`: the next turn sees it.
    let next = chat.send("Who am I?").await.unwrap();
    assert_eq!(next.result.messages.len(), 4);
    assert_eq!(chat.head().await.unwrap().unwrap().version, next.version);
}

#[tokio::test]
async fn approvals_can_be_streamed_on_both_sides() {
    let storage = Storage::in_memory().with_audit();
    let agent = Agent::builder(instant(vec![
        FakeReply::tool_call("refund", json!({"order": "A-1"})),
        FakeReply::text("Refunded."),
    ]))
    .tool(refund())
    .approval(Approval::new().tool("refund"))
    .storage(storage.clone())
    .build()
    .unwrap();
    let chat = agent.thread("t").actor("user:ada");
    let mut stream = chat.stream("refund A-1");
    let events = collect(&mut stream).await;
    let Some(AgentEvent::ApprovalNeeded(requests)) = events.last() else {
        panic!("{events:?}");
    };
    assert_eq!(requests[0].tool, "refund");
    let reply = stream.finish().await.unwrap();
    assert!(reply.needs_approval());

    // While it waits, a second stream fails before its run: no events.
    let mut refused = chat.stream("hello?");
    assert!(collect(&mut refused).await.is_empty());
    assert_eq!(refused.finish().await.unwrap_err().code, "thread_suspended");

    let mut approved = agent.thread("t").actor("user:boss").stream_approve();
    let events = collect(&mut approved).await;
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallFinished { tool, status: ToolStatus::Success, .. } if tool == "refund"
    )));
    let reply = approved.finish().await.unwrap();
    assert_eq!(reply.text(), Some("Refunded."));
    // Audited like the non-streaming calls.
    let decided = storage
        .audit()
        .unwrap()
        .query(&AuditQuery::new().action("approval.decided"))
        .await
        .unwrap();
    assert_eq!(decided[0].event.actor.as_deref(), Some("user:boss"));
}

#[tokio::test]
async fn reject_and_decide_stream_too() {
    let agent = Agent::builder(instant(vec![
        FakeReply::tool_call("refund", json!({})),
        FakeReply::text("Not refunded."),
        FakeReply::tool_call("refund", json!({})),
        FakeReply::text("Refunded after all."),
    ]))
    .tool(refund())
    .approval(Approval::new().tool("refund"))
    .build()
    .unwrap();
    let chat = agent.thread("t");
    chat.send("refund").await.unwrap();
    let reply = chat.stream_reject("no").finish().await.unwrap();
    assert_eq!(reply.text(), Some("Not refunded."));
    let pending = chat.send("please").await.unwrap().approvals();
    let reply = chat
        .stream_decide([(pending[0].call_id.clone(), Decision::approve())])
        .finish()
        .await
        .unwrap();
    assert_eq!(reply.text(), Some("Refunded after all."));
}

#[tokio::test]
async fn a_dropped_stream_cancels_the_run_and_the_thread_is_saved_for_retry() {
    let model = Arc::new(
        FakeModel::with_delay(Duration::from_millis(400), Duration::from_millis(400))
            .then(FakeReply::text("too late"))
            .then(FakeReply::text("here it is")),
    );
    let agent = Agent::builder_shared(model.clone())
        .storage(Storage::in_memory())
        .build()
        .unwrap();
    let chat = agent.thread("t");
    let stream = chat.stream("hi");
    // The client goes away while the model is answering.
    while model.requests().is_empty() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    drop(stream);
    // The run is cancelled and saved as failed, not left in progress.
    let mut head = None;
    for _ in 0..200 {
        head = chat.head().await.unwrap();
        if head
            .as_ref()
            .is_some_and(|h| h.status() == CheckpointStatus::Failed)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let head = head.unwrap();
    assert_eq!(head.status(), CheckpointStatus::Failed);
    assert_eq!(head.snapshot.unwrap().error.unwrap().code, "cancelled");
    let reply = chat.stream_retry().finish().await.unwrap();
    assert!(reply.text().is_some());
    assert_eq!(
        chat.head().await.unwrap().unwrap().status(),
        CheckpointStatus::Completed
    );
}

#[tokio::test]
async fn cancel_ends_the_stream_with_cancelled() {
    let model = Arc::new(
        FakeModel::with_delay(Duration::from_secs(5), Duration::from_secs(5))
            .then(FakeReply::text("never")),
    );
    let agent = Agent::builder_shared(model.clone()).build().unwrap();
    let chat = agent.thread("t");
    let mut stream = chat.stream("hi");
    while model.requests().is_empty() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let started = tokio::time::Instant::now();
    stream.cancel();
    while stream.next().await.is_some() {}
    assert_eq!(stream.finish().await.unwrap_err().code, "cancelled");
    // The model call in progress is stopped, not waited for.
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}
