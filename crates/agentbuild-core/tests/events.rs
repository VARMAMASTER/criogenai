//! Run events (design 0005 §6): order, completeness, custom events, failures,
//! suspend/resume numbering, backpressure and disconnects.

#[path = "support/fake_llm.rs"]
mod fake_llm;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agentbuild_core::{
    END, Event, EventKind, Node, NodeError, NodeOutput, RunOptions, State, Update, Workflow,
    WorkflowBuilder, event_channel, node_fn, node_with_context, router_fn,
};
use fake_llm::FakeLlm;
use serde_json::{Value, json};
use tokio::time::Instant;

fn noop() -> impl Node {
    node_fn(|_| async { Ok(Update::new()) })
}

fn ask(llm: &Arc<FakeLlm>, key: &'static str, prompt: &'static str) -> impl Node {
    let llm = llm.clone();
    node_fn(move |_| {
        let llm = llm.clone();
        async move { Ok(Update::new().set(key, llm.complete(prompt).await?)) }
    })
}

/// Drains a stream; returns its events and the run's result.
async fn collect(
    workflow: &Workflow,
    input: State,
) -> (
    Vec<Event>,
    Result<agentbuild_core::RunOutput, agentbuild_core::RunError>,
) {
    let mut run = workflow.stream(input);
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        events.push(event);
    }
    (events, run.finish().await)
}

/// `"type"` or `"type:node"` for each event.
fn trace(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| match &e.node {
            Some(node) => format!("{}:{node}", e.type_name()),
            None => e.type_name().to_string(),
        })
        .collect()
}

fn assert_contiguous(events: &[Event], first: u64) {
    let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
    let expected: Vec<u64> = (first..first + events.len() as u64).collect();
    assert_eq!(seqs, expected, "seq numbers have no gaps or reordering");
}

#[tokio::test]
async fn linear_run_reports_every_lifecycle_event_in_order() {
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("a", node_fn(|_| async { Ok(Update::new().set("x", 1)) }))
        .add_node("b", noop())
        .set_entry("a")
        .add_edge("a", "b")
        .add_edge("b", END);
    let workflow = builder.build().unwrap();

    let (events, result) = collect(&workflow, State::new()).await;
    let output = result.unwrap();

    assert_eq!(
        trace(&events),
        [
            "run_started",
            "step_started",
            "node_started:a",
            "node_finished:a",
            "step_committed",
            "step_started",
            "node_started:b",
            "node_finished:b",
            "step_committed",
            "run_finished",
        ]
    );
    assert_contiguous(&events, 0);
    assert!(events.iter().all(|e| e.run_id == output.run_id));
    assert!(events.iter().all(|e| e.origin == "internal"));

    let a_finished = &events[3];
    assert_eq!(a_finished.step, Some(0));
    assert_eq!(
        a_finished.task_id.as_deref(),
        Some(format!("{}:0:a", output.run_id).as_str())
    );
    match &a_finished.kind {
        EventKind::NodeFinished {
            update_keys,
            goto,
            suspended,
            ..
        } => {
            assert_eq!(update_keys, &["x"]);
            assert!(goto.is_empty());
            assert!(!suspended);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(
        events[4].kind,
        EventKind::StepCommitted {
            next: vec!["b".into()]
        }
    );
    assert_eq!(
        events.last().unwrap().kind,
        EventKind::RunFinished {
            status: "completed".into(),
            error: None,
            steps: 2
        }
    );
}

#[tokio::test(start_paused = true)]
async fn parallel_llm_nodes_report_as_each_call_finishes() {
    let llm = Arc::new(FakeLlm::new());
    let prompts = [
        ("pros", "list the pros"),
        ("cons", "list the cons"),
        ("risks", "list the risks"),
    ];
    let mut builder = WorkflowBuilder::new();
    builder.add_node("start", noop()).set_entry("start");
    for (name, prompt) in prompts {
        builder
            .add_node(name, ask(&llm, name, prompt))
            .add_edge("start", name)
            .add_edge(name, END);
    }
    let workflow = builder.build().unwrap();

    let start = Instant::now();
    let mut run = workflow.stream(State::new());
    let mut finished = Vec::new();
    while let Some(event) = run.next().await {
        if let (EventKind::NodeFinished { .. }, Some(node)) = (&event.kind, &event.node)
            && node != "start"
        {
            finished.push((node.clone(), start.elapsed()));
        }
    }
    run.finish().await.unwrap();

    // Each event arrives when its own call finishes, fastest first, not at
    // the end of the step.
    let mut expected: Vec<(String, Duration)> = prompts
        .iter()
        .map(|(name, prompt)| (name.to_string(), llm.delay_for(prompt)))
        .collect();
    expected.sort_by_key(|(_, delay)| *delay);
    assert_eq!(finished, expected);
}

#[tokio::test]
async fn nodes_emit_custom_events_between_start_and_finish() {
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "work",
            node_with_context(|ctx, _| async move {
                assert!(ctx.events().is_enabled());
                for done in 1..=3 {
                    ctx.events()
                        .emit("progress", json!({ "done": done, "of": 3 }));
                }
                Ok(NodeOutput::Update(Update::new()))
            }),
        )
        .set_entry("work")
        .add_edge("work", END);
    let workflow = builder.build().unwrap();

    let (events, result) = collect(&workflow, State::new()).await;
    result.unwrap();

    assert_eq!(
        trace(&events)[2..7],
        [
            "node_started:work",
            "custom:work",
            "custom:work",
            "custom:work",
            "node_finished:work"
        ]
    );
    let custom = &events[4];
    assert_eq!(custom.origin, "user");
    assert_eq!(custom.step, Some(0));
    assert!(custom.task_id.as_deref().unwrap().ends_with(":0:work"));
    assert_eq!(
        custom.kind,
        EventKind::Custom {
            name: "progress".into(),
            payload: json!({ "done": 2, "of": 3 })
        }
    );
}

#[tokio::test]
async fn without_a_subscriber_custom_events_are_ignored() {
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "work",
            node_with_context(|ctx, _| async move {
                assert!(!ctx.events().is_enabled());
                ctx.events().emit("progress", 1);
                Ok(NodeOutput::Update(Update::new().set("ok", true)))
            }),
        )
        .set_entry("work")
        .add_edge("work", END);
    let workflow = builder.build().unwrap();

    let output = workflow.run(State::new()).await.unwrap();
    assert_eq!(output.state.get("ok"), Some(&json!(true)));
}

