//! Saving a run while it executes (design 0008 §5.1, durability `step`).

use crate::error::NodeError;
use crate::node::BoxFuture;
use crate::runtime::{PendingWrite, Snapshot};

/// Receives a run's progress as it happens, so a crash loses at most the
/// work of the step in flight. Set with
/// [`RunOptions::persist`](crate::RunOptions::persist); storage backends
/// implement it.
///
/// Every call is awaited before the run goes on. An error stops the run with
/// [`RunErrorKind::StorageFailed`](crate::RunErrorKind::StorageFailed); its
/// snapshot keeps everything done so far in memory.
pub trait Persister: Send + Sync + 'static {
    /// The run is about to execute `snapshot.step` from `snapshot`: once
    /// before the first step of every `run`, `resume` and `retry` (with the
    /// resume values and saved work in `pending_writes`), then after each
    /// committed step the run continues from. Not called when the run
    /// completes, suspends or fails: the caller saves that outcome.
    fn save_checkpoint(&self, snapshot: Snapshot) -> BoxFuture<'static, Result<(), NodeError>>;

    /// Work a task did in `step` of the last saved checkpoint: its output
    /// when it finished, or a [`TaskMemo::save`](crate::TaskMemo::save)
    /// entry. Merge it into that checkpoint's `pending_writes[node]`: memo
    /// entries are added, `output` and `resume` replace.
    fn save_task(
        &self,
        step: usize,
        node: String,
        write: PendingWrite,
    ) -> BoxFuture<'static, Result<(), NodeError>>;
}
