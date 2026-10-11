//! Node context, `Command` goto, `Suspend`/resume with snapshots, and reducers
//! inside real runs (design 0005 §1–§3).

use std::sync::{Arc, Mutex};

use agentbuild_core::{
    END, Node, NodeError, NodeOutput, Reducer, RunErrorKind, RunOptions, SNAPSHOT_FORMAT, Snapshot,
    State, Update, ValidationIssue, Workflow, WorkflowBuilder, node_fn, node_with_context,
};
use serde_json::{Value, json};

fn noop() -> impl Node {
    node_fn(|_| async { Ok(Update::new()) })
}

fn names(out: &agentbuild_core::RunOutput) -> Vec<Vec<&str>> {
    out.steps
        .iter()
        .map(|s| s.nodes.iter().map(String::as_str).collect())
        .collect()
}

// ---------- NodeContext ----------

/// (run_id, step, node, task_id, attempt) as observed by a node.
type Seen = Arc<Mutex<Vec<(String, usize, String, String, u32)>>>;

#[tokio::test]
async fn context_exposes_run_step_node_and_task() {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let record = |seen: Seen| {
        node_with_context(move |ctx, _| {
            let seen = seen.clone();
            async move {
                assert!(ctx.resume().is_none());
                seen.lock().unwrap().push((
                    ctx.run_id().to_string(),
                    ctx.step(),
                    ctx.node().to_string(),
                    ctx.task_id(),
                    ctx.attempt(),
                ));
                Ok(Update::new().into())
            }
        })
    };
    let mut b = WorkflowBuilder::new();
    b.add_node("a", record(seen.clone()))
        .add_node("b", record(seen.clone()))
        .set_entry("a")
        .add_edge("a", "b")
        .add_edge("b", END);
    let out = b.build().unwrap().run(State::new()).await.unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let run_id = &out.run_id;
    assert_eq!(
        seen[0],
        (run_id.clone(), 0, "a".into(), format!("{run_id}:0:a"), 1)
    );
    assert_eq!(
        seen[1],
        (run_id.clone(), 1, "b".into(), format!("{run_id}:1:b"), 1)
    );
}

#[tokio::test]
async fn each_run_gets_a_new_run_id() {
    let mut b = WorkflowBuilder::new();
    b.add_node("a", noop()).set_entry("a").add_edge("a", END);
    let wf = b.build().unwrap();
    let (x, y) = (
        wf.run(State::new()).await.unwrap(),
        wf.run(State::new()).await.unwrap(),
    );
    assert_ne!(x.run_id, y.run_id);
    assert!(!x.is_suspended());
}

// ---------- Command / goto ----------

fn goto_node(targets: Vec<&'static str>) -> impl Node {
    node_with_context(move |_, _| {
        let targets = targets.clone();
        async move {
            Ok(NodeOutput::goto(
                Update::new().set("went", json!(targets)),
                targets,
            ))
        }
    })
}

#[tokio::test]
async fn goto_schedules_declared_targets_in_addition_to_edges() {
    let mut b = WorkflowBuilder::new();
    b.add_node("decide", goto_node(vec!["special"]))
        .add_node("always", noop())
        .add_node("special", noop())
        .set_entry("decide")
        .add_edge("decide", "always")
        .may_goto("decide", ["special"])
        .add_edge("always", END)
        .add_edge("special", END);
    let out = b.build().unwrap().run(State::new()).await.unwrap();
    assert_eq!(names(&out), vec![vec!["decide"], vec!["always", "special"]]);
}

#[tokio::test]
async fn goto_to_end_finishes_the_path() {
    let mut b = WorkflowBuilder::new();
    b.add_node("decide", goto_node(vec![END]))
        .add_node("never", noop())
        .set_entry("decide")
        .may_goto("decide", ["never", END])
        .add_edge("never", END);
    let out = b.build().unwrap().run(State::new()).await.unwrap();
    assert_eq!(names(&out), vec![vec!["decide"]]);
}

