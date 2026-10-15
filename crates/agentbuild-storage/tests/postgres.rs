//! The Postgres backend (design 0008 §14): the shared contract, the same
//! stored format as SQLite, migrations, and thread locks across processes.
//! Two `Storage` values with their own connections stand for two
//! processes: to the server they are.
//!
//! Needs a server: `bash scripts/with-postgres.sh cargo test …` (skips
//! without one).
#![cfg(feature = "postgres")]

#[path = "support/contract.rs"]
mod contract;
#[path = "support/postgres.rs"]
mod postgres;

use std::sync::Arc;
use std::time::Duration;

use agentbuild_core::{CancelToken, State};
use agentbuild_storage::{
    Checkpoint, Checkpointer, OnBusy, PostgresCheckpointer, PostgresOptions, Source, Storage,
};
use contract::state;
use postgres::TestDb;
use serde_json::json;
use tokio::time::Instant;

#[tokio::test]
async fn the_postgres_backend_keeps_the_contract() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let store = PostgresCheckpointer::connect(&db.url).await.unwrap();
    contract::all(&store).await;
    store.close().await;
    db.remove().await;
}

#[tokio::test]
async fn checkpoints_survive_reconnecting() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let first = Checkpoint::new("t", 1, Source::Step, state(1));
    let second = Checkpoint::new("t", 2, Source::Suspend, state(2))
        .parent(&first.id)
        .run("r", 4)
        .snapshot(contract::snapshot(
            json!({"state": {"n": 2}, "suspended": [{"node": "review", "payload": {}}]}),
        ));
    {
        let store = PostgresCheckpointer::connect(&db.url).await.unwrap();
        store.commit(first.clone(), 0).await.unwrap();
        store.commit(second.clone(), 1).await.unwrap();
        store.close().await;
    }
    // Another process: nothing cached.
    let store = PostgresCheckpointer::connect(&db.url).await.unwrap();
    assert_eq!(store.load("t", None).await.unwrap().unwrap(), second);
    assert_eq!(
        store.load("t", Some(&first.id)).await.unwrap().unwrap(),
        first
    );
    assert_eq!(store.history("t", 5).await.unwrap().len(), 2);
    store.close().await;
    // And through `Storage::connect`, with either scheme.
    for url in [
        db.url.clone(),
        db.url.replacen("postgres://", "postgresql://", 1),
    ] {
        let storage = Storage::connect(&url).await.unwrap();
        let head = storage.checkpointer().load("t", None).await.unwrap();
        assert_eq!(head.unwrap().id, second.id);
    }
    db.remove().await;
}

#[tokio::test]
async fn rows_are_stored_like_sqlite_deltas_and_keyframes() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let store = PostgresCheckpointer::connect(&db.url)
        .await
        .unwrap()
        .keyframe_interval(10);
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
    let pool = db.raw().await;
    let rows: Vec<(String, i64, i32)> = sqlx::query_as(
        "SELECT kind, depth, octet_length(body) FROM agentbuild_checkpoints
            WHERE thread_id = 'chat' ORDER BY version",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let kinds: String = rows
        .iter()
        .map(|(kind, _, _)| if kind == "keyframe" { 'K' } else { 'd' })
        .collect();
    assert_eq!(kinds, "Kddddddddd".repeat(4));
    assert!(rows[38].2 < rows[0].2 * 2, "{rows:?}");
    pool.close().await;
    store.close().await;

    let store = PostgresCheckpointer::connect(&db.url).await.unwrap();
    assert_eq!(
        store.load("chat", None).await.unwrap().unwrap(),
        last.unwrap()
    );
    store.close().await;
    db.remove().await;
}

#[tokio::test]
async fn migrations_run_once_even_when_processes_start_together() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    // Four processes open a new database at once: one migrates.
    let opened = futures_join_all(
        (0..4)
            .map(|_| {
                let url = db.url.clone();
                async move { PostgresCheckpointer::connect(&url).await }
            })
            .collect(),
    )
    .await;
    for store in &opened {
        let store = store.as_ref().unwrap();
        assert_eq!(store.schema_version().await.unwrap(), 2);
    }
    for store in opened {
        store.unwrap().close().await;
    }
    let pool = db.raw().await;
    let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agentbuild_schema_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(applied, 2);
    sqlx::query("INSERT INTO agentbuild_schema_migrations VALUES (99, 0)")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let err = PostgresCheckpointer::connect(&db.url).await.unwrap_err();
    assert_eq!(err.code(), "storage_incompatible");
    assert!(err.message.contains("schema version 99"), "{}", err.message);
    db.remove().await;
}

