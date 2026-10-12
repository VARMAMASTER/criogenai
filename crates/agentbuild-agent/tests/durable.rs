//! Threads with durability `step` (design 0008 §5.1, §5.5, §12): runs
//! saved per step and per tool call, continued after a crash.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use agentbuild_agent::{Agent, ToolError, tool_fn};
use agentbuild_core::{BoxFuture, PendingWrite};
use agentbuild_providers::{FakeModel, FakeReply, ToolSpec};
use agentbuild_storage::{
    Checkpoint, CheckpointMeta, CheckpointStatus, Checkpointer, Durability, InMemoryCheckpointer,
    Source, Storage, StorageError, StorageErrorKind,
};
use serde_json::json;

fn instant(replies: Vec<FakeReply>) -> Arc<FakeModel> {
    let mut model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO);
    for reply in replies {
        model = model.then(reply);
    }
    Arc::new(model)
}

/// `charge` counts its calls and answers at once; `ship` takes `ship_time`.
fn shop(
    model: Arc<FakeModel>,
    storage: &Storage,
    charges: &Arc<AtomicUsize>,
    ship_time: Duration,
) -> Agent {
    let charges = charges.clone();
    let charge = tool_fn(
        ToolSpec::new("charge", "Charge the card", json!({"type": "object"})),
        move |_args, _ctx| {
            let charges = charges.clone();
            async move {
                let n = charges.fetch_add(1, Ordering::SeqCst) + 1;
                Ok::<_, ToolError>(format!("charged (receipt {n})"))
            }
        },
    );
    let ship = tool_fn(
        ToolSpec::new("ship", "Ship the order", json!({"type": "object"})),
        move |_args, _ctx| async move {
            tokio::time::sleep(ship_time).await;
            Ok::<_, ToolError>("shipped")
        },
    );
    Agent::builder_shared(model)
        .tool(charge)
        .tool(ship)
        .storage(storage.clone())
        .build()
        .unwrap()
}

fn order_replies() -> Vec<FakeReply> {
    vec![
        FakeReply::tool_calls([("charge", json!({})), ("ship", json!({}))]),
        FakeReply::text("Charged and shipped."),
    ]
}

/// Starts an order whose `ship` call hangs, and "kills the process" once
/// `charge` finished: the run's task is aborted mid-step.
async fn crash_during_shipping(storage: &Storage, charges: &Arc<AtomicUsize>) {
    let agent = shop(
        instant(order_replies()),
        storage,
        charges,
        Duration::from_secs(3600),
    );
    let run = tokio::spawn(async move { agent.thread("order-1").send("buy").await });
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(charges.load(Ordering::SeqCst), 1);
    run.abort();
    assert!(run.await.unwrap_err().is_cancelled());
}

async fn recover(storage: &Storage, charges: &Arc<AtomicUsize>) {
    // A new process: a new agent on the same storage, `ship` now works.
    let agent = shop(
        instant(order_replies().split_off(1)),
        storage,
        charges,
        Duration::from_millis(10),
    );
    let chat = agent.thread("order-1");
    let head = chat.head().await.unwrap().unwrap();
    assert_eq!(head.status(), CheckpointStatus::InProgress);
    let pending = &head.snapshot.as_ref().unwrap().pending_writes;
    assert_eq!(
        pending["tools"].memo.len(),
        1,
        "charge was saved: {pending:?}"
    );

    let refused = chat.send("hello?").await.unwrap_err();
    assert_eq!(refused.code, "thread_interrupted");

    let done = chat.retry().await.unwrap();
    assert_eq!(done.text(), Some("Charged and shipped."));
    assert_eq!(
        charges.load(Ordering::SeqCst),
        1,
        "the card is charged once"
    );
    let results: Vec<String> = done.result.messages[2..4]
        .iter()
        .map(|m| m.text())
        .collect();
    assert_eq!(results, ["charged (receipt 1)", "shipped"]);
    assert_eq!(
        chat.head().await.unwrap().unwrap().status(),
        CheckpointStatus::Completed
    );
}

#[tokio::test(start_paused = true)]
async fn a_crash_mid_step_is_recovered_without_repeating_finished_tool_calls() {
    let storage = Storage::in_memory();
    let charges = Arc::new(AtomicUsize::new(0));
    crash_during_shipping(&storage, &charges).await;
    recover(&storage, &charges).await;
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_crash_is_recovered_from_a_sqlite_file() {
    use agentbuild_storage::SqliteCheckpointer;

    let path = std::env::temp_dir().join(format!("agentbuild-agent-{}.db", uuid_like()));
    let charges = Arc::new(AtomicUsize::new(0));
    {
        let storage = Storage::new(SqliteCheckpointer::open(&path).await.unwrap());
        crash_during_shipping(&storage, &charges).await;
    }
    // Reopened from the file: nothing from the first process survives in
    // memory.
    let storage = Storage::new(SqliteCheckpointer::open(&path).await.unwrap());
    recover(&storage, &charges).await;
    let history = storage.checkpointer().history("order-1", 20).await.unwrap();
    let sources: Vec<Source> = history.iter().rev().map(|c| c.source).collect();
    // send: input, model step, (crash in tools); retry: input, tools step,
    // final model step.
    assert_eq!(
        sources,
        [
            Source::Input,
            Source::Step,
            Source::Input,
            Source::Step,
            Source::Step
        ]
    );
    drop(storage);
    for suffix in ["", "-wal", "-shm"] {
        let mut file = path.clone().into_os_string();
        file.push(suffix);
        let _ = std::fs::remove_file(file);
    }
}

#[cfg(feature = "sqlite")]
fn uuid_like() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{nanos}-{}", std::process::id())
}

