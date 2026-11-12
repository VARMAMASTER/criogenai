//! The SQLite backend (design 0008 §12): the shared contract, plus what only
//! a database can show: data survives reopening, deltas, migrations.
#![cfg(feature = "sqlite")]

#[path = "support/contract.rs"]
mod contract;

use std::path::PathBuf;

use contract::state;
use criogenai_core::State;
use criogenai_storage::{Checkpoint, Checkpointer, Source, SqliteCheckpointer, Storage};
use serde_json::json;

/// A database file in the system temp directory, removed on drop.
struct TempDb(PathBuf);

impl TempDb {
    fn new() -> Self {
        let name = format!("criogenai-test-{}.db", uuid::Uuid::now_v7());
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
        "SELECT kind, depth, length(body) FROM criogen_checkpoints
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
    assert_eq!(store.schema_version().await.unwrap(), 6);
    drop(store);
    // Opening again applies nothing new.
    let store = db.open().await;
    assert_eq!(store.schema_version().await.unwrap(), 6);
    drop(store);

    let pool = raw(&db).await;
    sqlx::query("INSERT INTO criogen_schema_migrations VALUES (99, 0)")
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
        .join("criogenai-no-such-dir")
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

/// A small deterministic generator (xorshift), so failures reproduce.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

#[tokio::test]
async fn random_histories_load_back_exactly() {
    let db = TempDb::new();
    let store = db.open().await.keyframe_interval(4);
    for seed in 1..=20u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let thread = format!("t{seed}");
        let mut state = serde_json::Map::new();
        let mut written: Vec<Checkpoint> = Vec::new();
        for version in 1..=30u64 {
            for _ in 0..1 + rng.below(3) {
                let key = format!("k{}", rng.below(4));
                match rng.below(5) {
                    0..=2 => {
                        let mut list = match state.remove(&key) {
                            Some(serde_json::Value::Array(list)) => list,
                            _ => Vec::new(),
                        };
                        list.push(json!({"n": rng.below(100), "text": "x".repeat(rng.below(40) as usize)}));
                        state.insert(key, serde_json::Value::Array(list));
                    }
                    3 => {
                        state.insert(key, json!(rng.below(10)));
                    }
                    _ => {
                        state.remove(&key);
                    }
                }
            }
            // Branch now and then: the parent is an older checkpoint.
            let parent = match written.len() {
                0 => None,
                n if rng.below(6) == 0 => Some(written[rng.below(n as u64) as usize].id.clone()),
                n => Some(written[n - 1].id.clone()),
            };
            let mut checkpoint =
                Checkpoint::new(&thread, version, Source::Step, State::from(state.clone()));
            if let Some(parent) = parent {
                checkpoint = checkpoint.parent(parent);
            }
            store.commit(checkpoint.clone(), version - 1).await.unwrap();
            written.push(checkpoint);
        }
        for checkpoint in &written {
            let loaded = store.load(&thread, Some(&checkpoint.id)).await.unwrap();
            assert_eq!(
                loaded.as_ref(),
                Some(checkpoint),
                "seed {seed} v{}",
                checkpoint.version
            );
        }
    }
    // A fresh handle (no cache) reads the same.
    drop(store);
    let store = db.open().await;
    let head = store.load("t7", None).await.unwrap().unwrap();
    assert_eq!(head.version, 30);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_writers_on_one_file_never_fork_or_lose_a_commit() {
    let db = TempDb::new();
    // Two handles: as two processes would have.
    let handles = [
        std::sync::Arc::new(db.open().await),
        std::sync::Arc::new(db.open().await),
    ];
    let mut tasks = Vec::new();
    for writer in 0..8u64 {
        let store = handles[(writer % 2) as usize].clone();
        tasks.push(tokio::spawn(async move {
            let mut done = 0;
            let mut conflicts = 0;
            while done < 15 {
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
                    Err(err) if err.code() == "storage_conflict" => conflicts += 1,
                    Err(err) => panic!("writer {writer}: {err}"),
                }
            }
            conflicts
        }));
    }
    let mut conflicts = 0;
    for task in tasks {
        conflicts += task.await.unwrap();
    }
    // Every commit landed exactly once, on one linear branch.
    let history = handles[0].history("shared", 1000).await.unwrap();
    assert_eq!(history.len(), 8 * 15);
    let versions: Vec<u64> = history.iter().rev().map(|c| c.version).collect();
    assert_eq!(versions, (1..=120).collect::<Vec<_>>());
    for pair in history.windows(2) {
        assert_eq!(pair[0].parent_id.as_deref(), Some(pair[1].id.as_str()));
    }
    // Each writer's 15 commits are all there, in order.
    let mut seen = std::collections::BTreeMap::<u64, Vec<u64>>::new();
    for meta in history.iter().rev() {
        let checkpoint = handles[1]
            .load("shared", Some(&meta.id))
            .await
            .unwrap()
            .unwrap();
        let writer = checkpoint.state.get("writer").unwrap().as_u64().unwrap();
        let n = checkpoint.state.get("n").unwrap().as_u64().unwrap();
        seen.entry(writer).or_default().push(n);
    }
    for (writer, ns) in seen {
        assert_eq!(ns, (0..15).collect::<Vec<_>>(), "writer {writer}");
    }
    println!("{conflicts} lost compare-and-set races were retried");
}

/// Deletes `file` and its WAL files, retrying until `patience` runs out:
/// on Windows, other programs (the virus scanner) may hold a freshly
/// written file for a moment.
async fn remove_db(file: &std::path::Path, patience: std::time::Duration) -> std::io::Result<()> {
    let until = std::time::Instant::now() + patience;
    for suffix in ["", "-wal", "-shm"] {
        let mut path = file.to_path_buf().into_os_string();
        path.push(suffix);
        while std::path::Path::new(&path).exists() {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if std::time::Instant::now() >= until => return Err(e),
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn close_releases_the_database_file() {
    use criogenai_storage::{Namespace, Storage};
    use std::time::Duration;

    let open = |file: std::path::PathBuf| async move {
        let storage = Storage::connect(&format!("sqlite://{}", file.display()))
            .await
            .unwrap();
        let ns = Namespace::parse("users/1").unwrap();
        storage
            .memory()
            .put(&ns, "k", serde_json::json!(1), None)
            .await
            .unwrap();
        storage
    };
    let temp = |name: &str| {
        std::env::temp_dir().join(format!("criogenai-{name}-{}.db", uuid::Uuid::now_v7()))
    };

    let file = temp("close");
    let storage = open(file.clone()).await;
    storage.close().await;
    let ns = Namespace::parse("users/1").unwrap();
    let err = storage.memory().get(&ns, "k").await.unwrap_err();
    assert_eq!(err.code(), "storage_unavailable");
    remove_db(&file, Duration::from_secs(1)).await.unwrap();

    // Without close, the database keeps the file (on Windows it cannot be
    // deleted; elsewhere deleting an open file works, so only check there).
    if cfg!(windows) {
        let file = temp("open");
        let storage = open(file.clone()).await;
        assert!(remove_db(&file, Duration::from_millis(300)).await.is_err());
        storage.close().await;
        remove_db(&file, Duration::from_secs(1)).await.unwrap();
    }
    // In memory there is nothing to close.
    Storage::in_memory().close().await;
}

#[tokio::test]
async fn a_path_is_a_path_not_url_options() {
    // `#`, `%20` (and `?` where file names may hold it) in a file name must
    // not turn into connection options or be decoded.
    let odd = if cfg!(unix) { "a?b#c%20d" } else { "a#b%20c" };
    let path = std::env::temp_dir().join(format!("criogenai-{odd}-{}.db", uuid::Uuid::now_v7()));
    let store = SqliteCheckpointer::open(&path).await.unwrap();
    assert_eq!(store.schema_version().await.unwrap(), 6);
    store.close().await;
    assert!(path.exists(), "the file has exactly the name asked for");
    for suffix in ["", "-wal", "-shm"] {
        let mut file = path.clone().into_os_string();
        file.push(suffix);
        let _ = std::fs::remove_file(file);
    }
}
