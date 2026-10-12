//! The checkpointer contract (design 0008 §7, §11, §12). Every backend's
//! tests run these.

use agentbuild_core::{NodeOutput, PendingWrite, Snapshot, State, Update};
use agentbuild_storage::{Checkpoint, CheckpointStatus, Checkpointer, Source};
use serde_json::json;

pub fn state(n: u64) -> State {
    State::new().with("n", n)
}

pub fn snapshot(extra: serde_json::Value) -> Snapshot {
    let mut value = json!({
        "format": "agentbuild.snapshot/1",
        "workflow_fingerprint": "f",
        "run_id": "r",
        "step": 2,
        "state": {"n": 1},
        "frontier": ["tools"],
        "join_progress": {},
        "suspended": []
    });
    value
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(value).unwrap()
}

pub async fn all(store: &dyn Checkpointer) {
    commits_are_compare_and_set(store).await;
    history_follows_the_heads_branch(store).await;
    snapshots_and_task_writes_round_trip(store).await;
    long_threads_round_trip(store).await;
}

pub async fn commits_are_compare_and_set(store: &dyn Checkpointer) {
    assert!(store.load("cas", None).await.unwrap().is_none());

    let first = Checkpoint::new("cas", 1, Source::Step, state(1));
    store.commit(first.clone(), 0).await.unwrap();
    // Stale writer: still thinks the thread is new.
    let stale = Checkpoint::new("cas", 1, Source::Step, state(9));
    let err = store.commit(stale, 0).await.unwrap_err();
    assert_eq!(err.code(), "storage_conflict");
    assert!(err.message.contains("version 1, not 0"), "{}", err.message);
    // Right expectation, wrong version number.
    let skipping = Checkpoint::new("cas", 3, Source::Step, state(9));
    assert_eq!(
        store.commit(skipping, 1).await.unwrap_err().code(),
        "storage_conflict"
    );
    // Expecting a version the thread never reached.
    let ahead = Checkpoint::new("cas", 3, Source::Step, state(9));
    let err = store.commit(ahead, 2).await.unwrap_err();
    assert!(err.message.contains("version 1, not 2"), "{}", err.message);
    assert_eq!(store.load("cas", None).await.unwrap().unwrap(), first);

    let second = Checkpoint::new("cas", 2, Source::Step, state(2)).parent(&first.id);
    store.commit(second.clone(), 1).await.unwrap();
    assert_eq!(store.load("cas", None).await.unwrap().unwrap(), second);
    assert_eq!(
        store.load("cas", Some(&first.id)).await.unwrap().unwrap(),
        first
    );
    assert!(
        store
            .load("other", Some(&first.id))
            .await
            .unwrap()
            .is_none()
    );
    assert!(store.load("cas", Some("missing")).await.unwrap().is_none());
}

pub async fn history_follows_the_heads_branch(store: &dyn Checkpointer) {
    let a = Checkpoint::new("h", 1, Source::Step, state(1));
    let b = Checkpoint::new("h", 2, Source::Step, state(2)).parent(&a.id);
    let fork = Checkpoint::new("h", 3, Source::Fork, state(1)).parent(&a.id);
    for (checkpoint, expected) in [(&a, 0), (&b, 1), (&fork, 2)] {
        store.commit(checkpoint.clone(), expected).await.unwrap();
    }
    let history = store.history("h", 10).await.unwrap();
    let ids: Vec<&str> = history.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, [fork.id.as_str(), a.id.as_str()]);
    assert_eq!(history[0], fork.meta());
    assert_eq!(store.history("h", 1).await.unwrap().len(), 1);
    assert!(store.history("h", 0).await.unwrap().is_empty());
    assert!(store.history("nobody", 5).await.unwrap().is_empty());
    // The abandoned branch is still stored.
    assert_eq!(store.load("h", Some(&b.id)).await.unwrap().unwrap(), b);

    assert!(store.delete_thread("h").await.unwrap());
    assert!(store.load("h", Some(&b.id)).await.unwrap().is_none());
    assert!(store.load("h", None).await.unwrap().is_none());
    assert!(!store.delete_thread("h").await.unwrap());
    // A deleted thread starts again at version 1.
    let again = Checkpoint::new("h", 1, Source::Step, state(1));
    store.commit(again, 0).await.unwrap();
}

