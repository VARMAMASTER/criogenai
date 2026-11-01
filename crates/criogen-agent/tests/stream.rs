//! `agent.stream()`: typed events for a UI, then the same result as `run`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use criogen_agent::{
    Agent, AgentEvent, Approval, Decision, Hook, HookContext, Outcome, ToolError, tool_fn,
};
use criogen_providers::{BoxFuture, FakeModel, FakeReply, Role, ToolSpec, ToolStatus};
use serde_json::json;

fn fake(replies: Vec<FakeReply>) -> FakeModel {
    let mut model = FakeModel::with_delay(Duration::from_millis(100), Duration::from_millis(100));
    for reply in replies {
        model = model.then(reply);
    }
    model
}

fn weather() -> impl criogen_agent::Tool {
    tool_fn(
        ToolSpec::new(
            "weather",
            "",
            json!({"type": "object", "properties": {"city": {"type": "string"}}}),
        ),
        |_, _| async { Ok::<_, ToolError>(json!({"celsius": 21})) },
    )
}

/// A compact description of the events a chat UI would show.
fn summarize(event: &AgentEvent) -> Option<String> {
    match event {
        AgentEvent::Text { text, .. } => Some(format!("text:{text}")),
        AgentEvent::ToolCallStarted { tool, args, .. } => Some(format!(
            "start:{tool}:{}",
            serde_json::Value::Object(args.clone())
        )),
        AgentEvent::ToolCallFinished { tool, status, .. } => Some(format!("end:{tool}:{status:?}")),
        AgentEvent::Message(message) => Some(format!("message:{}", message.role.as_str())),
        AgentEvent::ApprovalNeeded(requests) => Some(format!("approval:{}", requests.len())),
        _ => None,
    }
}

#[tokio::test(start_paused = true)]
async fn a_stream_shows_the_loop_and_ends_with_the_result() {
    let agent = Agent::builder(fake(vec![
        FakeReply::tool_call("weather", json!({"city": "Paris"})),
        FakeReply::text("It is 21 °C."),
    ]))
    .tool(weather())
    .build()
    .unwrap();
    let mut stream = agent.stream("Weather in Paris?");
    let mut seen = Vec::new();
    while let Some(event) = stream.next().await {
        if let Some(line) = summarize(&event) {
            seen.push(line);
        }
    }
    let result = stream.finish().await.unwrap();
    assert_eq!(result.text(), Some("It is 21 °C."));
    assert_eq!(
        seen,
        [
            "message:assistant",
            r#"start:weather:{"city":"Paris"}"#,
            "end:weather:Success",
            "text:It is 21 °C.",
            "message:assistant",
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn approval_shows_up_as_an_event() {
    let agent = Agent::builder(fake(vec![
        FakeReply::tool_call("weather", json!({"city": "Oslo"})),
        FakeReply::text("cold"),
    ]))
    .tool(weather())
    .approval(Approval::new().tool("weather"))
    .build()
    .unwrap();
    let mut stream = agent.stream("Oslo?");
    let mut requests = Vec::new();
    while let Some(event) = stream.next().await {
        if let AgentEvent::ApprovalNeeded(found) = event {
            requests = found;
        }
    }
    let paused = stream.finish().await.unwrap();
    assert!(paused.is_suspended());
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].tool, "weather");
    let done = agent
        .resume(
            paused.snapshot().unwrap(),
            [(requests[0].call_id.clone(), Decision::approve())],
        )
        .await
        .unwrap();
    assert_eq!(done.text(), Some("cold"));
}

struct EndRecorder(Arc<Mutex<Vec<Outcome>>>);

impl Hook for EndRecorder {
    fn name(&self) -> &str {
        "end"
    }
    fn on_end<'a>(&'a self, _: &'a HookContext, outcome: Outcome) -> BoxFuture<'a, ()> {
        self.0.lock().unwrap().push(outcome);
        Box::pin(async {})
    }
}

#[tokio::test(start_paused = true)]
async fn a_stream_can_be_cancelled_and_hooks_still_end() {
    let slow = tool_fn(
        ToolSpec::new("slow", "", json!({"type": "object"})),
        |_, _| async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok::<_, ToolError>("late")
        },
    );
    let ends = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::builder(fake(vec![FakeReply::tool_call("slow", json!({}))]))
        .tool(slow)
        .hook(EndRecorder(ends.clone()))
        .build()
        .unwrap();
    let mut stream = agent.stream_with(
        "go",
        agent.run_options().cancel_grace(Duration::from_millis(10)),
    );
    while let Some(event) = stream.next().await {
        if matches!(event, AgentEvent::ToolCallStarted { .. }) {
            stream.cancel();
        }
    }
    let err = stream.finish().await.unwrap_err();
    assert_eq!(err.code, "cancelled");
    assert_eq!(*ends.lock().unwrap(), [Outcome::Failed]);
}

#[tokio::test(start_paused = true)]
async fn the_streamed_result_matches_a_plain_run() {
    let build = || {
        Agent::builder(fake(vec![
            FakeReply::tool_call("weather", json!({"city": "Rome"})),
            FakeReply::text("warm"),
        ]))
        .tool(weather())
        .build()
        .unwrap()
    };
    let plain = build().run("Rome?").await.unwrap();
    let streamed = build().stream("Rome?").finish().await.unwrap();
    let roles = |r: &criogen_agent::AgentResult| -> Vec<Role> {
        r.messages.iter().map(|m| m.role).collect()
    };
    assert_eq!(roles(&plain), roles(&streamed));
    assert_eq!(plain.output, streamed.output);
    assert_eq!(
        streamed
            .messages
            .iter()
            .filter(|m| m.status == Some(ToolStatus::Success))
            .count(),
        1
    );
}
