//! Node timeouts, retries, run deadlines and cancellation (design 0005 §4),
//! exercised with the simulated LLM on Tokio's paused clock, so every timing
//! assertion is exact.

#[path = "support/fake_llm.rs"]
mod fake_llm;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use criogenai_core::{
    CancelToken, END, Event, EventKind, Node, NodeError, NodeOutput, NodePolicy, RetryPolicy,
    RunOptions, State, Update, ValidationIssue, Workflow, WorkflowBuilder, event_channel, node_fn,
    node_with_context,
};
use fake_llm::FakeLlm;
use serde_json::json;
use tokio::time::Instant;

fn ask(llm: &Arc<FakeLlm>, key: &'static str, prompt: &'static str) -> impl Node {
    let llm = llm.clone();
    node_fn(move |_| {
        let llm = llm.clone();
        async move { Ok(Update::new().set(key, llm.complete(prompt).await?)) }
    })
}

fn single(name: &str, node: impl Node, policy: Option<NodePolicy>) -> Workflow {
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(name, node)
        .set_entry(name)
        .add_edge(name, END);
    if let Some(policy) = policy {
        builder.policy(name, policy);
    }
    builder.build().unwrap()
}

/// Exact backoff: 1 s, then 2 s, then 4 s...
fn fixed_retry(attempts: u32) -> RetryPolicy {
    RetryPolicy::new()
        .max_attempts(attempts)
        .backoff(Duration::from_secs(1), 2.0, Duration::from_secs(60))
        .jitter(false)
}

/// Runs with an event channel and returns the result plus every event.
async fn run_observed(
    workflow: &Workflow,
    options: RunOptions,
) -> (
    Result<criogenai_core::RunOutput, criogenai_core::RunError>,
    Vec<Event>,
) {
    let (sender, mut receiver) = event_channel(1024);
    let run = workflow.run_with(State::new(), options.events(sender));
    let read = async {
        let mut events = Vec::new();
        while let Some(event) = receiver.recv().await {
            events.push(event);
        }
        events
    };
    tokio::join!(run, read)
}

fn types(events: &[Event]) -> Vec<&'static str> {
    events.iter().map(Event::type_name).collect()
}

// ---------- retry ----------

#[tokio::test(start_paused = true)]
async fn retry_recovers_from_transient_llm_errors() {
    let llm = Arc::new(FakeLlm::new().flaky_first(2));
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let node = {
        let (llm, attempts) = (llm.clone(), attempts.clone());
        node_with_context(move |ctx, _| {
            let (llm, attempts) = (llm.clone(), attempts.clone());
            async move {
                attempts.lock().unwrap().push(ctx.attempt());
                let reply = llm.complete("summarise").await?;
                Ok(NodeOutput::Update(Update::new().set("answer", reply)))
            }
        })
    };
    let workflow = single("model", node, Some(NodePolicy::new().retry(fixed_retry(3))));

    let start = Instant::now();
    let (result, events) = run_observed(&workflow, RunOptions::default()).await;
    let took = start.elapsed();

    let output = result.unwrap();
    assert!(output.state.get("answer").is_some());
    assert_eq!(*attempts.lock().unwrap(), [1, 2, 3]);
    assert_eq!(llm.calls(), 3);
    // Three 2–3 s calls plus 1 s and 2 s of backoff.
    let call = llm.delay_for("summarise");
    assert_eq!(took, call * 3 + Duration::from_secs(3));

    assert_eq!(
        types(&events)[2..8],
        [
            "node_started",
            "attempt_failed",
            "node_started",
            "attempt_failed",
            "node_started",
            "node_finished"
        ]
    );
    let retry_delays: Vec<u64> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::AttemptFailed {
                retry_in_ms,
                message,
                ..
            } => {
                assert!(message.contains("simulated rate limit"));
                Some(*retry_in_ms)
            }
            _ => None,
        })
        .collect();
    assert_eq!(retry_delays, [1000, 2000]);
    let started: Vec<u32> = events
        .iter()
        .filter_map(|e| match e.kind {
            EventKind::NodeStarted { attempt } => Some(attempt),
            _ => None,
        })
        .collect();
    assert_eq!(started, [1, 2, 3]);
}

