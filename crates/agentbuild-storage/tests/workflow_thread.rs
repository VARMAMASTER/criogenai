//! Workflow threads (design 0008 §18): an expense approval workflow that
//! pauses for a human, kept in storage between calls and processes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use agentbuild_core::{
    END, NodeError, NodeOutput, State, Update, Workflow, WorkflowBuilder, node_with_context,
};
use agentbuild_storage::{AuditQuery, CheckpointStatus, Source, Storage, ThreadError};
use serde_json::{Value, json};

/// Counts runs of each node; `pay` fails while `fail_pay` is set.
#[derive(Clone, Default)]
struct Probe {
    drafts: Arc<AtomicUsize>,
    payments: Arc<AtomicUsize>,
    fail_pay: Arc<AtomicBool>,
}

/// draft → approve (pauses for a human) → pay.
fn expenses(probe: &Probe) -> Workflow {
    let drafts = probe.drafts.clone();
    let (payments, fail_pay) = (probe.payments.clone(), probe.fail_pay.clone());
    WorkflowBuilder::new()
        .add_node(
            "draft",
            node_with_context(move |_ctx, state| {
                let drafts = drafts.clone();
                async move {
                    drafts.fetch_add(1, Ordering::SeqCst);
                    let claims = state.get("claims").and_then(Value::as_u64).unwrap_or(0);
                    Ok(Update::new()
                        .set("status", "drafted")
                        .set("claims", claims + 1)
                        .into())
                }
            }),
        )
        .add_node(
            "approve",
            node_with_context(|ctx, state| async move {
                match ctx.resume() {
                    None => Ok(NodeOutput::suspend(
                        Update::new().set("status", "waiting"),
                        json!({"approve": state.get("amount").cloned().unwrap_or(json!(0))}),
                    )),
                    Some(answer) => Ok(Update::new()
                        .set("approved", answer.clone() == json!("yes"))
                        .set("status", "decided")
                        .into()),
                }
            }),
        )
        .add_node(
            "pay",
            node_with_context(move |_ctx, state| {
                let (payments, fail_pay) = (payments.clone(), fail_pay.clone());
                async move {
                    if fail_pay.load(Ordering::SeqCst) {
                        return Err(NodeError::new("the bank is down"));
                    }
                    let approved = state.get("approved") == Some(&json!(true));
                    if approved {
                        payments.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(Update::new()
                        .set("paid", approved)
                        .set("status", "done")
                        .into())
                }
            }),
        )
        .set_entry("draft")
        .add_edge("draft", "approve")
        .add_edge("approve", "pay")
        .add_edge("pay", END)
        .build()
        .unwrap()
}

fn input(amount: u64) -> State {
    State::new().with("amount", json!(amount))
}

#[tokio::test]
async fn a_paused_workflow_waits_in_storage_and_another_handle_resumes_it() {
    let storage = Storage::in_memory().with_audit();
    let probe = Probe::default();
    let workflow = expenses(&probe);
    let thread = storage
        .workflow_thread(&workflow, "expense-1")
        .actor("user:ada");

    let turn = thread.run(input(120)).await.unwrap();
    assert!(turn.is_suspended());
    assert_eq!(turn.interrupts()[0].node, "approve");
    assert_eq!(turn.interrupts()[0].payload, json!({"approve": 120}));
    assert_eq!(turn.state().get("status"), Some(&json!("waiting")));
    assert_eq!(thread.interrupts().await.unwrap().len(), 1);
    // Nothing else runs while it waits.
    let err = thread.run(input(5)).await.unwrap_err();
    assert_eq!(err.code(), "thread_suspended");
    // Input for a node that does not wait is refused, and it keeps waiting.
    let err = thread.resume([("pay", json!("yes"))]).await.unwrap_err();
    assert_eq!(err.code(), "resume_invalid");
    assert_eq!(thread.interrupts().await.unwrap().len(), 1);

    // Another handle (another process, in real life) decides.
    let manager = storage
        .workflow_thread(&expenses(&probe), "expense-1")
        .actor("user:boss");
    let turn = manager.resume([("approve", json!("yes"))]).await.unwrap();
    assert!(!turn.is_suspended());
    assert_eq!(turn.state().get("paid"), Some(&json!(true)));
    assert_eq!(probe.payments.load(Ordering::SeqCst), 1);
    assert_eq!(probe.drafts.load(Ordering::SeqCst), 1, "draft ran once");
    assert!(thread.interrupts().await.unwrap().is_empty());

    // The next run starts from the stored state, plus its input.
    let turn = thread.run(input(30)).await.unwrap();
    assert_eq!(turn.state().get("claims"), Some(&json!(2)));
    assert_eq!(turn.state().get("amount"), Some(&json!(30)));
    let history = thread.history(20).await.unwrap();
    assert_eq!(history[0].version, turn.version);
    assert!(history.iter().any(|c| c.source == Source::Suspend));

    // Who did what.
    let records = storage
        .audit()
        .unwrap()
        .query(&AuditQuery::new().thread("expense-1"))
        .await
        .unwrap();
    let summary: Vec<String> = records
        .iter()
        .map(|r| {
            format!(
                "{} {} {}",
                r.event.action,
                r.event.actor.as_deref().unwrap_or("-"),
                r.event.outcome.as_deref().unwrap_or("-")
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            "run.finished user:ada suspended",
            "run.resumed user:boss answered",
            "run.finished user:boss completed",
            "run.finished user:ada suspended",
        ]
    );
}

#[tokio::test]
async fn a_failed_run_is_saved_and_retry_does_not_repeat_finished_nodes() {
    let storage = Storage::in_memory();
    let probe = Probe::default();
    let thread = storage.workflow_thread(&expenses(&probe), "expense-2");
    thread.run(input(50)).await.unwrap();
    probe.fail_pay.store(true, Ordering::SeqCst);
    let err = thread
        .resume([("approve", json!("yes"))])
        .await
        .unwrap_err();
    assert_eq!(err.code(), "node_failed");
    assert!(matches!(err, ThreadError::Run(ref e) if e.snapshot().is_some()));
    let head = thread.head().await.unwrap().unwrap();
    assert_eq!(head.status(), CheckpointStatus::Failed);

    probe.fail_pay.store(false, Ordering::SeqCst);
    let turn = thread.retry().await.unwrap();
    assert_eq!(turn.state().get("paid"), Some(&json!(true)));
    assert_eq!(probe.drafts.load(Ordering::SeqCst), 1);
    assert_eq!(probe.payments.load(Ordering::SeqCst), 1);
    let err = thread.retry().await.unwrap_err();
    assert_eq!(err.code(), "resume_invalid");

    // Rewind to before the decision: it waits again.
    let suspended = thread
        .history(20)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.source == Source::Suspend)
        .unwrap();
    thread.rewind(&suspended.id).await.unwrap();
    assert_eq!(thread.interrupts().await.unwrap().len(), 1);
    let turn = thread.resume([("approve", json!("no"))]).await.unwrap();
    assert_eq!(turn.state().get("paid"), Some(&json!(false)));
    assert!(thread.delete().await.unwrap());
    assert!(thread.head().await.unwrap().is_none());
}

#[tokio::test]
async fn cancel_stops_the_run_and_it_is_saved() {
    let slow = WorkflowBuilder::new()
        .add_node(
            "slow",
            node_with_context(|ctx, _| async move {
                ctx.cancel_token().cancelled().await;
                Ok(Update::new().into())
            }),
        )
        .set_entry("slow")
        .add_edge("slow", END)
        .build()
        .unwrap();
    let storage = Storage::in_memory();
    let thread = storage.workflow_thread(&slow, "slow-1");
    let running = {
        let thread = thread.clone();
        tokio::spawn(async move { thread.run(State::new()).await })
    };
    while !thread.cancel() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let err = running.await.unwrap().unwrap_err();
    assert_eq!(err.code(), "cancelled");
    let head = thread.head().await.unwrap().unwrap();
    assert_eq!(head.status(), CheckpointStatus::Failed);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_paused_workflow_survives_reopening_the_database() {
    let file = std::env::temp_dir().join(format!(
        "agentbuild-workflow-thread-{}.db",
        uuid::Uuid::now_v7()
    ));
    let url = format!("sqlite://{}", file.display());
    let probe = Probe::default();
    {
        let storage = Storage::connect(&url).await.unwrap();
        let turn = storage
            .workflow_thread(&expenses(&probe), "expense-3")
            .run(input(70))
            .await
            .unwrap();
        assert!(turn.is_suspended());
    }
    // A new process: new storage, new workflow value.
    let storage = Storage::connect(&url).await.unwrap();
    let thread = storage.workflow_thread(&expenses(&probe), "expense-3");
    assert_eq!(
        thread.interrupts().await.unwrap()[0].payload,
        json!({"approve": 70})
    );
    let turn = thread.resume([("approve", json!("yes"))]).await.unwrap();
    assert_eq!(turn.state().get("paid"), Some(&json!(true)));
    assert_eq!(probe.drafts.load(Ordering::SeqCst), 1);
    drop(thread);
    drop(storage);
    for suffix in ["", "-wal", "-shm"] {
        let mut path = file.clone().into_os_string();
        path.push(suffix);
        let _ = std::fs::remove_file(path);
    }
}