#[tokio::test]
async fn goto_to_undeclared_target_fails_without_committing() {
    let mut b = WorkflowBuilder::new();
    b.add_node("decide", goto_node(vec!["elsewhere"]))
        .add_node("ok", noop())
        .add_node("elsewhere", noop())
        .set_entry("decide")
        .may_goto("decide", ["ok"])
        .add_edge("ok", END)
        .add_edge("elsewhere", END)
        .add_edge("ok", "elsewhere");
    let err = b.build().unwrap().run(State::new()).await.unwrap_err();
    assert_eq!(
        err.kind,
        RunErrorKind::UnknownRoute {
            node: "decide".into(),
            label: "elsewhere".into(),
            expected: vec!["ok".into()],
        }
    );
    assert_eq!(err.state.get("went"), None);
}

#[tokio::test]
async fn goto_only_node_must_name_a_target() {
    let mut b = WorkflowBuilder::new();
    b.add_node("decide", noop()) // returns a plain Update
        .add_node("next", noop())
        .set_entry("decide")
        .may_goto("decide", ["next"])
        .add_edge("next", END);
    let err = b.build().unwrap().run(State::new()).await.unwrap_err();
    assert_eq!(
        err.kind,
        RunErrorKind::MissingGoto {
            node: "decide".into()
        }
    );
    assert_eq!(err.code(), "missing_goto");
}

#[test]
fn goto_validation() {
    let mut b = WorkflowBuilder::new();
    b.add_node("a", noop())
        .add_node("b", noop())
        .set_entry("a")
        .may_goto("a", ["b", "ghost"])
        .add_edge("b", END);
    let issues = b.build().err().unwrap().issues;
    assert_eq!(
        issues,
        vec![ValidationIssue::UnknownNode {
            node: "ghost".into(),
            context: "may_goto of `a`".into()
        }]
    );

    let mut b = WorkflowBuilder::new();
    b.add_node("a", noop())
        .add_node("b", noop())
        .set_entry("a")
        .may_goto("a", ["b"])
        .add_router(
            "a",
            agentbuild_core::router_fn(|_| async { Ok::<_, NodeError>("x") }),
            [("x", "b")],
        )
        .add_edge("b", END);
    assert!(
        b.build()
            .err()
            .unwrap()
            .issues
            .contains(&ValidationIssue::GotoWithRouter("a".into()))
    );
}

// ---------- Suspend / resume ----------

/// A node that asks for approval and, once resumed, records the decision.
fn approval() -> impl Node {
    node_with_context(|ctx, state| async move {
        match ctx.resume() {
            None => Ok(NodeOutput::suspend(
                Update::new().set("status", "waiting"),
                json!({"question": "send email?", "draft": state.get("draft").cloned()}),
            )),
            Some(decision) => Ok(Update::new()
                .set("status", "decided")
                .set("decision", decision.clone())
                .into()),
        }
    })
}

fn approval_workflow() -> Workflow {
    let mut b = WorkflowBuilder::new();
    b.add_node(
        "draft",
        node_fn(|_| async { Ok(Update::new().set("draft", "hello")) }),
    )
    .add_node("approve", approval())
    .add_node(
        "send",
        node_fn(|s| async move {
            let ok = s.get("decision") == Some(&json!("yes"));
            Ok(Update::new().set("sent", ok))
        }),
    )
    .set_entry("draft")
    .add_edge("draft", "approve")
    .add_edge("approve", "send")
    .add_edge("send", END);
    b.build().unwrap()
}

#[tokio::test]
async fn suspend_commits_and_returns_a_resumable_snapshot() {
    let wf = approval_workflow();
    let paused = wf.run(State::new()).await.unwrap();

    assert!(paused.is_suspended());
    assert_eq!(names(&paused), vec![vec!["draft"], vec!["approve"]]);
    assert_eq!(paused.state.get("status"), Some(&json!("waiting")));
    let interrupts = paused.interrupts();
    assert_eq!(interrupts.len(), 1);
    assert_eq!(interrupts[0].node, "approve");
    assert_eq!(
        interrupts[0].payload,
        json!({"question": "send email?", "draft": "hello"})
    );

    let snap = paused.snapshot().unwrap();
    assert_eq!(snap.format, SNAPSHOT_FORMAT);
    assert_eq!(snap.workflow_fingerprint, wf.fingerprint());
    assert_eq!(snap.step, 2);
    assert_eq!(snap.frontier, vec!["approve"]);

    let done = wf.resume(snap, [("approve", json!("yes"))]).await.unwrap();
    assert!(!done.is_suspended());
    assert_eq!(done.run_id, paused.run_id);
    assert_eq!(names(&done), vec![vec!["approve"], vec!["send"]]);
    assert_eq!(done.steps[0].step, 2);
    assert_eq!(done.state.get("sent"), Some(&json!(true)));
    assert_eq!(done.state.get("status"), Some(&json!("decided")));
}

