//! The checkpointer contract (design 0008 §11) on the in-memory backend,
//! and thread leases.

use std::time::Duration;

use agentbuild_core::{CancelToken, Snapshot, State};
use agentbuild_storage::{
    Checkpoint, CheckpointStatus, Checkpointer, InMemoryCheckpointer, OnBusy, Source, Storage,
};
use serde_json::json;

fn state(n: u64) -> State {
    State::new().with("n", n)
}

fn snapshot(extra: serde_json::Value) -> Snapshot {
    let mut value = json!({
        "format": "agentbuild.snapshot/1",
        "workflow_fingerprint": "f",
        "run_id": "r",
        "step": 2,
        "state": {},
        "frontier": [],
        "join_progress": {},
        "suspended": []
    });
    value
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(value).unwrap()
}

#[tokio::test]
async fn commits_are_compare_and_set() {
    let store = InMemoryCheckpointer::new();
    assert!(store.load("t", None).await.unwrap().is_none());

    let first = Checkpoint::new("t", 1, Source::Step, state(1));
    store.commit(first.clone(), 0).await.unwrap();
    // Stale writer: still thinks the thread is new.
    let stale = Checkpoint::new("t", 1, Source::Step, state(9));
    let err = store.commit(stale, 0).await.unwrap_err();
    assert_eq!(err.code(), "storage_conflict");
    assert!(err.message.contains("version 1, not 0"), "{}", err.message);
    // Right expectation, wrong version number.
    let skipping = Checkpoint::new("t", 3, Source::Step, state(9));
    assert_eq!(
        store.commit(skipping, 1).await.unwrap_err().code(),
        "storage_conflict"
    );
    assert_eq!(store.load("t", None).await.unwrap().unwrap(), first);

    let second = Checkpoint::new("t", 2, Source::Step, state(2)).parent(&first.id);
    store.commit(second.clone(), 1).await.unwrap();
    assert_eq!(store.load("t", None).await.unwrap().unwrap(), second);
    assert_eq!(
        store.load("t", Some(&first.id)).await.unwrap().unwrap(),
        first
    );
    assert!(
        store
            .load("other", Some(&first.id))
            .await
            .unwrap()
            .is_none()
    );
    assert!(store.load("t", Some("missing")).await.unwrap().is_none());
}

#[tokio::test]
async fn history_follows_the_heads_branch() {
    let store = InMemoryCheckpointer::new();
    let a = Checkpoint::new("t", 1, Source::Step, state(1));
    let b = Checkpoint::new("t", 2, Source::Step, state(2)).parent(&a.id);
    let fork = Checkpoint::new("t", 3, Source::Fork, state(1)).parent(&a.id);
    for (checkpoint, expected) in [(&a, 0), (&b, 1), (&fork, 2)] {
        store.commit(checkpoint.clone(), expected).await.unwrap();
    }
    let ids: Vec<String> = store
        .history("t", 10)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(ids, [fork.id.clone(), a.id.clone()]);
    assert_eq!(store.history("t", 1).await.unwrap().len(), 1);
    assert!(store.history("t", 0).await.unwrap().is_empty());
    assert!(store.history("nobody", 5).await.unwrap().is_empty());
    // The abandoned branch is still stored.
    assert_eq!(store.load("t", Some(&b.id)).await.unwrap().unwrap(), b);

    assert!(store.delete_thread("t").await.unwrap());
    assert!(store.load("t", Some(&b.id)).await.unwrap().is_none());
    assert!(!store.delete_thread("t").await.unwrap());
    // A deleted thread starts again at version 1.
    let again = Checkpoint::new("t", 1, Source::Step, state(1));
    store.commit(again, 0).await.unwrap();
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
