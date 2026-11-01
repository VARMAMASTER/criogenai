//! Workflows that call a (simulated) LLM.
//!
//! `FakeLlm` takes 2–3 seconds per call, like a real model. These tests run on
//! Tokio's paused clock (`start_paused = true`): sleeps finish instantly in
//! wall time, but `Instant` still advances by the simulated amount, so timing
//! claims (parallel calls overlap, sequential calls add up) are checked
//! exactly without slowing the suite down.

#[path = "support/fake_llm.rs"]
mod fake_llm;

use std::sync::Arc;
use std::time::Duration;

use criogen_core::{
    END, Node, NodeOutput, RunErrorKind, State, Update, WorkflowBuilder, node_fn, node_with_context,
};
use fake_llm::FakeLlm;
use serde_json::{Value, json};
use tokio::time::Instant;

/// A node that sends `prompt(state)` to `llm` and stores the reply under `key`.
fn ask<P>(llm: &Arc<FakeLlm>, key: &'static str, prompt: P) -> impl Node
where
    P: Fn(&State) -> String + Send + Sync + 'static,
{
    let llm = llm.clone();
    node_fn(move |state| {
        let llm = llm.clone();
        let prompt = prompt(&state);
        async move { Ok(Update::new().set(key, llm.complete(&prompt).await?)) }
    })
}

fn text(state: &State, key: &str) -> String {
    state
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn names(out: &criogen_core::RunOutput) -> Vec<Vec<&str>> {
    out.steps
        .iter()
        .map(|s| s.nodes.iter().map(String::as_str).collect())
        .collect()
}

#[tokio::test(start_paused = true)]
async fn fake_llm_replies_after_two_to_three_seconds() {
    let llm = FakeLlm::new();
    for prompt in ["hi", "summarise this article", "write a haiku about Rust"] {
        let start = Instant::now();
        let reply = llm.complete(prompt).await.unwrap();
        let took = start.elapsed();

        assert!(
            (Duration::from_secs(2)..=Duration::from_secs(3)).contains(&took),
            "{prompt:?} took {took:?}"
        );
        assert_eq!(took, llm.delay_for(prompt), "delay is fixed per prompt");
        assert!(reply.contains(prompt));
    }
    assert_eq!(llm.calls(), 3);
}

#[tokio::test(start_paused = true)]
async fn sequential_llm_calls_add_up_and_see_earlier_replies() {
    let llm = Arc::new(FakeLlm::new());
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "plan",
            ask(&llm, "plan", |s| {
                format!("plan an essay on {}", text(s, "topic"))
            }),
        )
        .add_node(
            "write",
            ask(&llm, "essay", |s| {
                format!("write using: {}", text(s, "plan"))
            }),
        )
        .set_entry("plan")
        .add_edge("plan", "write")
        .add_edge("write", END);
    let workflow = builder.build().unwrap();

    let start = Instant::now();
    let out = workflow
        .run(State::new().with("topic", "supersteps"))
        .await
        .unwrap();
    let took = start.elapsed();

    let prompts = llm.prompts();
    assert_eq!(prompts.len(), 2);
    assert!(
        prompts[1].contains("[fake-llm] reply to: plan an essay on supersteps"),
        "the second call sees the first reply: {}",
        prompts[1]
    );
    assert_eq!(
        took,
        llm.delay_for(&prompts[0]) + llm.delay_for(&prompts[1])
    );
    assert!(text(&out.state, "essay").starts_with("[fake-llm]"));
}

#[tokio::test(start_paused = true)]
async fn parallel_llm_calls_overlap() {
    let llm = Arc::new(FakeLlm::new());
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("start", node_fn(|_| async { Ok(Update::new()) }))
        .add_node("pros", ask(&llm, "pros", |_| "list the pros".into()))
        .add_node("cons", ask(&llm, "cons", |_| "list the cons".into()))
        .add_node("risks", ask(&llm, "risks", |_| "list the risks".into()))
        .add_node(
            "combine",
            node_fn(|state| async move {
                let parts = ["pros", "cons", "risks"].map(|k| text(&state, k));
                Ok(Update::new().set("report", parts.join("\n")))
            }),
        )
        .set_entry("start")
        .add_edge("start", "pros")
        .add_edge("start", "cons")
        .add_edge("start", "risks")
        .add_join(["pros", "cons", "risks"], "combine")
        .add_edge("combine", END);
    let workflow = builder.build().unwrap();

    let start = Instant::now();
    let out = workflow.run(State::new()).await.unwrap();
    let took = start.elapsed();

    let delays: Vec<Duration> = llm.prompts().iter().map(|p| llm.delay_for(p)).collect();
    assert_eq!(delays.len(), 3);
    let slowest = *delays.iter().max().unwrap();
    let total: Duration = delays.iter().sum();
    assert_eq!(took, slowest, "the step takes as long as its slowest call");
    assert!(took < total, "calls overlap: {took:?} < {total:?}");

    assert_eq!(
        names(&out),
        vec![
            vec!["start"],
            vec!["pros", "cons", "risks"],
            vec!["combine"]
        ]
    );
    assert_eq!(text(&out.state, "report").lines().count(), 3);
}

#[tokio::test(start_paused = true)]
async fn failed_llm_call_aborts_the_step_and_commits_nothing() {
    let llm = Arc::new(FakeLlm::new().failing_on("FAIL"));
    // A much slower model on the other branch: fail-fast must not wait for it.
    let slow = Arc::new(FakeLlm::with_delay(
        Duration::from_secs(30),
        Duration::from_secs(30),
    ));
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "start",
            node_fn(|_| async { Ok(Update::new().set("started", true)) }),
        )
        .add_node("good", ask(&slow, "good", |_| "fine prompt".into()))
        .add_node("bad", ask(&llm, "bad", |_| "please FAIL".into()))
        .add_node("combine", node_fn(|_| async { Ok(Update::new()) }))
        .set_entry("start")
        .add_edge("start", "good")
        .add_edge("start", "bad")
        .add_join(["good", "bad"], "combine")
        .add_edge("combine", END);
    let workflow = builder.build().unwrap();

    let start = Instant::now();
    let err = workflow.run(State::new()).await.unwrap_err();
    let took = start.elapsed();

    assert_eq!(err.kind.code(), "node_failed");
    match &err.kind {
        RunErrorKind::NodeFailed { node, source } => {
            assert_eq!(node, "bad");
            assert_eq!(source.kind.as_deref(), Some("fake_llm_error"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(err.step, 1);
    assert_eq!(
        took,
        llm.delay_for("please FAIL"),
        "did not wait for `good`"
    );
    assert_eq!(err.state.get("started"), Some(&json!(true)));
    assert!(
        err.state.get("good").is_none(),
        "failed step commits nothing"
    );
}

#[tokio::test(start_paused = true)]
async fn caller_can_bound_a_slow_run_with_a_timeout() {
    // Per-node timeouts are designed (0005 §4) but not built; until then the
    // caller can bound the whole run. Dropping the run cancels the call.
    let llm = Arc::new(FakeLlm::new());
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("answer", ask(&llm, "answer", |_| "slow question".into()))
        .set_entry("answer")
        .add_edge("answer", END);
    let workflow = builder.build().unwrap();

    let start = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(1), workflow.run(State::new())).await;

    assert!(result.is_err(), "timed out before the 2–3 s reply");
    assert_eq!(start.elapsed(), Duration::from_secs(1));
    assert_eq!(llm.calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn resume_does_not_repeat_finished_llm_calls() {
    let llm = Arc::new(FakeLlm::new());
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "draft",
            ask(&llm, "email", |_| "draft a refund email".into()),
        )
        .add_node(
            "review",
            node_with_context(|ctx, state| async move {
                Ok(match ctx.resume() {
                    None => {
                        NodeOutput::suspend(Update::new(), json!({ "email": state.get("email") }))
                    }
                    Some(answer) if answer == "approve" => {
                        NodeOutput::goto(Update::new(), ["send"])
                    }
                    Some(_) => NodeOutput::goto(Update::new(), [END]),
                })
            }),
        )
        .add_node(
            "send",
            ask(&llm, "receipt", |s| {
                format!("confirm sending: {}", text(s, "email"))
            }),
        )
        .set_entry("draft")
        .add_edge("draft", "review")
        .may_goto("review", ["send", END])
        .add_edge("send", END);
    let workflow = builder.build().unwrap();

    let paused = workflow.run(State::new()).await.unwrap();
    assert!(paused.is_suspended());
    assert_eq!(llm.calls(), 1);

    let done = workflow
        .resume(paused.snapshot().unwrap(), [("review", json!("approve"))])
        .await
        .unwrap();
    assert!(!done.is_suspended());
    let prompts = llm.prompts();
    assert_eq!(prompts.len(), 2, "draft was not called again: {prompts:?}");
    assert!(prompts[1].starts_with("confirm sending: [fake-llm]"));
    assert!(text(&done.state, "receipt").starts_with("[fake-llm]"));
}