/// Awaits every future (no `futures` dependency for one helper).
async fn futures_join_all<F: std::future::Future + Send + 'static>(
    futures: Vec<F>,
) -> Vec<F::Output>
where
    F::Output: Send,
{
    let tasks: Vec<_> = futures.into_iter().map(tokio::spawn).collect();
    let mut out = Vec::new();
    for task in tasks {
        out.push(task.await.unwrap());
    }
    out
}

#[tokio::test]
async fn bad_urls_and_unreachable_servers_never_echo_the_password() {
    let err = PostgresCheckpointer::connect("postgres://u:hunter2@[bad")
        .await
        .unwrap_err();
    assert_eq!(err.code(), "storage_unavailable");
    assert!(!err.message.contains("hunter2"), "{}", err.message);

    // Nothing listens on port 1.
    let options = PostgresOptions::new().acquire_timeout(Duration::from_secs(3));
    let err = PostgresCheckpointer::connect_with("postgres://u:hunter2@127.0.0.1:1/x", options)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "storage_unavailable");
    assert!(!err.message.contains("hunter2"), "{}", err.message);
    let err = Storage::connect("mysql://u:hunter2@host/db")
        .await
        .unwrap_err();
    assert!(!err.message.contains("hunter2"), "{}", err.message);

    let Some(db) = TestDb::create().await else {
        return;
    };
    let store = PostgresCheckpointer::connect(&db.url).await.unwrap();
    // Text Postgres cannot hold (a NUL) is the caller's value, not an outage.
    let odd = Checkpoint::new("nul\0thread", 1, Source::Step, state(1));
    assert_eq!(
        store.commit(odd, 0).await.unwrap_err().code(),
        "invalid_value"
    );
    store.close().await;
    let err = store.load("t", None).await.unwrap_err();
    assert_eq!(err.code(), "storage_unavailable");
    db.remove().await;
}