/// Reads work; every write fails, as if the database went down.
struct Down(InMemoryCheckpointer);

fn down() -> StorageError {
    StorageError::new(StorageErrorKind::Unavailable, "database is down")
}

impl Checkpointer for Down {
    fn load<'a>(
        &'a self,
        thread: &'a str,
        at: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Option<Checkpoint>, StorageError>> {
        self.0.load(thread, at)
    }

    fn commit(&self, _: Checkpoint, _: u64) -> BoxFuture<'_, Result<(), StorageError>> {
        Box::pin(async { Err(down()) })
    }

    fn history<'a>(
        &'a self,
        thread: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<CheckpointMeta>, StorageError>> {
        self.0.history(thread, limit)
    }

    fn delete_thread<'a>(&'a self, thread: &'a str) -> BoxFuture<'a, Result<bool, StorageError>> {
        self.0.delete_thread(thread)
    }

    fn put_pending<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: &'a str,
        _: PendingWrite,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(async { Err(down()) })
    }
}

#[tokio::test(start_paused = true)]
async fn a_database_down_at_send_costs_no_model_call() {
    let model = instant(vec![FakeReply::text("hi")]);
    let agent = Agent::builder_shared(model.clone())
        .storage(Storage::new(Down(InMemoryCheckpointer::new())))
        .build()
        .unwrap();
    let error = agent.thread("t").send("hello").await.unwrap_err();
    assert_eq!(error.code, "storage_unavailable");
    assert_eq!(
        error.details.as_ref().unwrap()["run_error"]["code"],
        "storage_failed"
    );
    assert!(model.requests().is_empty(), "the model was not called");
    // The input is not lost: the error carries the run's snapshot.
    assert_eq!(error.snapshot().unwrap().step, 0);
}

/// Saves checkpoints and node outputs, but no tool result.
struct TasksDown {
    inner: InMemoryCheckpointer,
}

impl Checkpointer for TasksDown {
    fn load<'a>(
        &'a self,
        thread: &'a str,
        at: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Option<Checkpoint>, StorageError>> {
        self.inner.load(thread, at)
    }

    fn commit(
        &self,
        checkpoint: Checkpoint,
        expected: u64,
    ) -> BoxFuture<'_, Result<(), StorageError>> {
        self.inner.commit(checkpoint, expected)
    }

    fn history<'a>(
        &'a self,
        thread: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<CheckpointMeta>, StorageError>> {
        self.inner.history(thread, limit)
    }

    fn delete_thread<'a>(&'a self, thread: &'a str) -> BoxFuture<'a, Result<bool, StorageError>> {
        self.inner.delete_thread(thread)
    }

    fn put_pending<'a>(
        &'a self,
        thread: &'a str,
        checkpoint: &'a str,
        node: &'a str,
        write: PendingWrite,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        // Tool results (memo entries) are lost; the node's output would
        // still be saved.
        if node == "tools" && !write.memo.is_empty() {
            return Box::pin(async { Err(down()) });
        }
        self.inner.put_pending(thread, checkpoint, node, write)
    }
}

#[tokio::test(start_paused = true)]
async fn an_unsaved_tool_result_stops_the_run_before_the_next_model_call() {
    let model = instant(order_replies());
    let charges = Arc::new(AtomicUsize::new(0));
    let storage = Storage::new(TasksDown {
        inner: InMemoryCheckpointer::new(),
    });
    let agent = shop(model.clone(), &storage, &charges, Duration::from_millis(10));
    let chat = agent.thread("t");
    let error = chat.send("buy").await.unwrap_err();
    assert_eq!(error.code, "storage_failed");
    assert!(
        error.message.contains("database is down"),
        "{}",
        error.message
    );
    assert_eq!(model.requests().len(), 1, "no model call after the tools");
    // Saved as a failure; the results were kept in memory, so a retry
    // does not charge again.
    let head = chat.head().await.unwrap().unwrap();
    assert_eq!(head.status(), CheckpointStatus::Failed);
    assert_eq!(head.snapshot.unwrap().pending_writes["tools"].memo.len(), 2);
    assert_eq!(charges.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn durability_exit_writes_one_checkpoint_per_call() {
    let storage = Storage::in_memory().durability(Durability::Exit);
    let charges = Arc::new(AtomicUsize::new(0));
    let agent = shop(instant(order_replies()), &storage, &charges, Duration::ZERO);
    let chat = agent.thread("t");
    assert_eq!(chat.send("buy").await.unwrap().version, 1);
    let history = chat.history(10).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].source, Source::Step);
}
