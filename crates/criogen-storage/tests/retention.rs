//! Retention (design 0008 §15.1): `Storage::prune` and the background
//! pruner, on the in-memory backend and on SQLite.

use std::time::Duration;

use criogen_core::State;
use criogen_storage::{
    Checkpoint, Namespace, OnBusy, PruneReport, Retention, Source, Storage, ThreadQuery,
};
use serde_json::json;

/// Commits `n` checkpoints to `thread`, each a child of the last.
async fn chat(storage: &Storage, thread: &str, n: u64) {
    let store = storage.checkpointer();
    let head = store.load(thread, None).await.unwrap();
    let mut version = head.as_ref().map_or(0, |c| c.version);
    let mut parent = head.map(|c| c.id);
    for _ in 0..n {
        version += 1;
        let state = State::new().with("turn", version);
        let mut checkpoint = Checkpoint::new(thread, version, Source::Step, state);
        if let Some(parent) = &parent {
            checkpoint = checkpoint.parent(parent);
        }
        parent = Some(checkpoint.id.clone());
        store.commit(checkpoint, version - 1).await.unwrap();
    }
}

async fn threads(storage: &Storage) -> Vec<String> {
    storage
        .checkpointer()
        .threads(&ThreadQuery::new())
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.thread_id)
        .collect()
}

async fn history_len(storage: &Storage, thread: &str) -> usize {
    storage
        .checkpointer()
        .history(thread, 1000)
        .await
        .unwrap()
        .len()
}

/// The rules on any storage.
async fn rules(storage: Storage) {
    // Nothing set: nothing deleted.
    chat(&storage, "a", 3).await;
    chat(&storage, "b", 3).await;
    assert_eq!(storage.prune().await.unwrap(), PruneReport::default());
    assert_eq!(threads(&storage).await, ["a", "b"]);

    // A long time to live: everything is recent.
    let keep_a_day = storage
        .clone()
        .retention(Retention::new().thread_ttl(Duration::from_secs(86_400)));
    assert_eq!(keep_a_day.prune().await.unwrap().threads_deleted, 0);

    // TTL zero: every thread is idle, except one a run holds.
    let expire_all = storage
        .clone()
        .retention(Retention::new().thread_ttl(Duration::ZERO));
    let held = storage.lease("b").await.unwrap();
    let report = expire_all.prune().await.unwrap();
    assert_eq!(report.threads_deleted, 1);
    assert_eq!(report.threads_busy, 1);
    assert_eq!(threads(&storage).await, ["b"]);
    assert!(
        storage
            .checkpointer()
            .load("a", None)
            .await
            .unwrap()
            .is_none()
    );
    drop(held);
    assert_eq!(expire_all.prune().await.unwrap().threads_deleted, 1);
    assert!(threads(&storage).await.is_empty());

    // Shortening: the head and its ancestors, many threads (several pages).
    for i in 0..230 {
        chat(&storage, &format!("t{i:03}"), 4).await;
    }
    let short = storage
        .clone()
        .retention(Retention::new().keep_checkpoints(2));
    let report = short.prune().await.unwrap();
    assert_eq!(report.checkpoints_deleted, 230 * 2);
    assert_eq!(history_len(&storage, "t000").await, 2);
    assert_eq!(history_len(&storage, "t229").await, 2);
    assert_eq!(short.prune().await.unwrap().checkpoints_deleted, 0);
    let head = storage.checkpointer().load("t007", None).await.unwrap();
    assert_eq!(head.unwrap().state, State::new().with("turn", 4u64));

    // Abandoned branches only: a rewind leaves one.
    chat(&storage, "r", 3).await;
    let first = storage.checkpointer().history("r", 10).await.unwrap()[2].clone();
    let fork =
        Checkpoint::new("r", 4, Source::Fork, State::new().with("turn", 1u64)).parent(&first.id);
    storage.checkpointer().commit(fork, 3).await.unwrap();
    let branches = storage.clone().retention(Retention::new().drop_branches());
    assert_eq!(
        branches
            .checkpointer()
            .prune_thread("r", None)
            .await
            .unwrap(),
        2
    );
    assert_eq!(history_len(&storage, "r").await, 2);

    // Expired memory is removed and counted.
    let ns = Namespace::parse("users/1").unwrap();
    storage
        .memory()
        .put(&ns, "old", json!("x"), Some(Duration::ZERO))
        .await
        .unwrap();
    storage
        .memory()
        .put(&ns, "new", json!("y"), None)
        .await
        .unwrap();
    assert_eq!(storage.prune().await.unwrap().memory_items_deleted, 1);
    assert!(storage.memory().get(&ns, "new").await.unwrap().is_some());
}

#[tokio::test]
async fn retention_rules_apply_in_memory() {
    rules(Storage::in_memory()).await;
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn retention_rules_apply_on_sqlite() {
    rules(Storage::connect("sqlite::memory:").await.unwrap()).await;
}

#[test]
fn retention_settings() {
    let rules = Retention::new()
        .drop_branches()
        .keep_checkpoints(0)
        .thread_ttl(Duration::from_secs(5));
    assert_eq!(rules.ttl(), Some(Duration::from_secs(5)));
    // keep_checkpoints replaces drop_branches, not the other way round.
    assert_eq!(
        Retention::new().keep_checkpoints(3).drop_branches(),
        Retention::new().keep_checkpoints(3)
    );
    let storage = Storage::in_memory().retention(rules);
    assert_eq!(storage.retention_rules(), rules);
    assert!(format!("{storage:?}").contains("retention"));
}

#[tokio::test]
async fn the_pruner_runs_in_the_background_until_dropped() {
    let storage = Storage::in_memory().retention(Retention::new().keep_checkpoints(2));
    chat(&storage, "t", 5).await;
    let pruner = storage.spawn_pruner(Duration::from_millis(20));
    wait_for(|| async { history_len(&storage, "t").await == 2 }).await;
    let report = wait_for_report(&pruner).await;
    assert_eq!(report.unwrap().checkpoints_deleted, 3);

    // Later commits are pruned by later passes.
    chat(&storage, "t", 4).await;
    chat(&storage, "u", 3).await;
    wait_for(|| async {
        history_len(&storage, "t").await == 2 && history_len(&storage, "u").await == 2
    })
    .await;

    // Stopped: nothing more happens.
    pruner.stop();
    chat(&storage, "v", 5).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(history_len(&storage, "v").await, 5);
}

#[tokio::test]
async fn a_pruner_reports_errors_and_keeps_going() {
    // A busy thread with TTL zero is skipped, not an error.
    let storage = Storage::in_memory()
        .on_busy(OnBusy::Wait)
        .retention(Retention::new().thread_ttl(Duration::ZERO));
    chat(&storage, "held", 1).await;
    let lease = storage.lease("held").await.unwrap();
    let pruner = storage.spawn_pruner(Duration::from_millis(20));
    let report = wait_for_report(&pruner).await.unwrap();
    assert_eq!(report.threads_busy, 1);
    drop(lease);
    wait_for(|| async { threads(&storage).await.is_empty() }).await;
}

async fn wait_for_report(
    pruner: &criogen_storage::Pruner,
) -> Result<PruneReport, criogen_storage::StorageError> {
    for _ in 0..500 {
        if let Some(report) = pruner.last() {
            return report;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the pruner never finished a pass");
}

async fn wait_for<F, Fut>(mut done: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..500 {
        if done().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out");
}
