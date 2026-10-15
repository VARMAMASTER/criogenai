use std::time::{SystemTime, UNIX_EPOCH};

use agentbuild_core::{BoxFuture, PendingWrite, Snapshot, State};
use serde::{Deserialize, Serialize};

use crate::error::StorageError;

/// Why a checkpoint was written (design 0008 §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Source {
    /// Written before a run (or resumed run) executes its first step.
    Input,
    /// A committed step: the run continues from it, or completed with it.
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
    /// A run continues from this checkpoint. If no process is running it
    /// (the run was interrupted, e.g. by a crash), it can be retried from
    /// its snapshot.
    InProgress,
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

    /// From the snapshot: suspended if nodes wait for input, failed if it
    /// records an error, in progress if a run continues from it, completed
    /// without one.
    pub fn status(&self) -> CheckpointStatus {
        match &self.snapshot {
            Some(s) if !s.suspended.is_empty() => CheckpointStatus::Suspended,
            Some(s) if s.error.is_some() => CheckpointStatus::Failed,
            Some(_) => CheckpointStatus::InProgress,
            None => CheckpointStatus::Completed,
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

/// Which threads [`Checkpointer::threads`] lists: in thread id order,
/// optionally only those last committed in a time window (Unix
/// milliseconds), one page at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ThreadQuery {
    /// Only threads last committed before this time (exclusive).
    pub updated_before: Option<u64>,
    /// Only threads last committed at or after this time.
    pub updated_after: Option<u64>,
    /// Only threads whose id sorts after this one (the last id of the
    /// previous page).
    pub after: Option<String>,
    /// The most threads to return. Default 100.
    pub limit: usize,
}

impl Default for ThreadQuery {
    fn default() -> Self {
        Self {
            updated_before: None,
            updated_after: None,
            after: None,
            limit: 100,
        }
    }
}

impl ThreadQuery {
    /// Every thread, 100 at a time.
    pub fn new() -> Self {
        Self::default()
    }

    /// Only threads last committed before `ms` (exclusive).
    pub fn updated_before(mut self, ms: u64) -> Self {
        self.updated_before = Some(ms);
        self
    }

    /// Only threads last committed at or after `ms`.
    pub fn updated_after(mut self, ms: u64) -> Self {
        self.updated_after = Some(ms);
        self
    }

    /// The page after the thread `id`.
    pub fn after(mut self, id: impl Into<String>) -> Self {
        self.after = Some(id.into());
        self
    }

    /// At most `limit` threads.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
}

/// A thread in [`Checkpointer::threads`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ThreadInfo {
    /// The thread's id.
    pub thread_id: String,
    /// The head's version.
    pub version: u64,
    /// When its first checkpoint was committed (Unix milliseconds).
    pub created_at_ms: u64,
    /// When its last checkpoint was committed.
    pub updated_at_ms: u64,
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

    /// Adds work a task did to the snapshot of `checkpoint` (a checkpoint
    /// of `thread` that has one): [`merge_pending`] of `node` and `write`.
    /// Every later [`load`](Self::load) of the checkpoint includes it.
    /// Fails with `checkpoint_not_found` if there is no such checkpoint.
    fn put_pending<'a>(
        &'a self,
        thread: &'a str,
        checkpoint: &'a str,
        node: &'a str,
        write: PendingWrite,
    ) -> BoxFuture<'a, Result<(), StorageError>>;

    /// Threads matching `query`, in thread id order (for retention and for
    /// listing threads).
    fn threads<'a>(
        &'a self,
        query: &'a ThreadQuery,
    ) -> BoxFuture<'a, Result<Vec<ThreadInfo>, StorageError>>;

    /// Shortens the thread: keeps only the head's branch, at most `keep`
    /// checkpoints of it (`None`: all of it; at least the head). Abandoned
    /// branches and older checkpoints are deleted with their task writes.
    /// The oldest kept checkpoint loads as before and its parent is cleared
    /// (design 0008 §15.1). Returns how many checkpoints were deleted.
    fn prune_thread<'a>(
        &'a self,
        thread: &'a str,
        keep: Option<usize>,
    ) -> BoxFuture<'a, Result<u64, StorageError>>;
}

/// What [`Checkpointer::prune_thread`] does to a thread with `checkpoints`
/// (`(id, parent_id)`) and `head`: the ids kept (newest first) and the ids
/// deleted. Nothing is deleted if every checkpoint is kept.
pub(crate) fn prune_plan(
    head: &str,
    checkpoints: &[(String, Option<String>)],
    keep: Option<usize>,
) -> (Vec<String>, Vec<String>) {
    let parents: std::collections::HashMap<&str, Option<&str>> = checkpoints
        .iter()
        .map(|(id, parent)| (id.as_str(), parent.as_deref()))
        .collect();
    let limit = keep.unwrap_or(usize::MAX).max(1);
    let mut kept = Vec::new();
    let mut next = Some(head);
    while let Some(id) = next.filter(|_| kept.len() < limit) {
        let Some(parent) = parents.get(id) else {
            break;
        };
        kept.push(id.to_string());
        next = *parent;
    }
    let keep_set: std::collections::HashSet<&str> = kept.iter().map(String::as_str).collect();
    let deleted = checkpoints
        .iter()
        .filter(|(id, _)| !keep_set.contains(id.as_str()))
        .map(|(id, _)| id.clone())
        .collect();
    (kept, deleted)
}

/// Merges a task write into `snapshot.pending_writes[node]`: memo entries
/// are added (replacing the same keys), `output` and `resume` replace when
/// set. Backends that store writes separately apply them in order with
/// this.
pub fn merge_pending(snapshot: &mut Snapshot, node: &str, write: PendingWrite) {
    let entry = snapshot.pending_writes.entry(node.to_string()).or_default();
    entry.memo.extend(write.memo);
    if write.output.is_some() {
        entry.output = write.output;
    }
    if write.resume.is_some() {
        entry.resume = write.resume;
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