#[tokio::test(start_paused = true)]
async fn failure_reports_failed_cancelled_and_run_finished() {
    let llm = Arc::new(FakeLlm::new().failing_on("FAIL"));
    let slow = Arc::new(FakeLlm::with_delay(
        Duration::from_secs(30),
        Duration::from_secs(30),
    ));
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("start", noop())
        .add_node("good", ask(&slow, "good", "fine"))
        .add_node("bad", ask(&llm, "bad", "please FAIL"))
        .set_entry("start")
        .add_edge("start", "good")
        .add_edge("start", "bad")
        .add_edge("good", END)
        .add_edge("bad", END);
    let workflow = builder.build().unwrap();

    let (events, result) = collect(&workflow, State::new()).await;
    assert_eq!(result.unwrap_err().kind.code(), "node_failed");

    let tail = trace(&events);
    assert_eq!(
        tail[tail.len() - 3..],
        ["node_failed:bad", "node_cancelled:good", "run_finished"]
    );
    match &events[events.len() - 3].kind {
        EventKind::NodeFailed { code, message, .. } => {
            assert_eq!(code, "node_failed");
            assert!(message.contains("simulated model failure"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(
        events.last().unwrap().kind,
        EventKind::RunFinished {
            status: "failed".into(),
            error: Some("node_failed".into()),
            steps: 1
        }
    );
    // No step_committed for the failed step.
    assert_eq!(
        events
            .iter()
            .filter(|e| e.type_name() == "step_committed")
            .count(),
        1
    );
    assert_contiguous(&events, 0);
}

#[tokio::test]
async fn panics_and_step_limits_also_end_with_run_finished() {
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "boom",
            node_fn(|_| async {
                if true {
                    panic!("kaboom");
                }
                Ok(Update::new())
            }),
        )
        .set_entry("boom")
        .add_edge("boom", END);
    let (events, result) = collect(&builder.build().unwrap(), State::new()).await;
    assert_eq!(result.unwrap_err().kind.code(), "node_panicked");
    match &events[events.len() - 2].kind {
        EventKind::NodeFailed { code, message, .. } => {
            assert_eq!(code, "node_panicked");
            assert_eq!(message, "kaboom");
        }
        other => panic!("unexpected {other:?}"),
    }

    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("loop", noop())
        .set_entry("loop")
        .add_router(
            "loop",
            router_fn(|_| async { Ok::<_, NodeError>("again") }),
            [("again", "loop"), ("done", END)],
        );
    let workflow = builder.build().unwrap();
    let mut run = workflow.stream_with(State::new(), RunOptions::default().max_steps(3));
    let mut last = None;
    while let Some(event) = run.next().await {
        last = Some(event);
    }
    assert_eq!(
        run.finish().await.unwrap_err().kind.code(),
        "step_limit_exceeded"
    );
    assert_eq!(
        last.unwrap().kind,
        EventKind::RunFinished {
            status: "failed".into(),
            error: Some("step_limit_exceeded".into()),
            steps: 3
        }
    );
}

#[tokio::test]
async fn event_numbering_continues_across_suspend_and_resume() {
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("draft", noop())
        .add_node(
            "review",
            node_with_context(|ctx, _| async move {
                Ok(match ctx.resume() {
                    None => NodeOutput::suspend(Update::new(), json!({ "question": "ok?" })),
                    Some(_) => NodeOutput::Update(Update::new().set("approved", true)),
                })
            }),
        )
        .set_entry("draft")
        .add_edge("draft", "review")
        .add_edge("review", END);
    let workflow = builder.build().unwrap();

    let (first, result) = collect(&workflow, State::new()).await;
    let paused = result.unwrap();
    let tail = trace(&first);
    assert_eq!(
        tail[tail.len() - 4..],
        [
            "node_finished:review",
            "step_committed",
            "suspended",
            "run_finished"
        ]
    );
    match &first[first.len() - 2].kind {
        EventKind::Suspended { interrupts } => {
            assert_eq!(interrupts[0].node, "review");
            assert_eq!(interrupts[0].payload, json!({ "question": "ok?" }));
        }
        other => panic!("unexpected {other:?}"),
    }
    let snapshot = paused.snapshot().unwrap().clone();
    assert_eq!(snapshot.event_seq, first.len() as u64);

    // The snapshot survives JSON, including the event counter.
    let snapshot: agentbuild_core::Snapshot =
        serde_json::from_str(&serde_json::to_string(&snapshot).unwrap()).unwrap();

    let (sender, mut receiver) = event_channel(64);
    let resume = workflow.resume_with(
        &snapshot,
        [("review", json!("yes"))],
        RunOptions::default().events(sender),
    );
    let read = async {
        let mut events = Vec::new();
        while let Some(event) = receiver.recv().await {
            events.push(event);
        }
        events
    };
    let (result, second) = tokio::join!(resume, read);
    result.unwrap();

    assert_eq!(second[0].kind, EventKind::RunStarted { resumed: true });
    assert_contiguous(&second, first.len() as u64);
    assert!(second.iter().all(|e| e.run_id == paused.run_id));
}

#[test]
fn snapshots_without_event_seq_still_load() {
    let json = json!({
        "format": "agentbuild.snapshot/1", "workflow_fingerprint": "x", "run_id": "r",
        "step": 1, "state": {}, "frontier": [], "join_progress": {}, "suspended": []
    });
    let snapshot: agentbuild_core::Snapshot = serde_json::from_value(json).unwrap();
    assert_eq!(snapshot.event_seq, 0);
}

#[tokio::test(start_paused = true)]
async fn slow_subscriber_gets_every_event_and_never_blocks_a_node() {
    const EMITS: usize = 50;
    let node_time = Arc::new(std::sync::Mutex::new(None));
    let after_ran_at = Arc::new(std::sync::Mutex::new(None));
    let clock = Instant::now();
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("chatty", {
            let node_time = node_time.clone();
            node_with_context(move |ctx, _| {
                let node_time = node_time.clone();
                async move {
                    let start = Instant::now();
                    for n in 0..EMITS {
                        ctx.events().emit("tick", n);
                    }
                    *node_time.lock().unwrap() = Some(start.elapsed());
                    Ok(NodeOutput::Update(Update::new()))
                }
            })
        })
        .add_node("after", {
            let after_ran_at = after_ran_at.clone();
            node_fn(move |_| {
                let after_ran_at = after_ran_at.clone();
                async move {
                    *after_ran_at.lock().unwrap() = Some(clock.elapsed());
                    Ok(Update::new())
                }
            })
        })
        .set_entry("chatty")
        .add_edge("chatty", "after")
        .add_edge("after", END);
    let workflow = builder.build().unwrap();

    // Room for one event; the reader takes 10 ms per event.
    let (sender, mut receiver) = event_channel(1);
    let run = workflow.run_with(State::new(), RunOptions::default().events(sender));
    let read = async {
        let mut events = Vec::new();
        while let Some(event) = receiver.recv().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
            events.push(event);
        }
        events
    };
    let (result, events) = tokio::join!(run, read);
    result.unwrap();

    assert_eq!(
        *node_time.lock().unwrap(),
        Some(Duration::ZERO),
        "emitting never waited for the reader"
    );
    let ticks: Vec<Value> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Custom { payload, .. } => Some(payload.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(ticks, (0..EMITS).map(Value::from).collect::<Vec<_>>());
    assert_contiguous(&events, 0);
    assert_eq!(events.last().unwrap().type_name(), "run_finished");
    // Backpressure happens at the step boundary: `after` (step 1) ran only
    // once the reader had worked through step 0's backlog of 50+ events at
    // 10 ms each, so an unread backlog never grows across steps.
    let after_ran_at = after_ran_at.lock().unwrap().unwrap();
    assert!(
        after_ran_at >= Duration::from_millis(10 * EMITS as u64),
        "step 1 started at {after_ran_at:?}"
    );
}

#[tokio::test]
async fn dropped_receiver_does_not_change_the_run() {
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("a", node_fn(|_| async { Ok(Update::new().set("x", 1)) }))
        .add_node("b", node_fn(|_| async { Ok(Update::new().set("y", 2)) }))
        .set_entry("a")
        .add_edge("a", "b")
        .add_edge("b", END);
    let workflow = builder.build().unwrap();

    let plain = workflow.run(State::new()).await.unwrap();

    let (sender, receiver) = event_channel(1);
    drop(receiver);
    let observed = workflow
        .run_with(State::new(), RunOptions::default().events(sender))
        .await
        .unwrap();

    assert_eq!(observed.state, plain.state);
    assert_eq!(observed.steps, plain.steps);
}

#[tokio::test(start_paused = true)]
async fn dropping_a_run_stream_cancels_the_run() {
    let finished = Arc::new(AtomicBool::new(false));
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("slow", {
            let finished = finished.clone();
            node_fn(move |_| {
                let finished = finished.clone();
                async move {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    finished.store(true, Ordering::SeqCst);
                    Ok(Update::new())
                }
            })
        })
        .set_entry("slow")
        .add_edge("slow", END);
    let workflow = builder.build().unwrap();

    let mut run = workflow.stream(State::new());
    // Wait until the node is running.
    while let Some(event) = run.next().await {
        if event.type_name() == "node_started" {
            break;
        }
    }
    drop(run);
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert!(!finished.load(Ordering::SeqCst));
}

#[tokio::test]
async fn events_serialize_with_type_and_data() {
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("a", noop())
        .set_entry("a")
        .add_edge("a", END);
    let (events, _) = collect(&builder.build().unwrap(), State::new()).await;

    let json = serde_json::to_value(&events[2]).unwrap();
    assert_eq!(json["type"], "node_started");
    assert_eq!(json["data"], json!({ "attempt": 1 }));
    assert_eq!(json["node"], "a");
    assert!(json["ts_ms"].as_u64().unwrap() > 0);
    let run_started = serde_json::to_value(&events[0]).unwrap();
    assert!(
        run_started.get("node").is_none(),
        "absent fields are omitted"
    );
}