#[tokio::test]
async fn tls_connections_work_and_verify_the_server() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let Ok(ca) = std::env::var("AGENTBUILD_TEST_POSTGRES_CA") else {
        eprintln!("skipped: the test server has no TLS (needs openssl)");
        db.remove().await;
        return;
    };
    for options in [
        "sslmode=require".to_string(),
        format!("sslmode=verify-full&sslrootcert={ca}"),
    ] {
        let url = format!("{}?{options}", db.url);
        let store = PostgresCheckpointer::connect(&url).await.unwrap();
        store
            .commit(Checkpoint::new("t", 1, Source::Step, state(1)), 0)
            .await
            .unwrap();
        assert!(store.delete_thread("t").await.unwrap());
        store.close().await;
        // The connection really is encrypted.
        let pool = sqlx::PgPool::connect(&url).await.unwrap();
        let encrypted: bool =
            sqlx::query_scalar("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(encrypted, "{options}");
        pool.close().await;
    }
    // A server whose certificate is not trusted is refused.
    let options = PostgresOptions::new().acquire_timeout(Duration::from_secs(3));
    let err =
        PostgresCheckpointer::connect_with(&format!("{}?sslmode=verify-full", db.url), options)
            .await
            .unwrap_err();
    assert_eq!(err.code(), "storage_unavailable");
    db.remove().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_writers_in_many_processes_never_fork_or_lose_a_commit() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let handles = [
        Arc::new(PostgresCheckpointer::connect(&db.url).await.unwrap()),
        Arc::new(PostgresCheckpointer::connect(&db.url).await.unwrap()),
        Arc::new(PostgresCheckpointer::connect(&db.url).await.unwrap()),
    ];
    let mut tasks = Vec::new();
    for writer in 0..9u64 {
        let store = handles[(writer % 3) as usize].clone();
        tasks.push(tokio::spawn(async move {
            let mut done = 0;
            while done < 10 {
                let head = store.load("shared", None).await.unwrap();
                let (expected, parent) = head.map_or((0, None), |h| (h.version, Some(h.id)));
                let mut checkpoint = Checkpoint::new(
                    "shared",
                    expected + 1,
                    Source::Step,
                    State::new().with("writer", writer).with("n", done),
                );
                if let Some(parent) = parent {
                    checkpoint = checkpoint.parent(parent);
                }
                match store.commit(checkpoint, expected).await {
                    Ok(()) => done += 1,
                    Err(err) if err.code() == "storage_conflict" => {}
                    Err(err) => panic!("writer {writer}: {err}"),
                }
            }
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let history = handles[0].history("shared", 1000).await.unwrap();
    let versions: Vec<u64> = history.iter().rev().map(|c| c.version).collect();
    assert_eq!(versions, (1..=90).collect::<Vec<_>>());
    for pair in history.windows(2) {
        assert_eq!(pair[0].parent_id.as_deref(), Some(pair[1].id.as_str()));
    }
    for handle in handles {
        handle.close().await;
    }
    db.remove().await;
}

#[tokio::test]
async fn thread_locks_hold_across_processes() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    // Two processes.
    let a = Storage::connect(&db.url).await.unwrap();
    let b = Storage::connect(&db.url)
        .await
        .unwrap()
        .busy_timeout(Duration::from_millis(700));
    assert!(format!("{b:?}").contains("across_processes: true"));

    let held = a.lease("t").await.unwrap();
    // Other threads are independent.
    drop(b.lease("other").await.unwrap());

    // Reject: at once.
    let started = Instant::now();
    let err = b
        .clone()
        .on_busy(OnBusy::Reject)
        .lease("t")
        .await
        .unwrap_err();
    assert_eq!(err.code(), "thread_busy");
    assert!(err.message.contains("another process"), "{}", err.message);
    assert!(started.elapsed() < Duration::from_millis(500));

    // Wait: gives up after busy_timeout.
    let started = Instant::now();
    let err = b.lease("t").await.unwrap_err();
    assert_eq!(err.code(), "thread_busy");
    assert!(err.message.contains("700 ms"), "{}", err.message);
    assert!(started.elapsed() >= Duration::from_millis(650));

    // Wait: gets the thread as soon as the other process lets go.
    let waiting = {
        let b = b.clone().busy_timeout(Duration::from_secs(20));
        tokio::spawn(async move { b.lease("t").await.map(drop) })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!waiting.is_finished());
    let released = Instant::now();
    drop(held);
    waiting.await.unwrap().unwrap();
    assert!(released.elapsed() < Duration::from_secs(5));

    // A waiter that gives up early leaves no lock behind.
    let held = a.lease("t").await.unwrap();
    let gave_up = {
        let b = b.clone().busy_timeout(Duration::from_secs(20));
        tokio::spawn(async move { b.lease("t").await.map(drop) })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    gave_up.abort();
    drop(held);
    let fast = b.clone().busy_timeout(Duration::from_secs(5));
    drop(fast.lease("t").await.unwrap());
    drop(a.lease("t").await.unwrap());
    db.remove().await;
}

#[tokio::test]
async fn interrupt_cancels_the_run_in_another_process() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let a = Storage::connect(&db.url).await.unwrap();
    let b = Storage::connect(&db.url)
        .await
        .unwrap()
        .on_busy(OnBusy::Interrupt)
        .busy_timeout(Duration::from_secs(20));
    let held = a.lease("t").await.unwrap();
    let token = CancelToken::new();
    held.set_cancel(token.clone());
    // The run in process A stops when cancelled, releasing the thread.
    let run = {
        let token = token.clone();
        tokio::spawn(async move {
            token.cancelled().await;
            drop(held);
        })
    };
    let started = Instant::now();
    drop(b.lease("t").await.unwrap());
    assert!(token.is_cancelled());
    assert!(started.elapsed() < Duration::from_secs(5));
    run.await.unwrap();
    // A run that is not interrupted is left alone.
    let held = a.lease("u").await.unwrap();
    let other = CancelToken::new();
    held.set_cancel(other.clone());
    drop(b.lease("v").await.unwrap());
    assert!(!other.is_cancelled());
    drop(held);
    db.remove().await;
}

#[tokio::test]
async fn a_lost_connection_frees_the_thread() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let a = Storage::connect(&db.url).await.unwrap();
    let b = Storage::connect(&db.url)
        .await
        .unwrap()
        .busy_timeout(Duration::from_secs(20));
    let held = a.lease("t").await.unwrap();
    let waiting = tokio::spawn(async move { b.lease("t").await.map(drop) });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!waiting.is_finished());
    // As if process A crashed: the server drops its lock connection.
    let pool = db.raw().await;
    let killed: Vec<bool> = sqlx::query_scalar(
        "SELECT pg_terminate_backend(pid) FROM pg_locks
            WHERE locktype = 'advisory' AND granted AND database =
                (SELECT oid FROM pg_database WHERE datname = current_database())",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(killed, [true]);
    waiting.await.unwrap().unwrap();
    drop(held);
    pool.close().await;
    db.remove().await;
}