#[tokio::test]
async fn snapshot_survives_json_round_trip() {
    let wf = approval_workflow();
    let paused = wf.run(State::new()).await.unwrap();
    // e.g. stored in a database, loaded by another process
    let json = serde_json::to_string(paused.snapshot().unwrap()).unwrap();
    let restored: Snapshot = serde_json::from_str(&json).unwrap();

    let done = wf
        .resume(&restored, [("approve", json!("no"))])
        .await
        .unwrap();
    assert_eq!(done.state.get("sent"), Some(&json!(false)));
}

#[tokio::test]
async fn resume_rejects_bad_input_and_leaves_snapshot_usable() {
    let wf = approval_workflow();
    let paused = wf.run(State::new()).await.unwrap();
    let snap = paused.snapshot().unwrap();

    let missing = wf
        .resume(snap, Vec::<(&str, Value)>::new())
        .await
        .unwrap_err();
    assert_eq!(missing.code(), "resume_invalid");
    assert!(
        missing
            .to_string()
            .contains("missing resume value for suspended node `approve`")
    );

    let extra = wf
        .resume(snap, [("approve", json!("yes")), ("send", json!(1))])
        .await
        .unwrap_err();
    assert!(extra.to_string().contains("node `send` is not suspended"));

    let mut wrong_format = snap.clone();
    wrong_format.format = "agentbuild.snapshot/99".into();
    let err = wf
        .resume(&wrong_format, [("approve", json!("yes"))])
        .await
        .unwrap_err();
    assert_eq!(err.code(), "resume_invalid");

    // The original snapshot is untouched and still resumes.
    assert!(wf.resume(snap, [("approve", json!("yes"))]).await.is_ok());
}

#[tokio::test]
async fn resume_on_a_different_workflow_is_rejected() {
    let paused = approval_workflow().run(State::new()).await.unwrap();
    let mut b = WorkflowBuilder::new();
    b.add_node("draft", noop())
        .add_node("approve", approval())
        .set_entry("draft")
        .add_edge("draft", "approve")
        .add_edge("approve", END); // no `send`: different shape
    let other = b.build().unwrap();
    let err = other
        .resume(paused.snapshot().unwrap(), [("approve", json!("yes"))])
        .await
        .unwrap_err();
    assert_eq!(err.code(), "workflow_mismatch");
}

#[tokio::test]
async fn sibling_of_a_suspended_node_commits_and_its_successor_waits_for_resume() {
    let mut b = WorkflowBuilder::new();
    b.add_node("start", noop())
        .add_node("approve", approval())
        .add_node(
            "research",
            node_fn(|_| async { Ok(Update::new().set("facts", 3)) }),
        )
        .add_node("summarize", noop())
        .add_node("send", noop())
        .set_entry("start")
        .add_edge("start", "approve")
        .add_edge("start", "research")
        .add_edge("research", "summarize")
        .add_edge("approve", "send")
        .add_edge("summarize", END)
        .add_edge("send", END);
    let wf = b.build().unwrap();

    let paused = wf.run(State::new()).await.unwrap();
    assert_eq!(paused.state.get("facts"), Some(&json!(3))); // sibling committed
    assert_eq!(
        paused.snapshot().unwrap().frontier,
        vec!["approve", "summarize"]
    );

    let done = wf
        .resume(paused.snapshot().unwrap(), [("approve", json!("yes"))])
        .await
        .unwrap();
    assert_eq!(
        names(&done),
        vec![vec!["approve", "summarize"], vec!["send"]]
    );
}

