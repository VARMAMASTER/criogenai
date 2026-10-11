//! Runtime behaviour: ordering, state passing, routing, joins, and failures.

use std::sync::Arc;
use std::time::Duration;

use agentbuild_core::{
    END, Node, NodeError, RunErrorKind, RunOptions, State, Update, WorkflowBuilder, node_fn,
    router_fn,
};
use serde_json::{Value, json};
use tokio::sync::Barrier;

/// A node that appends its name to the `trail` key it reads.
fn append(name: &'static str) -> impl Node {
    node_fn(move |state| async move {
        let mut trail = state.get("trail").cloned().unwrap_or(json!([]));
        trail.as_array_mut().unwrap().push(json!(name));
        Ok(Update::new().set("trail", trail))
    })
}

fn noop() -> impl Node {
    node_fn(|_| async { Ok(Update::new()) })
}

fn trail(state: &State) -> Value {
    state.get("trail").cloned().unwrap_or(Value::Null)
}

fn step_names(steps: &[agentbuild_core::StepRecord]) -> Vec<Vec<&str>> {
    steps
        .iter()
        .map(|s| s.nodes.iter().map(String::as_str).collect())
        .collect()
}

#[tokio::test]
async fn linear_workflow_passes_state_in_order() {
    let mut b = WorkflowBuilder::new();
    b.add_node("a", append("a"))
        .add_node("b", append("b"))
        .add_node("c", append("c"))
        .set_entry("a")
        .add_edge("a", "b")
        .add_edge("b", "c")
        .add_edge("c", END);

    let out = b.build().unwrap().run(State::new()).await.unwrap();
    assert_eq!(trail(&out.state), json!(["a", "b", "c"]));
    assert_eq!(
        step_names(&out.steps),
        vec![vec!["a"], vec!["b"], vec!["c"]]
    );
}

#[tokio::test]
async fn input_keys_survive_untouched() {
    let mut b = WorkflowBuilder::new();
    b.add_node("a", append("a"))
        .set_entry("a")
        .add_edge("a", END);
    let out = b
        .build()
        .unwrap()
        .run(State::new().with("keep", 7))
        .await
        .unwrap();
    assert_eq!(out.state.get("keep"), Some(&json!(7)));
}

fn branching() -> agentbuild_core::Workflow {
    let mut b = WorkflowBuilder::new();
    b.add_node(
        "measure",
        node_fn(|s| async move {
            let n = s.get("n").and_then(Value::as_i64).unwrap_or(0);
            Ok(Update::new().set("big", n > 10))
        }),
    )
    .add_node(
        "big",
        node_fn(|_| async { Ok(Update::new().set("out", "big")) }),
    )
    .add_node(
        "small",
        node_fn(|_| async { Ok(Update::new().set("out", "small")) }),
    )
    .set_entry("measure")
    .add_router(
        "measure",
        // Routers see the step's merged updates.
        router_fn(|s| async move {
            Ok::<_, NodeError>(if s.get("big") == Some(&json!(true)) {
                "yes"
            } else {
                "no"
            })
        }),
        [("yes", "big"), ("no", "small")],
    )
    .add_edge("big", END)
    .add_edge("small", END);
    b.build().unwrap()
}

#[tokio::test]
async fn router_selects_branch_from_updated_state() {
    let wf = branching();
    let big = wf.run(State::new().with("n", 50)).await.unwrap();
    assert_eq!(big.state.get("out"), Some(&json!("big")));
    assert_eq!(step_names(&big.steps), vec![vec!["measure"], vec!["big"]]);

    let small = wf.run(State::new().with("n", 1)).await.unwrap();
    assert_eq!(small.state.get("out"), Some(&json!("small")));
}

#[tokio::test]
async fn router_can_route_to_end() {
    let mut b = WorkflowBuilder::new();
    b.add_node("a", noop())
        .add_node("b", noop())
        .set_entry("a")
        .add_router(
            "a",
            router_fn(|_| async { Ok::<_, NodeError>("stop") }),
            [("stop", END), ("go", "b")],
        )
        .add_edge("b", END);
    let out = b.build().unwrap().run(State::new()).await.unwrap();
    assert_eq!(step_names(&out.steps), vec![vec!["a"]]);
}