pub async fn snapshots_and_task_writes_round_trip(store: &dyn Checkpointer) {
    let running = Checkpoint::new("p", 1, Source::Input, state(1))
        .run("r", 2)
        .snapshot(snapshot(json!({})));
    assert_eq!(running.status(), CheckpointStatus::InProgress);
    store.commit(running.clone(), 0).await.unwrap();

    let mut memo = PendingWrite::default();
    memo.memo.insert("call:1".into(), json!({"ok": 1}));
    store
        .put_pending("p", &running.id, "tools", memo)
        .await
        .unwrap();
    let mut more = PendingWrite::default();
    more.memo.insert("call:2".into(), json!({"ok": 2}));
    more.memo.insert("call:1".into(), json!({"ok": "again"}));
    store
        .put_pending("p", &running.id, "tools", more)
        .await
        .unwrap();
    let mut finished = PendingWrite::default();
    finished.output = Some(NodeOutput::from(Update::new().set("done", true)));
    store
        .put_pending("p", &running.id, "tools", finished)
        .await
        .unwrap();
    let mut other = PendingWrite::default();
    other.resume = Some(json!(null));
    store
        .put_pending("p", &running.id, "review", other)
        .await
        .unwrap();

    let loaded = store.load("p", None).await.unwrap().unwrap();
    let writes = &loaded.snapshot.as_ref().unwrap().pending_writes;
    assert_eq!(writes["tools"].memo["call:1"], json!({"ok": "again"}));
    assert_eq!(writes["tools"].memo["call:2"], json!({"ok": 2}));
    assert_eq!(
        writes["tools"].output,
        Some(NodeOutput::from(Update::new().set("done", true)))
    );
    assert_eq!(writes["review"].resume, Some(json!(null)));
    assert_eq!(loaded.status(), CheckpointStatus::InProgress);
    assert_eq!(loaded.state, running.state);
    assert_eq!(loaded.snapshot.as_ref().unwrap().state, running.state);

    // Only checkpoints with a run take task writes.
    let done = Checkpoint::new("p", 2, Source::Step, state(2)).parent(&running.id);
    store.commit(done.clone(), 1).await.unwrap();
    for (thread, id) in [
        ("p", done.id.as_str()),
        ("p", "missing"),
        ("q", running.id.as_str()),
    ] {
        let err = store
            .put_pending(thread, id, "tools", PendingWrite::default())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "checkpoint_not_found");
    }

    // A failed and a suspended checkpoint keep their snapshots; a snapshot
    // whose state differs from the checkpoint's is kept as it is.
    let failed = Checkpoint::new("p", 3, Source::Failure, state(1))
        .parent(&done.id)
        .snapshot(snapshot(
            json!({"error": {"code": "node_failed", "message": "x"}}),
        ));
    store.commit(failed.clone(), 2).await.unwrap();
    assert_eq!(store.load("p", None).await.unwrap().unwrap(), failed);
    let suspended = Checkpoint::new("p", 4, Source::Suspend, state(7))
        .parent(&failed.id)
        .snapshot(snapshot(
            json!({"suspended": [{"node": "review", "payload": {"q": 1}}]}),
        ));
    store.commit(suspended.clone(), 3).await.unwrap();
    let loaded = store.load("p", None).await.unwrap().unwrap();
    assert_eq!(loaded, suspended);
    assert_eq!(loaded.status(), CheckpointStatus::Suspended);
    let statuses: Vec<CheckpointStatus> = store
        .history("p", 10)
        .await
        .unwrap()
        .iter()
        .map(|c| c.status)
        .collect();
    assert_eq!(
        statuses,
        [
            CheckpointStatus::Suspended,
            CheckpointStatus::Failed,
            CheckpointStatus::Completed,
            CheckpointStatus::InProgress
        ]
    );
}

/// A chat-like thread: a growing message list plus changing keys, over more
/// checkpoints than a keyframe interval. Every checkpoint loads back exactly.
pub async fn long_threads_round_trip(store: &dyn Checkpointer) {
    let mut checkpoints = Vec::new();
    let mut messages = Vec::new();
    let mut parent: Option<String> = None;
    for version in 1..=60u64 {
        messages.push(json!({"role": "user", "text": format!("message {version}")}));
        if version == 30 {
            // Rewritten, not appended.
            messages.truncate(3);
        }
        let mut state = State::new()
            .with("messages", messages.clone())
            .with("turn", version);
        if version % 7 == 0 {
            state.insert("note", format!("every seventh: {version}"));
        }
        let mut checkpoint = Checkpoint::new("long", version, Source::Step, state);
        if let Some(parent) = &parent {
            checkpoint = checkpoint.parent(parent);
        }
        parent = Some(checkpoint.id.clone());
        store.commit(checkpoint.clone(), version - 1).await.unwrap();
        checkpoints.push(checkpoint);
    }
    for checkpoint in &checkpoints {
        let loaded = store
            .load("long", Some(&checkpoint.id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&loaded, checkpoint, "version {}", checkpoint.version);
    }
    assert_eq!(store.history("long", 100).await.unwrap().len(), 60);
}
