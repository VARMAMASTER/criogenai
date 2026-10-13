//! The SQLite backend (design 0008 §12): the shared contract, plus what only
//! a database can show: data survives reopening, deltas, migrations.
#![cfg(feature = "sqlite")]

#[path = "support/contract.rs"]
mod contract;

use std::path::PathBuf;

use agentbuild_core::State;
use agentbuild_storage::{Checkpoint, Checkpointer, Source, SqliteCheckpointer, Storage};
use contract::state;
use serde_json::json;

/// A database file in the system temp directory, removed on drop.
struct TempDb(PathBuf);

impl TempDb {
    fn new() -> Self {
        let name = format!("agentbuild-test-{}.db", uuid::Uuid::now_v7());
        Self(std::env::temp_dir().join(name))
    }

    async fn open(&self) -> SqliteCheckpointer {
        SqliteCheckpointer::open(&self.0).await.unwrap()
    }

    fn url(&self) -> String {
        format!("sqlite://{}", self.0.display())
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut path = self.0.clone().into_os_string();
            path.push(suffix);
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Raw access to the file, to look at how rows are stored.
async fn raw(db: &TempDb) -> sqlx::SqlitePool {
    sqlx::SqlitePool::connect(&db.url()).await.unwrap()
}

#[tokio::test]
async fn the_sqlite_backend_keeps_the_contract() {
    let db = TempDb::new();
    contract::all(&db.open().await).await;
    // Also in memory.
    contract::all(
        &SqliteCheckpointer::connect("sqlite::memory:")
            .await
            .unwrap(),
    )
    .await;
}

#[tokio::test]
async fn checkpoints_survive_reopening_the_file() {
    let db = TempDb::new();
    let first = Checkpoint::new("t", 1, Source::Step, state(1));
    let second = Checkpoint::new("t", 2, Source::Suspend, state(2))
        .parent(&first.id)
        .run("r", 4)
        .snapshot(contract::snapshot(
            json!({"state": {"n": 2}, "suspended": [{"node": "review", "payload": {}}]}),
        ));
    {
        let store = db.open().await;
        store.commit(first.clone(), 0).await.unwrap();
        store.commit(second.clone(), 1).await.unwrap();
        store.close().await;
    }
    // A new process: nothing cached.
    let store = db.open().await;
    assert_eq!(store.load("t", None).await.unwrap().unwrap(), second);
    assert_eq!(
        store.load("t", Some(&first.id)).await.unwrap().unwrap(),
        first
    );
    assert_eq!(store.history("t", 5).await.unwrap().len(), 2);
    // And through `Storage::connect`.
    let storage = Storage::connect(&db.url()).await.unwrap();
    let head = storage.checkpointer().load("t", None).await.unwrap();
    assert_eq!(head.unwrap().id, second.id);
}

#[tokio::test]
async fn a_long_chat_stores_each_message_once() {
    let db = TempDb::new();
    let store = db.open().await.keyframe_interval(10);
    let mut messages = Vec::new();
    let mut parent: Option<String> = None;
    let mut last = None;
    for version in 1..=40u64 {
        let text = format!("message number {version}: {}", "lorem ipsum ".repeat(20));
        messages.push(json!({"role": "user", "text": text}));
        let state = State::new().with("messages", messages.clone());
        let mut checkpoint = Checkpoint::new("chat", version, Source::Step, state);
        if let Some(parent) = &parent {
            checkpoint = checkpoint.parent(parent);
        }
        parent = Some(checkpoint.id.clone());
        store.commit(checkpoint.clone(), version - 1).await.unwrap();
        last = Some(checkpoint);
    }

    let pool = raw(&db).await;
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT kind, depth, length(body) FROM agentbuild_checkpoints
            WHERE thread_id = 'chat' ORDER BY version",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let kinds: String = rows
        .iter()
        .map(|(kind, _, _)| if kind == "keyframe" { 'K' } else { 'd' })
        .collect();
    // A keyframe every 10 checkpoints, deltas in between.
    assert_eq!(kinds, "Kddddddddd".repeat(4));
    assert_eq!(rows[9].1, 9);
    // A delta holds one message (the first keyframe's size), not the
    // whole list.
    let one_message = rows[0].2;
    assert!(rows[38].2 < one_message * 2, "{rows:?}");
    // In total: 4 keyframes plus one message per delta, far below storing
    // the full list every time (~ n²/2 messages).
    let stored: i64 = rows.iter().map(|r| r.2).sum();
    let full_every_time: i64 = (1..=40).map(|n| n * 260).sum();
    assert!(
        stored * 3 < full_every_time,
        "{stored} vs {full_every_time}"
    );

    // Reopened, the last checkpoint is rebuilt from its keyframe and deltas.
    drop(store);
    let store = db.open().await;
    let last = last.unwrap();
    assert_eq!(store.load("chat", None).await.unwrap().unwrap(), last);
}

#[tokio::test]
async fn migrations_run_once_and_refuse_newer_databases() {
    let db = TempDb::new();
    let store = db.open().await;
    assert_eq!(store.schema_version().await.unwrap(), 1);
    drop(store);
    // Opening again applies nothing new.
    let store = db.open().await;
    assert_eq!(store.schema_version().await.unwrap(), 1);
    drop(store);

    let pool = raw(&db).await;
    sqlx::query("INSERT INTO agentbuild_schema_migrations VALUES (99, 0)")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let err = SqliteCheckpointer::open(&db.0).await.unwrap_err();
    assert_eq!(err.code(), "storage_incompatible");
    assert!(err.message.contains("schema version 99"), "{}", err.message);
}

#[tokio::test]
async fn bad_urls_and_closed_databases_are_reported() {
    let err = Storage::connect("mysql://x").await.unwrap_err();
    assert_eq!(err.code(), "storage_unavailable");
    let missing_dir = std::env::temp_dir()
        .join("agentbuild-no-such-dir")
        .join("x.db");
    let err = SqliteCheckpointer::open(missing_dir).await.unwrap_err();
    assert_eq!(err.code(), "storage_unavailable");

    let store = SqliteCheckpointer::connect("sqlite::memory:")
        .await
        .unwrap();
    store.close().await;
    let err = store.load("t", None).await.unwrap_err();
    assert_eq!(err.code(), "storage_unavailable");
}

#[tokio::test]
async fn two_handles_on_one_file_cannot_fork_a_thread() {
    let db = TempDb::new();
    let (a, b) = (db.open().await, db.open().await);
    let first = Checkpoint::new("t", 1, Source::Step, state(1));
    a.commit(first.clone(), 0).await.unwrap();
    // Both read version 1 and write version 2; one wins.
    let from_a = Checkpoint::new("t", 2, Source::Step, state(2)).parent(&first.id);
    let from_b = Checkpoint::new("t", 2, Source::Step, state(3)).parent(&first.id);
    let (ra, rb) = tokio::join!(a.commit(from_a, 1), b.commit(from_b, 1));
    assert!(ra.is_ok() != rb.is_ok(), "{ra:?} {rb:?}");
    let loser = ra.err().or(rb.err()).unwrap();
    assert_eq!(loser.code(), "storage_conflict");
    assert_eq!(
        a.load("t", None).await.unwrap(),
        b.load("t", None).await.unwrap()
    );
}

#[tokio::test]
async fn storage_can_be_opened_and_used_from_spawned_tasks() {
    let db = TempDb::new();
    let url = db.url();
    let storage = tokio::spawn(async move { Storage::connect(&url).await.unwrap() })
        .await
        .unwrap();
    let path = db.0.clone();
    let store = tokio::spawn(async move { SqliteCheckpointer::open(path).await.unwrap() })
        .await
        .unwrap();
    let first = Checkpoint::new("t", 1, Source::Step, state(1));
    let checkpointer = storage.checkpointer().clone();
    let commit = first.clone();
    tokio::spawn(async move { checkpointer.commit(commit, 0).await.unwrap() })
        .await
        .unwrap();
    assert_eq!(store.load("t", None).await.unwrap().unwrap(), first);
}