#[tokio::test]
async fn fan_out_runs_nodes_concurrently() {
    // Both nodes wait on a 2-party barrier: this only completes if they run at
    // the same time.
    let barrier = Arc::new(Barrier::new(2));
    let waiter = |key: &'static str| {
        let barrier = barrier.clone();
        node_fn(move |_| {
            let barrier = barrier.clone();
            async move {
                barrier.wait().await;
                Ok(Update::new().set(key, true))
            }
        })
    };
    let mut b = WorkflowBuilder::new();
    b.add_node("start", noop())
        .add_node("left", waiter("left"))
        .add_node("right", waiter("right"))
        .set_entry("start")
        .add_edge("start", "left")
        .add_edge("start", "right")
        .add_edge("left", END)
        .add_edge("right", END);
    let wf = b.build().unwrap();

    let out = tokio::time::timeout(Duration::from_secs(5), wf.run(State::new()))
        .await
        .expect("fan-out nodes did not run concurrently")
        .unwrap();
    assert_eq!(out.state.into_value(), json!({"left": true, "right": true}));
    assert_eq!(
        step_names(&out.steps),
        vec![vec!["start"], vec!["left", "right"]]
    );
}

#[tokio::test]
async fn diamond_runs_shared_successor_once() {
    let mut b = WorkflowBuilder::new();
    b.add_node("a", noop())
        .add_node("b", noop())
        .add_node("c", noop())
        .add_node("d", noop())
        .set_entry("a")
        .add_edge("a", "b")
        .add_edge("a", "c")
        .add_edge("b", "d")
        .add_edge("c", "d")
        .add_edge("d", END);
    let out = b.build().unwrap().run(State::new()).await.unwrap();
    assert_eq!(
        step_names(&out.steps),
        vec![vec!["a"], vec!["b", "c"], vec!["d"]]
    );
}

#[tokio::test]
async fn join_waits_for_branches_of_different_lengths() {
    let mut b = WorkflowBuilder::new();
    b.add_node("start", noop())
        .add_node(
            "short",
            node_fn(|_| async { Ok(Update::new().set("short", 1)) }),
        )
        .add_node("long1", noop())
        .add_node(
            "long2",
            node_fn(|_| async { Ok(Update::new().set("long", 2)) }),
        )
        .add_node(
            "combine",
            node_fn(|s| async move {
                let sum = s.get("short").and_then(Value::as_i64).unwrap()
                    + s.get("long").and_then(Value::as_i64).unwrap();
                Ok(Update::new().set("sum", sum))
            }),
        )
        .set_entry("start")
        .add_edge("start", "short")
        .add_edge("start", "long1")
        .add_edge("long1", "long2")
        .add_join(["short", "long2"], "combine")
        .add_edge("combine", END);
    let out = b.build().unwrap().run(State::new()).await.unwrap();
    assert_eq!(out.state.get("sum"), Some(&json!(3)));
    assert_eq!(
        step_names(&out.steps),
        vec![
            vec!["start"],
            vec!["short", "long1"],
            vec!["long2"],
            vec!["combine"]
        ]
    );
}

fn counter_loop() -> WorkflowBuilder {
    let mut b = WorkflowBuilder::new();
    b.add_node(
        "inc",
        node_fn(|s| async move {
            let n = s.get("n").and_then(Value::as_i64).unwrap_or(0);
            Ok(Update::new().set("n", n + 1))
        }),
    )
    .set_entry("inc")
    .add_router(
        "inc",
        router_fn(|s| async move {
            let n = s.get("n").and_then(Value::as_i64).unwrap();
            Ok::<_, NodeError>(if n < 3 { "again" } else { "done" })
        }),
        [("again", "inc"), ("done", END)],
    );
    b
}

#[tokio::test]
async fn cycles_run_until_router_exits() {
    let out = counter_loop()
        .build()
        .unwrap()
        .run(State::new())
        .await
        .unwrap();
    assert_eq!(out.state.get("n"), Some(&json!(3)));
    assert_eq!(out.steps.len(), 3);
}

#[tokio::test]
async fn step_limit_stops_runaway_cycles() {
    let wf = counter_loop().build().unwrap();
    let err = wf
        .run_with(State::new(), RunOptions::default().max_steps(2))
        .await
        .unwrap_err();
    assert_eq!(err.kind, RunErrorKind::StepLimitExceeded { limit: 2 });
    assert_eq!(err.code(), "step_limit_exceeded");
    assert_eq!(err.step, 2);
    assert_eq!(err.state.get("n"), Some(&json!(2)));
}

#[tokio::test]
async fn node_error_reports_node_and_pre_step_state() {
    let mut b = WorkflowBuilder::new();
    b.add_node("ok", node_fn(|_| async { Ok(Update::new().set("x", 1)) }))
        .add_node(
            "bad",
            node_fn(|_| async {
                Err(NodeError::new("boom")
                    .with_kind("ValueError")
                    .with_details(json!({"retry": false})))
            }),
        )
        .set_entry("ok")
        .add_edge("ok", "bad")
        .add_edge("bad", END);
    let err = b.build().unwrap().run(State::new()).await.unwrap_err();

    assert_eq!(err.code(), "node_failed");
    assert_eq!(err.step, 1);
    assert_eq!(err.state.into_value(), json!({"x": 1}));
    let RunErrorKind::NodeFailed { node, source } = err.kind else {
        panic!("wrong kind")
    };
    assert_eq!(node, "bad");
    assert_eq!(source.kind.as_deref(), Some("ValueError"));
    assert_eq!(source.to_string(), "ValueError: boom");
}

