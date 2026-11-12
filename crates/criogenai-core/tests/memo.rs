//! Saved task writes, the task memo and snapshots on failure (design 0005 §5),
//! exercised with the simulated LLM on Tokio's paused clock.

#[path = "support/fake_llm.rs"]
mod fake_llm;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use criogenai_core::{
    CancelToken, END, Event, EventKind, Node, NodeError, NodeOutput, NodePolicy, Reducer,
    RetryPolicy, RunError, RunOptions, Snapshot, State, Update, Workflow, WorkflowBuilder,
    event_channel, node_fn, node_with_context, router_fn,
};
use fake_llm::FakeLlm;
use serde_json::{Value, json};
use tokio::time::Instant;

/// A fault that tests switch off to simulate "fix the bug, then resume".
#[derive(Clone, Default)]
struct Fault(Arc<AtomicBool>);

impl Fault {
    fn on() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }
    fn fix(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
    fn check(&self, what: &str) -> Result<(), NodeError> {
        if self.0.load(Ordering::SeqCst) {
            Err(NodeError::new(format!("bug in {what}")))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Default)]
struct Counter(Arc<AtomicUsize>);

impl Counter {
    fn hit(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
    fn get(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

/// Calls the LLM with `prompt` and stores the reply under `key`.
fn ask(llm: &Arc<FakeLlm>, key: &'static str, prompt: &'static str) -> impl Node {
    let llm = llm.clone();
    node_fn(move |_| {
        let llm = llm.clone();
        async move { Ok(Update::new().set(key, llm.complete(prompt).await?)) }
    })
}

/// Fails while `fault` is on, after `delay`.
fn buggy(fault: &Fault, runs: &Counter, key: &'static str, delay: Duration) -> impl Node {
    let (fault, runs) = (fault.clone(), runs.clone());
    node_fn(move |_| {
        let (fault, runs) = (fault.clone(), runs.clone());
        async move {
            runs.hit();
            tokio::time::sleep(delay).await;
            fault.check(key)?;
            Ok(Update::new().set(key, "fixed"))
        }
    })
}

fn failure(result: Result<criogenai_core::RunOutput, RunError>) -> (RunError, Snapshot) {
    let err = result.expect_err("run should fail");
    let snapshot = err.snapshot().expect("failures carry a snapshot").clone();
    (err, snapshot)
}

/// Runs `run` with an event channel and returns its result plus every event.
async fn observed<F: Future>(run: impl FnOnce(RunOptions) -> F) -> (F::Output, Vec<Event>) {
    let (sender, mut receiver) = event_channel(1024);
    let read = async {
        let mut events = Vec::new();
        while let Some(event) = receiver.recv().await {
            events.push(event);
        }
        events
    };
    tokio::join!(run(RunOptions::default().events(sender)), read)
}

// ---------- saved outputs ----------

/// `email` (2–3 s) and `summary` run in parallel; `summary` has a bug.
fn email_and_summary(llm: &Arc<FakeLlm>, fault: &Fault, runs: &Counter) -> Workflow {
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("start", node_fn(|_| async { Ok(Update::new()) }))
        .add_node("email", ask(llm, "email", "send the weekly email"))
        .add_node(
            "summary",
            buggy(fault, runs, "summary", Duration::from_secs(4)),
        )
        .set_entry("start")
        .add_edge("start", "email")
        .add_edge("start", "summary")
        .add_edge("email", END)
        .add_edge("summary", END);
    builder.build().unwrap()
}

#[tokio::test(start_paused = true)]
async fn finished_nodes_are_not_rerun_when_a_failed_step_is_retried() {
    let llm = Arc::new(FakeLlm::new());
    let (fault, runs) = (Fault::on(), Counter::default());
    let wf = email_and_summary(&llm, &fault, &runs);

    let (err, snapshot) = failure(wf.run(State::new()).await);
    assert_eq!(err.code(), "node_failed");
    assert_eq!(err.step, 1);
    assert_eq!(snapshot.step, 1);
    assert_eq!(snapshot.frontier, ["email", "summary"]);
    assert_eq!(snapshot.error.as_ref().unwrap().code, "node_failed");
    let saved = &snapshot.pending_writes["email"];
    assert!(matches!(saved.output, Some(NodeOutput::Update(_))));
    assert!(!snapshot.pending_writes.contains_key("summary"));
    assert_eq!(llm.calls(), 1);

    fault.fix();
    let started = Instant::now();
    let (done, events) = observed(|o| wf.retry_with(&snapshot, o)).await;
    let done = done.unwrap();

    // The email was not sent again; only `summary` ran (4 s).
    assert_eq!(llm.calls(), 1);
    assert_eq!(runs.get(), 2);
    assert_eq!(started.elapsed(), Duration::from_secs(4));
    assert_eq!(done.run_id, snapshot.run_id);
    assert_eq!(done.state.get("summary").unwrap(), "fixed");
    assert!(
        done.state
            .get("email")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("weekly email")
    );

    let email_events: Vec<&EventKind> = events
        .iter()
        .filter(|e| e.node.as_deref() == Some("email"))
        .map(|e| &e.kind)
        .collect();
    assert!(
        matches!(
            email_events.as_slice(),
            [EventKind::NodeFinished { reused: true, .. }]
        ),
        "a reused node gets only node_finished: {email_events:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn failure_snapshot_survives_json_and_retry_from_another_process() {
    let llm = Arc::new(FakeLlm::new());
    let (fault, runs) = (Fault::on(), Counter::default());
    let wf = email_and_summary(&llm, &fault, &runs);
    let (_, snapshot) = failure(wf.run(State::new()).await);

    let json = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(json["error"]["code"], "node_failed");
    assert!(json["pending_writes"]["email"]["output"]["update"]["email"].is_string());
    assert!(json["pending_writes"]["email"].get("memo").is_none());

    // "Another process": a fresh workflow value and LLM, same graph.
    let fresh_llm = Arc::new(FakeLlm::new());
    fault.fix();
    let wf2 = email_and_summary(&fresh_llm, &fault, &runs);
    let restored: Snapshot = serde_json::from_value(json).unwrap();
    assert_eq!(restored, snapshot);
    let done = wf2.retry(&restored).await.unwrap();
    assert_eq!(fresh_llm.calls(), 0);
    assert_eq!(done.state.get("summary").unwrap(), "fixed");
}

#[tokio::test]
async fn snapshots_without_the_new_fields_still_load() {
    let old = json!({
        "format": "criogen.snapshot/1",
        "workflow_fingerprint": "fnv1a64:0",
        "run_id": "r",
        "step": 1,
        "state": {},
        "frontier": ["a"],
        "join_progress": {},
        "suspended": []
    });
    let snapshot: Snapshot = serde_json::from_value(old).unwrap();
    assert!(snapshot.pending_writes.is_empty());
    assert!(snapshot.error.is_none());
    assert_eq!(snapshot.event_seq, 0);
}

// ---------- the memo ----------

/// Sends an email (once, through the memo), then asks a model that may fail.
fn email_then_model(emails: &Counter, llm: &Arc<FakeLlm>, prompt: &'static str) -> impl Node {
    let (emails, llm) = (emails.clone(), llm.clone());
    node_with_context(move |ctx, _| {
        let (emails, llm) = (emails.clone(), llm.clone());
        async move {
            let receipt: String = ctx
                .memo()
                .once("send_email", || async {
                    emails.hit();
                    Ok(format!("msg-{}", emails.get()))
                })
                .await?;
            let answer = llm.complete(prompt).await?;
            Ok(Update::new()
                .set("receipt", receipt)
                .set("answer", answer)
                .into())
        }
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

#[tokio::test(start_paused = true)]
async fn memo_survives_retries_within_a_step() {
    let emails = Counter::default();
    let llm = Arc::new(FakeLlm::new().flaky_first(2));
    let retry = RetryPolicy::new()
        .max_attempts(3)
        .backoff(Duration::from_secs(1), 2.0, Duration::from_secs(10))
        .jitter(false);
    let wf = single(
        "notify",
        email_then_model(&emails, &llm, "summarise"),
        Some(NodePolicy::new().retry(retry)),
    );

    let out = wf.run(State::new()).await.unwrap();
    assert_eq!(llm.calls(), 3, "the model was retried");
    assert_eq!(emails.get(), 1, "the email was sent once");
    assert_eq!(out.state.get("receipt").unwrap(), "msg-1");
}

#[tokio::test(start_paused = true)]
async fn memo_survives_a_timed_out_attempt() {
    let emails = Counter::default();
    let llm = Arc::new(FakeLlm::with_delay(
        Duration::from_secs(5),
        Duration::from_secs(5),
    ));
    let wf = single(
        "notify",
        email_then_model(&emails, &llm, "slow"),
        Some(NodePolicy::new().timeout(Duration::from_secs(1))),
    );
    let (err, snapshot) = failure(wf.run(State::new()).await);
    assert_eq!(err.code(), "node_timeout");
    assert_eq!(
        snapshot.pending_writes["notify"].memo["send_email"],
        "msg-1"
    );
    assert!(snapshot.pending_writes["notify"].output.is_none());

    let again = wf.retry(&snapshot).await.unwrap_err();
    assert_eq!(again.code(), "node_timeout");
    assert_eq!(emails.get(), 1, "not resent after the timeout or on retry");
}

#[tokio::test(start_paused = true)]
async fn memo_survives_resume_of_a_failed_step() {
    let emails = Counter::default();
    let llm = Arc::new(FakeLlm::new().failing_on("BROKEN"));
    let wf = single("notify", email_then_model(&emails, &llm, "BROKEN"), None);
    let (_, snapshot) = failure(wf.run(State::new()).await);
    assert_eq!(emails.get(), 1);

    // The fixed code (a different prompt) runs against the saved memo.
    let fixed = Arc::new(FakeLlm::new().failing_on("BROKEN"));
    let wf = single(
        "notify",
        email_then_model(&emails, &fixed, "summarise"),
        None,
    );
    let restored: Snapshot =
        serde_json::from_str(&serde_json::to_string(&snapshot).unwrap()).unwrap();
    let out = wf.retry(&restored).await.unwrap();
    assert_eq!(emails.get(), 1, "the email was not resent");
    assert_eq!(out.state.get("receipt").unwrap(), "msg-1");
    assert_eq!(fixed.calls(), 1);
}

/// Drafts with the LLM (memoised), then asks a human to approve.
fn draft_then_approve(llm: &Arc<FakeLlm>) -> impl Node {
    let llm = llm.clone();
    node_with_context(move |ctx, _| {
        let llm = llm.clone();
        async move {
            let draft: String = ctx
                .memo()
                .once("draft", || llm.complete("draft a reply"))
                .await?;
            match ctx.resume() {
                None => Ok(NodeOutput::suspend(
                    Update::new(),
                    json!({ "approve": draft }),
                )),
                Some(answer) => Ok(Update::new()
                    .set("sent", draft)
                    .set("approved", answer.clone())
                    .into()),
            }
        }
    })
}

#[tokio::test(start_paused = true)]
async fn memo_survives_suspend_and_resume() {
    let llm = Arc::new(FakeLlm::new());
    let wf = single("review", draft_then_approve(&llm), None);

    let started = Instant::now();
    let paused = wf.run(State::new()).await.unwrap();
    let snapshot = paused.snapshot().unwrap();
    assert!(snapshot.pending_writes["review"].memo.contains_key("draft"));
    assert_eq!(started.elapsed(), llm.delay_for("draft a reply"));

    let resumed_at = Instant::now();
    let done = wf
        .resume(snapshot, [("review", json!(true))])
        .await
        .unwrap();
    assert_eq!(llm.calls(), 1, "the draft was not regenerated");
    assert_eq!(resumed_at.elapsed(), Duration::ZERO);
    assert_eq!(done.state.get("approved").unwrap(), true);
    assert!(
        done.state
            .get("sent")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("draft a reply")
    );
}

#[tokio::test]
async fn memo_starts_empty_in_each_new_step() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let node = {
        let seen = seen.clone();
        node_with_context(move |ctx, state| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(ctx.memo().get("visited"));
                ctx.memo().put("visited", ctx.step());
                let n = state.get("n").and_then(Value::as_u64).unwrap_or(0) + 1;
                let update = Update::new().set("n", n);
                Ok(if n < 3 {
                    NodeOutput::goto(update, ["loop"])
                } else {
                    NodeOutput::goto(update, [END])
                })
            }
        })
    };
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("loop", node)
        .may_goto("loop", ["loop", END])
        .set_entry("loop");
    let out = builder.build().unwrap().run(State::new()).await.unwrap();
    assert_eq!(out.state.get("n").unwrap(), 3);
    assert_eq!(*seen.lock().unwrap(), [None, None, None]);
}

// ---------- the blame rule ----------

#[tokio::test]
async fn output_that_caused_the_failure_is_not_saved() {
    let (good_runs, bad_runs) = (Counter::default(), Counter::default());
    let fault = Fault::on();
    let good = {
        let runs = good_runs.clone();
        node_fn(move |_| {
            let runs = runs.clone();
            async move {
                runs.hit();
                Ok(Update::new().set("log", json!(["good"])))
            }
        })
    };
    // While the fault is on, writes a non-list to an `append` key.
    let bad = {
        let (runs, fault) = (bad_runs.clone(), fault.clone());
        node_fn(move |_| {
            let (runs, fault) = (runs.clone(), fault.clone());
            async move {
                runs.hit();
                let value = if fault.check("bad").is_err() {
                    json!("oops")
                } else {
                    json!(["bad"])
                };
                Ok(Update::new().set("log", value))
            }
        })
    };
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("start", node_fn(|_| async { Ok(Update::new()) }))
        .add_node("good", good)
        .add_node("bad", bad)
        .reducer("log", Reducer::Append)
        .set_entry("start")
        .add_edge("start", "good")
        .add_edge("start", "bad")
        .add_edge("good", END)
        .add_edge("bad", END);
    let wf = builder.build().unwrap();

    let (err, snapshot) = failure(wf.run(State::new()).await);
    assert_eq!(err.code(), "invalid_value");
    assert!(snapshot.pending_writes.contains_key("good"));
    assert!(
        !snapshot.pending_writes.contains_key("bad"),
        "the blamed output must not be reused"
    );

    fault.fix();
    let out = wf.retry(&snapshot).await.unwrap();
    assert_eq!((good_runs.get(), bad_runs.get()), (1, 2));
    assert_eq!(out.state.get("log").unwrap(), &json!(["good", "bad"]));
}

#[tokio::test]
async fn update_conflict_blames_both_writers() {
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("start", node_fn(|_| async { Ok(Update::new()) }))
        .add_node("a", node_fn(|_| async { Ok(Update::new().set("x", 1)) }))
        .add_node("b", node_fn(|_| async { Ok(Update::new().set("x", 2)) }))
        .add_node("c", node_fn(|_| async { Ok(Update::new().set("y", 3)) }))
        .set_entry("start")
        .add_edge("start", "a")
        .add_edge("start", "b")
        .add_edge("start", "c")
        .add_edge("a", END)
        .add_edge("b", END)
        .add_edge("c", END);
    let (err, snapshot) = failure(builder.build().unwrap().run(State::new()).await);
    assert_eq!(err.code(), "update_conflict");
    let saved: Vec<&String> = snapshot.pending_writes.keys().collect();
    assert_eq!(saved, ["c"]);
}

#[tokio::test]
async fn router_failure_keeps_outputs_and_reruns_the_router() {
    let (runs, fault) = (Counter::default(), Fault::on());
    let node = {
        let runs = runs.clone();
        node_fn(move |_| {
            let runs = runs.clone();
            async move {
                runs.hit();
                Ok(Update::new().set("x", 1))
            }
        })
    };
    let router = {
        let fault = fault.clone();
        router_fn(move |_| {
            let fault = fault.clone();
            async move {
                fault.check("router")?;
                Ok("done")
            }
        })
    };
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("work", node)
        .set_entry("work")
        .add_router("work", router, [("done", END)]);
    let wf = builder.build().unwrap();

    let (err, snapshot) = failure(wf.run(State::new()).await);
    assert_eq!(err.code(), "router_failed");
    assert!(snapshot.pending_writes["work"].output.is_some());
    fault.fix();
    let out = wf.retry(&snapshot).await.unwrap();
    assert_eq!(runs.get(), 1);
    assert_eq!(out.state.get("x").unwrap(), 1);
}

// ---------- suspend and failure together (0005 C2) ----------

#[tokio::test(start_paused = true)]
async fn suspend_and_failure_in_one_step_then_retry_and_resume() {
    // `review` drafts (2–3 s) and suspends; `audit` fails at 10 s. The
    // failure wins, and the suspension is saved with the draft in its memo.
    let llm = Arc::new(FakeLlm::new());
    let (fault2, runs2) = (Fault::on(), Counter::default());
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("start", node_fn(|_| async { Ok(Update::new()) }))
        .add_node("review", draft_then_approve(&llm))
        .add_node(
            "audit",
            buggy(&fault2, &runs2, "audit", Duration::from_secs(10)),
        )
        .set_entry("start")
        .add_edge("start", "review")
        .add_edge("start", "audit")
        .add_edge("review", END)
        .add_edge("audit", END);
    let wf = builder.build().unwrap();
    let (_, snapshot) = failure(wf.run(State::new()).await);
    assert!(matches!(
        snapshot.pending_writes["review"].output,
        Some(NodeOutput::Suspend { .. })
    ));
    let calls = llm.calls();

    fault2.fix();
    let paused = wf.retry(&snapshot).await.unwrap();
    assert!(paused.is_suspended());
    assert_eq!(paused.interrupts()[0].node, "review");
    let done = wf
        .resume(paused.snapshot().unwrap(), [("review", json!("ok"))])
        .await
        .unwrap();
    assert_eq!(done.state.get("audit").unwrap(), "fixed");
    assert_eq!(done.state.get("approved").unwrap(), "ok");
    // `review` ran again on resume, in a later step, but its memo travelled
    // through both snapshots: no second draft.
    assert_eq!(llm.calls(), calls);
    assert_eq!(runs2.get(), 2);
}

#[tokio::test]
async fn resume_value_already_supplied_is_kept_even_when_null() {
    let (fault, runs) = (Fault::on(), Counter::default());
    let approve = {
        let (fault, runs) = (fault.clone(), runs.clone());
        node_with_context(move |ctx, _| {
            let (fault, runs) = (fault.clone(), runs.clone());
            async move {
                runs.hit();
                match ctx.resume() {
                    None => Ok(NodeOutput::suspend(Update::new(), "approve?")),
                    Some(value) => {
                        fault.check("approve")?;
                        Ok(Update::new().set("answer", value.clone()).into())
                    }
                }
            }
        })
    };
    let wf = single("approve", approve, None);
    let paused = wf.run(State::new()).await.unwrap();
    let (_, snapshot) = failure(
        wf.resume(paused.snapshot().unwrap(), [("approve", Value::Null)])
            .await,
    );
    assert!(snapshot.suspended.is_empty());
    let json = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(json["pending_writes"]["approve"]["resume"], Value::Null);
    assert!(
        json["pending_writes"]["approve"]
            .as_object()
            .unwrap()
            .contains_key("resume")
    );

    fault.fix();
    let restored: Snapshot = serde_json::from_value(json).unwrap();
    let done = wf.retry(&restored).await.unwrap();
    assert_eq!(done.state.get("answer").unwrap(), &Value::Null);
    assert_eq!(runs.get(), 3);
}

// ---------- stops at a step boundary or mid-step ----------

#[tokio::test(start_paused = true)]
async fn cancelled_run_can_be_retried_without_repeating_finished_nodes() {
    let fast = Arc::new(FakeLlm::with_delay(
        Duration::from_secs(1),
        Duration::from_secs(1),
    ));
    let slow = Arc::new(FakeLlm::with_delay(
        Duration::from_secs(3),
        Duration::from_secs(3),
    ));
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("start", node_fn(|_| async { Ok(Update::new()) }))
        .add_node("fast", ask(&fast, "fast", "quick"))
        .add_node("slow", ask(&slow, "slow", "long"))
        .set_entry("start")
        .add_edge("start", "fast")
        .add_edge("start", "slow")
        .add_edge("fast", END)
        .add_edge("slow", END);
    let wf = builder.build().unwrap();

    let token = CancelToken::new();
    tokio::spawn({
        let token = token.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            token.cancel();
        }
    });
    let (err, snapshot) = failure(
        wf.run_with(State::new(), RunOptions::default().cancel_token(token))
            .await,
    );
    assert_eq!(err.code(), "cancelled");
    // `fast` finished before the cancel; `slow` finished during the grace
    // period, and output returned after cancellation is never saved.
    assert!(snapshot.pending_writes["fast"].output.is_some());
    assert!(!snapshot.pending_writes.contains_key("slow"));

    let started = Instant::now();
    let done = wf.retry(&snapshot).await.unwrap();
    assert_eq!(started.elapsed(), Duration::from_secs(3));
    assert_eq!((fast.calls(), slow.calls()), (1, 2));
    assert!(done.state.get("fast").is_some() && done.state.get("slow").is_some());
}

#[tokio::test]
async fn step_limit_failure_continues_with_a_larger_limit() {
    let node = node_with_context(|_, state| async move {
        let n = state.get("n").and_then(Value::as_u64).unwrap_or(0) + 1;
        let target = if n < 5 { "count" } else { END };
        Ok(NodeOutput::goto(Update::new().set("n", n), [target]))
    });
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("count", node)
        .may_goto("count", ["count", END])
        .set_entry("count");
    let wf = builder.build().unwrap();

    let (err, snapshot) = failure(
        wf.run_with(State::new(), RunOptions::default().max_steps(2))
            .await,
    );
    assert_eq!(err.code(), "step_limit_exceeded");
    assert_eq!(snapshot.step, 2);
    assert_eq!(snapshot.frontier, ["count"]);
    assert!(snapshot.pending_writes.is_empty());
    assert_eq!(snapshot.state.get("n").unwrap(), 2);

    let done = wf
        .retry_with(&snapshot, RunOptions::default().max_steps(10))
        .await
        .unwrap();
    assert_eq!(done.state.get("n").unwrap(), 5);
    assert_eq!(done.steps.first().unwrap().step, 2);
}

// ---------- rejected input ----------

#[tokio::test]
async fn rejected_resume_has_no_snapshot_and_leaves_the_input_usable() {
    let fault = Fault::on();
    let runs = Counter::default();
    let wf = single("work", buggy(&fault, &runs, "work", Duration::ZERO), None);
    let (_, snapshot) = failure(wf.run(State::new()).await);

    let mut tampered = serde_json::to_value(&snapshot).unwrap();
    tampered["pending_writes"] = json!({ "ghost": { "memo": { "k": 1 } } });
    let tampered: Snapshot = serde_json::from_value(tampered).unwrap();
    let err = wf.retry(&tampered).await.unwrap_err();
    assert_eq!(err.code(), "resume_invalid");
    assert!(err.snapshot().is_none());

    let err = wf
        .resume(&snapshot, [("work", json!(1))])
        .await
        .unwrap_err();
    assert_eq!(err.code(), "resume_invalid", "nothing is suspended");

    fault.fix();
    assert!(wf.retry(&snapshot).await.is_ok());
}

#[tokio::test]
async fn retry_needs_values_for_suspended_nodes() {
    let llm = Arc::new(FakeLlm::with_delay(Duration::ZERO, Duration::ZERO));
    let wf = single("review", draft_then_approve(&llm), None);
    let paused = wf.run(State::new()).await.unwrap();
    let err = wf.retry(paused.snapshot().unwrap()).await.unwrap_err();
    assert_eq!(err.code(), "resume_invalid");
}

#[tokio::test]
async fn nodes_see_the_runs_metadata() {
    use criogenai_core::{
        END, NodeOutput, RunOptions, State, Update, WorkflowBuilder, node_with_context,
    };
    use serde_json::json;

    let workflow = WorkflowBuilder::new()
        .add_node(
            "who",
            node_with_context(|ctx, _state| async move {
                let actor = ctx
                    .metadata()
                    .and_then(|m| m.get("actor"))
                    .cloned()
                    .unwrap_or(json!(null));
                Ok(NodeOutput::from(Update::new().set("actor", actor)))
            }),
        )
        .set_entry("who")
        .add_edge("who", END)
        .build()
        .unwrap();
    let options = RunOptions::default().metadata(json!({"actor": "dr-rao"}));
    assert_eq!(options.metadata_value(), Some(&json!({"actor": "dr-rao"})));
    let output = workflow.run_with(State::new(), options).await.unwrap();
    assert_eq!(output.state.get("actor"), Some(&json!("dr-rao")));
    let output = workflow.run(State::new()).await.unwrap();
    assert_eq!(output.state.get("actor"), Some(&json!(null)));
}
