use std::time::{SystemTime, UNIX_EPOCH};

use agentbuild_core::{BoxFuture, Snapshot, State};
use serde::{Deserialize, Serialize};

use crate::error::StorageError;

/// Why a checkpoint was written (design 0008 §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Source {
    /// Input written before a run starts.
    Input,
    /// A committed step (in step 1: a completed run).
    Step,
    /// The run suspended (e.g. waiting for approval).
    Suspend,
    /// The run failed; the snapshot can be retried.
    Failure,
    /// State edited by hand.
    Manual,
    /// An older checkpoint made the head again (rewind).
    Fork,
}

/// Whether a checkpoint's run can continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CheckpointStatus {
    /// Nothing left to run.
    Completed,
    /// Waiting for resume values (approval decisions).
    Suspended,
    /// Failed; can be retried from its snapshot.
    Failed,
}

/// The durable record of a thread's state at one point. Plain JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Checkpoint {
    /// Unique id (UUIDv7, so ids sort by creation time).
    pub id: String,
    /// The thread it belongs to.
    pub thread_id: String,
    /// The checkpoint this one follows, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// The thread's version after this checkpoint: 1, 2, 3…
    pub version: u64,
    /// Why it was written.
    pub source: Source,
    /// The run that produced it (empty if none).
    #[serde(default)]
    pub run_id: String,
    /// The step the run reached.
    #[serde(default)]
    pub step: usize,
    /// The committed state.
    pub state: State,
    /// What a suspended or failed run continues from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Snapshot>,
    /// Creation time, milliseconds since the Unix epoch. For display only:
    /// ordering uses versions (0008 P12).
    pub created_at_ms: u64,
}

impl Checkpoint {
    /// A new checkpoint for `thread_id` at `version`, with a fresh id.
    pub fn new(thread_id: impl Into<String>, version: u64, source: Source, state: State) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            thread_id: thread_id.into(),
            parent_id: None,
            version,
            source,
            run_id: String::new(),
            step: 0,
            state,
            snapshot: None,
            created_at_ms: now_ms(),
        }
    }

    /// Sets [`parent_id`](Self::parent_id).
    pub fn parent(mut self, parent_id: impl Into<String>) -> Self {
        self.parent_id = Some(parent_id.into());
        self
    }

    /// Sets [`run_id`](Self::run_id) and [`step`](Self::step).
    pub fn run(mut self, run_id: impl Into<String>, step: usize) -> Self {
        self.run_id = run_id.into();
        self.step = step;
        self
    }

    /// Sets [`snapshot`](Self::snapshot).
    pub fn snapshot(mut self, snapshot: Snapshot) -> Self {
        self.snapshot = Some(snapshot);
        self
    }

    /// Completed, suspended or failed, from the snapshot.
    pub fn status(&self) -> CheckpointStatus {
        match &self.snapshot {
            Some(s) if !s.suspended.is_empty() => CheckpointStatus::Suspended,
            Some(s) if s.error.is_some() => CheckpointStatus::Failed,
            _ => CheckpointStatus::Completed,
        }
    }

    /// The summary [`Checkpointer::history`] returns.
    pub fn meta(&self) -> CheckpointMeta {
        CheckpointMeta {
            id: self.id.clone(),
            parent_id: self.parent_id.clone(),
            version: self.version,
            source: self.source,
            status: self.status(),
            run_id: self.run_id.clone(),
            step: self.step,
            created_at_ms: self.created_at_ms,
        }
    }
}

/// A checkpoint without its state, for listing history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CheckpointMeta {
    /// See [`Checkpoint::id`].
    pub id: String,
    /// See [`Checkpoint::parent_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// See [`Checkpoint::version`].
    pub version: u64,
    /// See [`Checkpoint::source`].
    pub source: Source,
    /// See [`Checkpoint::status`].
    pub status: CheckpointStatus,
    /// See [`Checkpoint::run_id`].
    pub run_id: String,
    /// See [`Checkpoint::step`].
    pub step: usize,
    /// See [`Checkpoint::created_at_ms`].
    pub created_at_ms: u64,
}

/// Where checkpoints are kept (design 0008 §7). Backends only store and
/// return records; threads, leases and resume logic live above this trait.
///
/// Methods return boxed futures so the trait can be used as
/// `Arc<dyn Checkpointer>`.
pub trait Checkpointer: Send + Sync + 'static {
    /// The thread's head (`at = None`), or the checkpoint `at` if it belongs
    /// to the thread. `None` if there is no such thread or checkpoint.
    fn load<'a>(
        &'a self,
        thread: &'a str,
        at: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Option<Checkpoint>, StorageError>>;

    /// Makes `checkpoint` the head of its thread, if the head's version is
    /// still `expected` (0 for a new thread) and `checkpoint.version` is
    /// `expected + 1`. Otherwise fails with
    /// [`StorageErrorKind::Conflict`](crate::StorageErrorKind::Conflict)
    /// and stores nothing.
    fn commit(
        &self,
        checkpoint: Checkpoint,
        expected: u64,
    ) -> BoxFuture<'_, Result<(), StorageError>>;

    /// The head's branch (the head, its parent, …), newest first, at most
    /// `limit` entries. Empty for an unknown thread.
    fn history<'a>(
        &'a self,
        thread: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<CheckpointMeta>, StorageError>>;

    /// Removes the thread and all its checkpoints. `true` if it existed.
    fn delete_thread<'a>(&'a self, thread: &'a str) -> BoxFuture<'a, Result<bool, StorageError>>;
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
