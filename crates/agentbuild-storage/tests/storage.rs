//! The checkpointer contract (design 0008 §11) on the in-memory backend,
//! and thread leases.

#[path = "support/contract.rs"]
mod contract;

use std::sync::Arc;
use std::time::Duration;

use agentbuild_core::{CancelToken, PendingWrite, Persister, State};
use agentbuild_storage::{
    Checkpoint, CheckpointStatus, Checkpointer, InMemoryCheckpointer, OnBusy, Source, Storage,
    ThreadWriter,
};
use contract::{snapshot, state};
use serde_json::json;

#[tokio::test]
async fn the_in_memory_backend_keeps_the_contract() {
    contract::all(&InMemoryCheckpointer::new()).await;
}

#[tokio::test]
async fn a_thread_writer_chains_checkpoints_and_task_writes() {
    let store: Arc<dyn Checkpointer> = Arc::new(InMemoryCheckpointer::new());
    let first = Checkpoint::new("w", 1, Source::Step, state(1));
    store.commit(first.clone(), 0).await.unwrap();

    let writer = ThreadWriter::new(store.clone(), "w", Some(&first));
    assert_eq!(
        (writer.head_id(), writer.version()),
        (Some(first.id.clone()), 1)
    );
    // Nothing to attach task writes to before the run's first checkpoint.
    let early = writer
        .save_task(0, "tools".into(), PendingWrite::default())
        .await
        .unwrap_err();
    assert_eq!(early.kind.as_deref(), Some("checkpoint_not_found"));

    writer
        .save_checkpoint(snapshot(json!({"step": 0})))
        .await
        .unwrap();
    let mut write = PendingWrite::default();
    write.memo.insert("call:1".into(), json!("done"));
    writer.save_task(0, "tools".into(), write).await.unwrap();
    writer
        .save_checkpoint(snapshot(json!({"step": 1})))
        .await
        .unwrap();
    let done = writer
        .commit(Source::Step, state(5), ("r", 2), None)
        .await
        .unwrap();
    assert_eq!(
        (done.version, done.status),
        (4, CheckpointStatus::Completed)
    );

    let history = store.history("w", 10).await.unwrap();
    let sources: Vec<Source> = history.iter().map(|c| c.source).collect();
    assert_eq!(
        sources,
        [Source::Step, Source::Step, Source::Input, Source::Step]
    );
    let input = store
        .load("w", Some(&history[2].id))
        .await
        .unwrap()
        .unwrap();
    let pending = &input.snapshot.unwrap().pending_writes["tools"];
    assert_eq!(pending.memo["call:1"], json!("done"));
    assert_eq!(history[2].parent_id.as_deref(), Some(first.id.as_str()));

    // Someone else moved the thread: the next write loses.
    let other = Checkpoint::new("w", 5, Source::Manual, State::new()).parent(&done.id);
    store.commit(other, 4).await.unwrap();
    let err = writer
        .save_checkpoint(snapshot(json!({})))
        .await
        .unwrap_err();
    assert_eq!(err.kind.as_deref(), Some("storage_conflict"));
}

#[test]
fn status_and_json_shape() {
    let completed = Checkpoint::new("t", 1, Source::Step, state(1)).run("r", 3);
    assert_eq!(completed.status(), CheckpointStatus::Completed);
    let suspended = Checkpoint::new("t", 1, Source::Suspend, state(1)).snapshot(snapshot(
        json!({"suspended": [{"node": "review", "payload": {}}]}),
    ));
    assert_eq!(suspended.status(), CheckpointStatus::Suspended);
    let failed = Checkpoint::new("t", 1, Source::Failure, state(1)).snapshot(snapshot(
        json!({"error": {"code": "node_failed", "message": "x"}}),
    ));
    assert_eq!(failed.status(), CheckpointStatus::Failed);

    let value = serde_json::to_value(&completed).unwrap();
    assert_eq!(value["source"], "step");
    assert_eq!(value["version"], 1);
    assert_eq!(value["run_id"], "r");
    assert_eq!(value["state"], json!({"n": 1}));
    assert!(value.get("snapshot").is_none() && value.get("parent_id").is_none());
    let back: Checkpoint = serde_json::from_value(value).unwrap();
    assert_eq!(back, completed);
    // UUIDv7 ids sort by creation time.
    let later = Checkpoint::new("t", 2, Source::Step, state(2));
    assert!(later.id > completed.id);

    let meta = serde_json::to_value(failed.meta()).unwrap();
    assert_eq!(meta["status"], "failed");
    assert_eq!(meta["source"], "failure");
    assert!(meta.get("state").is_none());
}