#[tokio::test(start_paused = true)]
async fn exhausted_retries_fail_with_the_last_error() {
    let llm = Arc::new(FakeLlm::new().flaky_first(10));
    let workflow = single(
        "model",
        ask(&llm, "answer", "hi"),
        Some(NodePolicy::new().retry(fixed_retry(3))),
    );
    let err = workflow.run(State::new()).await.unwrap_err();
    assert_eq!(err.code(), "node_failed");
    assert!(err.to_string().contains("call 3"), "{err}");
    assert_eq!(llm.calls(), 3);
}

#[tokio::test(start_paused = true)]
async fn errors_not_marked_retryable_are_not_retried() {
    let llm = Arc::new(FakeLlm::new().failing_on("bad"));
    let workflow = single(
        "model",
        ask(&llm, "answer", "bad prompt"),
        Some(NodePolicy::new().retry(fixed_retry(5))),
    );
    let err = workflow.run(State::new()).await.unwrap_err();
    assert_eq!(err.code(), "node_failed");
    assert_eq!(llm.calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn without_a_policy_nothing_is_retried() {
    let llm = Arc::new(FakeLlm::new().flaky_first(1));
    let workflow = single("model", ask(&llm, "answer", "hi"), None);
    assert_eq!(
        workflow.run(State::new()).await.unwrap_err().code(),
        "node_failed"
    );
    assert_eq!(llm.calls(), 1);
}

// ---------- timeout ----------

#[tokio::test(start_paused = true)]
async fn slow_llm_call_times_out_and_its_token_fires() {
    let llm = Arc::new(FakeLlm::new()); // 2–3 s
    let token = Arc::new(Mutex::new(None::<CancelToken>));
    let node = {
        let (llm, token) = (llm.clone(), token.clone());
        node_with_context(move |ctx, _| {
            let (llm, token) = (llm.clone(), token.clone());
            async move {
                *token.lock().unwrap() = Some(ctx.cancel_token().clone());
                assert_eq!(ctx.remaining_time(), Some(Duration::from_secs(1)));
                let reply = llm.complete("slow").await?;
                Ok(NodeOutput::Update(Update::new().set("answer", reply)))
            }
        })
    };
    let workflow = single(
        "model",
        node,
        Some(NodePolicy::new().timeout(Duration::from_secs(1))),
    );

    let start = Instant::now();
    let (result, events) = run_observed(&workflow, RunOptions::default()).await;
    let err = result.unwrap_err();

    assert_eq!(err.code(), "node_timeout");
    assert_eq!(
        err.to_string(),
        "step 0: node `model` timed out after 1000 ms"
    );
    assert_eq!(start.elapsed(), Duration::from_secs(1));
    assert!(token.lock().unwrap().as_ref().unwrap().is_cancelled());
    assert!(err.state.get("answer").is_none());
    match &events[events.len() - 2].kind {
        EventKind::NodeFailed { code, .. } => assert_eq!(code, "node_timeout"),
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn timeouts_are_not_retried() {
    let llm = Arc::new(FakeLlm::new());
    let workflow = single(
        "model",
        ask(&llm, "answer", "slow"),
        Some(
            NodePolicy::new()
                .timeout(Duration::from_secs(1))
                .retry(fixed_retry(3)),
        ),
    );
    assert_eq!(
        workflow.run(State::new()).await.unwrap_err().code(),
        "node_timeout"
    );
    assert_eq!(llm.calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn timeout_applies_to_each_attempt() {
    // Each call takes 2–3 s and fits in a 5 s timeout, but the total of three
    // attempts plus backoff does not, and that is fine: the limit is per attempt.
    let llm = Arc::new(FakeLlm::new().flaky_first(2));
    let workflow = single(
        "model",
        ask(&llm, "answer", "hi"),
        Some(
            NodePolicy::new()
                .timeout(Duration::from_secs(5))
                .retry(fixed_retry(3)),
        ),
    );
    workflow.run(State::new()).await.unwrap();
    assert_eq!(llm.calls(), 3);
}

#[tokio::test(start_paused = true)]
async fn a_timeout_cancels_siblings_like_any_failure() {
    let fast = Arc::new(FakeLlm::new());
    let slow = Arc::new(FakeLlm::with_delay(
        Duration::from_secs(10),
        Duration::from_secs(10),
    ));
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("start", node_fn(|_| async { Ok(Update::new()) }))
        .add_node("fast", ask(&fast, "a", "fast"))
        .add_node("slow", ask(&slow, "b", "slow"))
        .set_entry("start")
        .add_edge("start", "fast")
        .add_edge("start", "slow")
        .add_edge("fast", END)
        .add_edge("slow", END)
        .policy("slow", NodePolicy::new().timeout(Duration::from_secs(4)));
    let workflow = builder.build().unwrap();

    let (result, events) = run_observed(&workflow, RunOptions::default()).await;
    assert_eq!(result.unwrap_err().code(), "node_timeout");
    // `fast` finished (2–3 s) before `slow` timed out (4 s); nothing is committed.
    let trace: Vec<String> = events
        .iter()
        .filter(|e| e.step == Some(1) && e.node.is_some())
        .map(|e| format!("{}:{}", e.type_name(), e.node.as_deref().unwrap()))
        .collect();
    assert_eq!(
        trace,
        [
            "node_started:fast",
            "node_started:slow",
            "node_finished:fast",
            "node_failed:slow"
        ]
    );
}

// ---------- deadline ----------

#[tokio::test(start_paused = true)]
async fn deadline_stops_a_run_mid_step_and_keeps_committed_work() {
    let llm = Arc::new(FakeLlm::new());
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("first", ask(&llm, "first", "one"))
        .add_node("second", ask(&llm, "second", "two"))
        .set_entry("first")
        .add_edge("first", "second")
        .add_edge("second", END);
    let workflow = builder.build().unwrap();

    let start = Instant::now();
    let (result, events) = run_observed(
        &workflow,
        RunOptions::default().deadline(Duration::from_secs(4)),
    )
    .await;
    let err = result.unwrap_err();

    assert_eq!(err.code(), "deadline_exceeded");
    assert_eq!(err.step, 1);
    assert_eq!(start.elapsed(), Duration::from_secs(4));
    assert!(err.state.get("first").is_some(), "step 0 stays committed");
    assert!(err.state.get("second").is_none());
    let tail = types(&events);
    assert_eq!(tail[tail.len() - 2..], ["node_cancelled", "run_finished"]);
    assert_eq!(
        events[events.len() - 2].kind,
        EventKind::NodeCancelled {
            reason: "deadline_exceeded".into()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn deadline_cuts_a_backoff_short_instead_of_sleeping_past_it() {
    let llm = Arc::new(FakeLlm::new().flaky_first(1));
    let workflow = single(
        "model",
        ask(&llm, "answer", "hi"),
        Some(
            NodePolicy::new().retry(
                RetryPolicy::new()
                    .backoff(Duration::from_secs(30), 2.0, Duration::from_secs(60))
                    .jitter(false),
            ),
        ),
    );

    let start = Instant::now();
    let err = workflow
        .run_with(
            State::new(),
            RunOptions::default().deadline(Duration::from_secs(10)),
        )
        .await
        .unwrap_err();

    assert_eq!(err.code(), "deadline_exceeded");
    // Failed right after the first call, without waiting for the deadline.
    assert_eq!(start.elapsed(), llm.delay_for("hi"));
    assert_eq!(llm.calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn remaining_time_is_the_earlier_of_deadline_and_timeout() {
    let seen = Arc::new(Mutex::new(None));
    let node = {
        let seen = seen.clone();
        node_with_context(move |ctx, _| {
            let seen = seen.clone();
            async move {
                *seen.lock().unwrap() = ctx.remaining_time();
                Ok(NodeOutput::Update(Update::new()))
            }
        })
    };
    let workflow = single(
        "n",
        node,
        Some(NodePolicy::new().timeout(Duration::from_secs(30))),
    );
    workflow
        .run_with(
            State::new(),
            RunOptions::default().deadline(Duration::from_secs(4)),
        )
        .await
        .unwrap();
    assert_eq!(*seen.lock().unwrap(), Some(Duration::from_secs(4)));

    let unbounded = single(
        "n",
        node_with_context(|ctx, _| async move {
            assert_eq!(ctx.remaining_time(), None);
            Ok(NodeOutput::Update(Update::new()))
        }),
        None,
    );
    unbounded.run(State::new()).await.unwrap();
}

// ---------- cancellation ----------

/// A node that cleans up for 100 ms when told to stop.
fn cooperative(cleaned_up: Arc<AtomicBool>) -> impl Node {
    node_with_context(move |ctx, _| {
        let cleaned_up = cleaned_up.clone();
        async move {
            ctx.cancel_token().cancelled().await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            cleaned_up.store(true, Ordering::SeqCst);
            Ok(NodeOutput::Update(Update::new().set("never", "committed")))
        }
    })
}

#[tokio::test(start_paused = true)]
async fn cancel_gives_nodes_a_grace_period_then_drops_them() {
    let cleaned_up = Arc::new(AtomicBool::new(false));
    let llm = Arc::new(FakeLlm::with_delay(
        Duration::from_secs(60),
        Duration::from_secs(60),
    ));
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "start",
            node_fn(|_| async { Ok(Update::new().set("x", 1)) }),
        )
        .add_node("polite", cooperative(cleaned_up.clone()))
        .add_node("stubborn", ask(&llm, "y", "ignores cancellation"))
        .set_entry("start")
        .add_edge("start", "polite")
        .add_edge("start", "stubborn")
        .add_edge("polite", END)
        .add_edge("stubborn", END);
    let workflow = builder.build().unwrap();

    let token = CancelToken::new();
    tokio::spawn({
        let token = token.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            token.cancel();
        }
    });
    let start = Instant::now();
    let (result, events) = run_observed(
        &workflow,
        RunOptions::default()
            .cancel_token(token)
            .cancel_grace(Duration::from_millis(500)),
    )
    .await;
    let err = result.unwrap_err();

    assert_eq!(err.code(), "cancelled");
    // 1 s until cancel + 0.5 s grace for the node that ignores it.
    assert_eq!(start.elapsed(), Duration::from_millis(1500));
    assert!(cleaned_up.load(Ordering::SeqCst), "polite node cleaned up");
    assert_eq!(err.state.get("x"), Some(&json!(1)));
    assert!(
        err.state.get("never").is_none(),
        "results after cancel are discarded"
    );

    let cancelled: Vec<&str> = events
        .iter()
        .filter(|e| {
            e.kind
                == EventKind::NodeCancelled {
                    reason: "run_cancelled".into(),
                }
        })
        .map(|e| e.node.as_deref().unwrap())
        .collect();
    assert_eq!(cancelled, ["polite", "stubborn"]);
    assert_eq!(
        events.last().unwrap().kind,
        EventKind::RunFinished {
            status: "failed".into(),
            error: Some("cancelled".into()),
            steps: 1
        }
    );
}

#[tokio::test(start_paused = true)]
async fn cooperative_nodes_end_the_run_without_waiting_for_grace() {
    let cleaned_up = Arc::new(AtomicBool::new(false));
    let workflow = single("polite", cooperative(cleaned_up.clone()), None);
    let token = CancelToken::new();
    tokio::spawn({
        let token = token.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            token.cancel();
        }
    });
    let start = Instant::now();
    let err = workflow
        .run_with(State::new(), RunOptions::default().cancel_token(token))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "cancelled");
    assert_eq!(start.elapsed(), Duration::from_millis(2100));
    assert!(cleaned_up.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn cancel_during_backoff_ends_the_run_at_once() {
    let llm = Arc::new(FakeLlm::new().flaky_first(1));
    let workflow = single(
        "model",
        ask(&llm, "answer", "hi"),
        Some(
            NodePolicy::new().retry(
                RetryPolicy::new()
                    .backoff(Duration::from_secs(30), 2.0, Duration::from_secs(60))
                    .jitter(false),
            ),
        ),
    );
    let token = CancelToken::new();
    let cancel_at = llm.delay_for("hi") + Duration::from_secs(1);
    tokio::spawn({
        let token = token.clone();
        async move {
            tokio::time::sleep(cancel_at).await;
            token.cancel();
        }
    });
    let start = Instant::now();
    let err = workflow
        .run_with(State::new(), RunOptions::default().cancel_token(token))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "cancelled");
    assert_eq!(start.elapsed(), cancel_at);
    assert_eq!(llm.calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn already_cancelled_token_runs_nothing() {
    let llm = Arc::new(FakeLlm::new());
    let workflow = single("model", ask(&llm, "answer", "hi"), None);
    let token = CancelToken::new();
    token.cancel();
    let err = workflow
        .run_with(State::new(), RunOptions::default().cancel_token(token))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "cancelled");
    assert_eq!(err.step, 0);
    assert_eq!(llm.calls(), 0);
}

#[tokio::test(start_paused = true)]
async fn run_stream_cancel_reports_the_whole_ending() {
    let llm = Arc::new(FakeLlm::new());
    let workflow = single("model", ask(&llm, "answer", "hi"), None);

    let mut run = workflow.stream(State::new());
    let mut seen = Vec::new();
    while let Some(event) = run.next().await {
        if event.type_name() == "node_started" {
            run.cancel();
        }
        seen.push(event.type_name());
    }
    assert_eq!(
        seen,
        [
            "run_started",
            "step_started",
            "node_started",
            "node_cancelled",
            "run_finished"
        ]
    );
    assert_eq!(run.finish().await.unwrap_err().code(), "cancelled");
}

// ---------- validation and data ----------

#[test]
fn policy_validation() {
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("a", node_fn(|_| async { Ok(Update::new()) }))
        .set_entry("a")
        .add_edge("a", END)
        .policy("a", NodePolicy::new())
        .policy("a", NodePolicy::new())
        .policy("ghost", NodePolicy::new())
        .policy(
            "a2",
            NodePolicy::new().retry(RetryPolicy::new().max_attempts(0)),
        );
    let issues = builder.build().err().unwrap().issues;
    assert!(issues.contains(&ValidationIssue::DuplicatePolicy("a".into())));
    assert!(issues.contains(&ValidationIssue::UnknownNode {
        node: "ghost".into(),
        context: "policy for `ghost`".into()
    }));
    assert!(issues.iter().any(|i| matches!(
        i,
        ValidationIssue::InvalidPolicy { node, reason }
            if node == "a2" && reason.contains("max_attempts")
    )));
}

#[test]
fn policies_do_not_change_the_fingerprint() {
    let build =
        |policy: Option<NodePolicy>| single("a", node_fn(|_| async { Ok(Update::new()) }), policy);
    assert_eq!(
        build(None).fingerprint(),
        build(Some(NodePolicy::new().timeout(Duration::from_secs(1)))).fingerprint()
    );
}

#[test]
fn retryable_flag_round_trips_as_json() {
    let error = NodeError::new("rate limited").retryable();
    let json = serde_json::to_value(&error).unwrap();
    assert_eq!(
        json,
        json!({ "message": "rate limited", "retryable": true })
    );
    assert_eq!(serde_json::from_value::<NodeError>(json).unwrap(), error);

    let plain = serde_json::to_value(NodeError::new("bug")).unwrap();
    assert_eq!(plain, json!({ "message": "bug" }), "false is omitted");
}