#[tokio::test]
async fn join_waits_for_a_suspended_source() {
    let mut b = WorkflowBuilder::new();
    b.add_node("start", noop())
        .add_node("approve", approval())
        .add_node("other", noop())
        .add_node(
            "combine",
            node_fn(|_| async { Ok(Update::new().set("combined", true)) }),
        )
        .set_entry("start")
        .add_edge("start", "approve")
        .add_edge("start", "other")
        .add_join(["approve", "other"], "combine")
        .add_edge("combine", END);
    let wf = b.build().unwrap();

    let paused = wf.run(State::new()).await.unwrap();
    let snap = paused.snapshot().unwrap();
    assert_eq!(
        snap.join_progress.get("combine"),
        Some(&vec!["other".to_string()])
    );
    assert_eq!(paused.state.get("combined"), None);

    let done = wf.resume(snap, [("approve", json!("yes"))]).await.unwrap();
    assert_eq!(names(&done), vec![vec!["approve"], vec!["combine"]]);
    assert_eq!(done.state.get("combined"), Some(&json!(true)));
}

#[tokio::test]
async fn step_limit_counts_across_resume() {
    let wf = approval_workflow();
    let paused = wf.run(State::new()).await.unwrap(); // used steps 0 and 1
    let err = wf
        .resume_with(
            paused.snapshot().unwrap(),
            [("approve", json!("yes"))],
            RunOptions::default().max_steps(3),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind, RunErrorKind::StepLimitExceeded { limit: 3 });
}

// ---------- Reducers in a run ----------

#[tokio::test]
async fn parallel_nodes_append_messages_with_upsert_by_id() {
    let say = |id: &'static str, text: &'static str| {
        node_fn(move |_| async move {
            Ok(Update::new().set("messages", json!([{"id": id, "text": text}])))
        })
    };
    let mut b = WorkflowBuilder::new();
    b.add_node("start", say("m1", "hi"))
        .add_node("left", say("m2", "from left"))
        .add_node("right", say("m3", "from right"))
        .add_node("edit", say("m1", "hi (edited)"))
        .set_entry("start")
        .add_edge("start", "left")
        .add_edge("start", "right")
        .add_join(["left", "right"], "edit")
        .add_edge("edit", END)
        .reducer("messages", Reducer::UpsertById);
    let out = b.build().unwrap().run(State::new()).await.unwrap();
    assert_eq!(
        out.state.get("messages").unwrap(),
        &json!([
            {"id": "m1", "text": "hi (edited)"},
            {"id": "m2", "text": "from left"},
            {"id": "m3", "text": "from right"}
        ])
    );
}

#[tokio::test]
async fn invalid_reducer_write_fails_the_step() {
    let mut b = WorkflowBuilder::new();
    b.add_node(
        "bad",
        node_fn(|_| async { Ok(Update::new().set("log", "not a list")) }),
    )
    .set_entry("bad")
    .add_edge("bad", END)
    .reducer("log", Reducer::Append);
    let err = b.build().unwrap().run(State::new()).await.unwrap_err();
    assert_eq!(
        err.kind,
        RunErrorKind::InvalidValue {
            node: "bad".into(),
            key: "log".into(),
            message: "append expects a list".into(),
        }
    );
}

#[test]
fn duplicate_reducer_is_a_build_error() {
    let mut b = WorkflowBuilder::new();
    b.add_node("a", noop())
        .set_entry("a")
        .add_edge("a", END)
        .reducer("k", Reducer::Append)
        .reducer("k", Reducer::Replace);
    assert_eq!(
        b.build().err().unwrap().issues,
        vec![ValidationIssue::DuplicateReducer("k".into())]
    );
}

#[test]
fn fingerprint_tracks_structure_not_implementation() {
    let build = |reducer: Option<Reducer>, impl_b: bool| {
        let mut b = WorkflowBuilder::new();
        if impl_b {
            b.add_node("a", node_fn(|_| async { Ok(Update::new().set("x", 1)) }));
        } else {
            b.add_node("a", noop());
        }
        b.set_entry("a").add_edge("a", END);
        if let Some(r) = reducer {
            b.reducer("messages", r);
        }
        b.build().unwrap().fingerprint().to_string()
    };
    let base = build(None, false);
    assert!(base.starts_with("fnv1a64:"));
    assert_eq!(
        base,
        build(None, true),
        "implementation change keeps fingerprint"
    );
    assert_ne!(
        base,
        build(Some(Reducer::Append), false),
        "reducer change alters fingerprint"
    );
}