#[tokio::test(start_paused = true)]
async fn leases_wait_reject_time_out_and_interrupt() {
    let storage = Storage::in_memory().busy_timeout(Duration::from_secs(5));
    let held = storage.lease("t").await.unwrap();
    assert_eq!(held.thread(), "t");
    // Other threads are independent.
    drop(storage.lease("u").await.unwrap());

    // Wait: gets the lease once it is released.
    let waiting = {
        let storage = storage.clone();
        tokio::spawn(async move { storage.lease("t").await.map(|_| ()) })
    };
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!waiting.is_finished());
    drop(held);
    waiting.await.unwrap().unwrap();

    // Wait: gives up after busy_timeout.
    let held = storage.lease("t").await.unwrap();
    let err = storage.lease("t").await.unwrap_err();
    assert_eq!(err.code(), "thread_busy");
    assert!(err.message.contains("5000 ms"), "{}", err.message);

    // Reject: fails at once.
    let rejecting = storage.clone().on_busy(OnBusy::Reject);
    let started = tokio::time::Instant::now();
    assert_eq!(
        rejecting.lease("t").await.unwrap_err().code(),
        "thread_busy"
    );
    assert_eq!(started.elapsed(), Duration::ZERO);

    // Interrupt: cancels the active run, then takes over when it ends.
    let token = CancelToken::new();
    held.set_cancel(token.clone());
    let holder = tokio::spawn(async move {
        token.cancelled().await;
        drop(held);
    });
    let interrupting = storage.clone().on_busy(OnBusy::Interrupt);
    interrupting.lease("t").await.unwrap();
    holder.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancel_reaches_the_registered_run_only_while_it_holds_the_lease() {
    let storage = Storage::in_memory();
    let token = CancelToken::new();
    let lease = storage.lease("t").await.unwrap();
    assert!(!storage.cancel("t"), "no run registered yet");
    lease.set_cancel(token.clone());
    assert!(!storage.cancel("other"));
    assert!(storage.cancel("t"));
    assert!(token.is_cancelled());

    drop(lease);
    let next = CancelToken::new();
    let lease = storage.lease("t").await.unwrap();
    lease.set_cancel(next.clone());
    drop(lease);
    assert!(!storage.cancel("t"), "the run's lease was released");
    assert!(!next.is_cancelled());
}

#[tokio::test(start_paused = true)]
async fn a_finished_runs_token_is_not_cancelled_for_the_next_holder() {
    let storage = Storage::in_memory();
    let old = CancelToken::new();
    let lease = storage.lease("t").await.unwrap();
    lease.set_cancel(old.clone());
    let waiter = {
        let storage = storage.clone();
        tokio::spawn(async move { storage.lease("t").await.unwrap() })
    };
    tokio::task::yield_now().await;
    drop(lease);
    // The waiter holds the thread now but has not registered a run yet.
    let next = waiter.await.unwrap();
    assert!(!storage.cancel("t"));
    assert!(!old.is_cancelled());
    drop(next);
}

#[tokio::test(start_paused = true)]
async fn an_interrupt_before_the_run_registers_still_stops_it() {
    let storage = Storage::in_memory().busy_timeout(Duration::from_secs(5));
    // The holder has the lease but has not registered its run yet.
    let held = storage.lease("t").await.unwrap();
    let interrupting = {
        let storage = storage.clone().on_busy(OnBusy::Interrupt);
        tokio::spawn(async move { storage.lease("t").await.map(drop) })
    };
    tokio::task::yield_now().await;
    let token = CancelToken::new();
    held.set_cancel(token.clone());
    assert!(token.is_cancelled(), "the interrupt was remembered");
    drop(held);
    interrupting.await.unwrap().unwrap();

    // The next lease starts clean.
    let next = storage.lease("t").await.unwrap();
    let fresh = CancelToken::new();
    next.set_cancel(fresh.clone());
    assert!(!fresh.is_cancelled());
}