#[tokio::test]
async fn failed_step_discards_sibling_updates() {
    let mut b = WorkflowBuilder::new();
    b.add_node("start", noop())
        .add_node(
            "writes",
            node_fn(|_| async { Ok(Update::new().set("partial", true)) }),
        )
        .add_node(
            "fails",
            node_fn(|_| async {
                // Let the sibling finish first so its update is definitely produced.
                tokio::time::sleep(Duration::from_millis(20)).await;
                Err(NodeError::new("nope"))
            }),
        )
        .set_entry("start")
        .add_edge("start", "writes")
        .add_edge("start", "fails")
        .add_edge("writes", END)
        .add_edge("fails", END);
    let err = b.build().unwrap().run(State::new()).await.unwrap_err();
    assert_eq!(err.code(), "node_failed");
    assert_eq!(err.state.get("partial"), None);
}

#[tokio::test]
async fn same_key_written_twice_in_a_step_is_a_conflict() {
    let mut b = WorkflowBuilder::new();
    b.add_node("start", noop())
        .add_node("x", node_fn(|_| async { Ok(Update::new().set("k", 1)) }))
        .add_node("y", node_fn(|_| async { Ok(Update::new().set("k", 2)) }))
        .set_entry("start")
        .add_edge("start", "x")
        .add_edge("start", "y")
        .add_edge("x", END)
        .add_edge("y", END);
    let err = b.build().unwrap().run(State::new()).await.unwrap_err();
    assert_eq!(
        err.kind,
        RunErrorKind::UpdateConflict {
            key: "k".into(),
            first: "x".into(),
            second: "y".into()
        }
    );
    assert_eq!(err.state.get("k"), None);
}

#[tokio::test]
async fn unknown_route_label_fails_without_committing_step() {
    let mut b = WorkflowBuilder::new();
    b.add_node("a", node_fn(|_| async { Ok(Update::new().set("a", 1)) }))
        .set_entry("a")
        .add_router(
            "a",
            router_fn(|_| async { Ok::<_, NodeError>("nowhere") }),
            [("end", END)],
        );
    let err = b.build().unwrap().run(State::new()).await.unwrap_err();
    assert_eq!(
        err.kind,
        RunErrorKind::UnknownRoute {
            node: "a".into(),
            label: "nowhere".into(),
            expected: vec!["end".into()]
        }
    );
    assert_eq!(err.state.get("a"), None);
}

#[tokio::test]
async fn router_error_is_reported() {
    let mut b = WorkflowBuilder::new();
    b.add_node("a", noop()).set_entry("a").add_router(
        "a",
        router_fn(|_| async { Err::<String, _>(NodeError::new("no route")) }),
        [("end", END)],
    );
    let err = b.build().unwrap().run(State::new()).await.unwrap_err();
    assert_eq!(err.code(), "router_failed");
}

#[tokio::test]
async fn panicking_node_is_reported_not_propagated() {
    let mut b = WorkflowBuilder::new();
    b.add_node("p", node_fn(|_| async { panic!("kaboom") }))
        .set_entry("p")
        .add_edge("p", END);
    let err = b.build().unwrap().run(State::new()).await.unwrap_err();
    assert_eq!(
        err.kind,
        RunErrorKind::NodePanicked {
            node: "p".into(),
            message: "kaboom".into()
        }
    );
}

#[tokio::test]
async fn workflow_is_reusable_across_concurrent_runs() {
    let wf = branching();
    let shared = wf.clone();
    let (a, b) = tokio::join!(
        wf.run(State::new().with("n", 99)),
        shared.run(State::new().with("n", 0))
    );
    assert_eq!(a.unwrap().state.get("out"), Some(&json!("big")));
    assert_eq!(b.unwrap().state.get("out"), Some(&json!("small")));
}

#[test]
fn run_error_display_includes_step_and_cause() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut b = WorkflowBuilder::new();
    b.add_node("bad", node_fn(|_| async { Err(NodeError::new("boom")) }))
        .set_entry("bad")
        .add_edge("bad", END);
    let err = rt
        .block_on(b.build().unwrap().run(State::new()))
        .unwrap_err();
    assert_eq!(err.to_string(), "step 0: node `bad` failed: boom");
}
