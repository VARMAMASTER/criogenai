use std::sync::{Arc, Mutex, MutexGuard};

use agentbuild_core::{BoxFuture, NodeError, PendingWrite, Persister, Snapshot, State};

use crate::checkpoint::{Checkpoint, CheckpointMeta, Checkpointer, Source};
use crate::error::{StorageError, StorageErrorKind};

/// Writes one thread's checkpoints during a call, each following the last
/// one it wrote (compare-and-set on the thread's version).
///
/// As a [`Persister`] (pass it to
/// [`RunOptions::persist`](agentbuild_core::RunOptions::persist)) it saves
/// the run's progress: an [`Input`](Source::Input) checkpoint before the
/// first step, a [`Step`](Source::Step) checkpoint after each step, and
/// task writes into the latest one. [`commit`](Self::commit) then saves
/// the outcome. Cheap to clone; clones share the head.
#[derive(Clone)]
pub struct ThreadWriter {
    inner: Arc<Inner>,
}

struct Inner {
    store: Arc<dyn Checkpointer>,
    thread: String,
    head: Mutex<Head>,
}

#[derive(Clone, Default)]
struct Head {
    id: Option<String>,
    version: u64,
    /// Whether this writer saved a checkpoint yet.
    wrote: bool,
}

impl std::fmt::Debug for ThreadWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let head = self.head();
        f.debug_struct("ThreadWriter")
            .field("thread", &self.inner.thread)
            .field("head", &head.id)
            .field("version", &head.version)
            .finish()
    }
}

impl ThreadWriter {
    /// A writer for `thread`, whose current head is `head` (`None` for a
    /// new thread).
    pub fn new(
        store: Arc<dyn Checkpointer>,
        thread: impl Into<String>,
        head: Option<&Checkpoint>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                thread: thread.into(),
                head: Mutex::new(Head {
                    id: head.map(|h| h.id.clone()),
                    version: head.map_or(0, |h| h.version),
                    wrote: false,
                }),
            }),
        }
    }

    /// The id of the thread's head as this writer knows it.
    pub fn head_id(&self) -> Option<String> {
        self.head().id
    }

    /// The thread's version as this writer knows it.
    pub fn version(&self) -> u64 {
        self.head().version
    }

    /// Writes the next checkpoint: `state` (and `snapshot`, if the run can
    /// continue from it) at `step` of `run_id`, after the current head.
    pub async fn commit(
        &self,
        source: Source,
        state: State,
        run: (&str, usize),
        snapshot: Option<Snapshot>,
    ) -> Result<CheckpointMeta, StorageError> {
        let head = self.head();
        let mut checkpoint =
            Checkpoint::new(&self.inner.thread, head.version + 1, source, state).run(run.0, run.1);
        if let Some(parent) = head.id {
            checkpoint = checkpoint.parent(parent);
        }
        if let Some(snapshot) = snapshot {
            checkpoint = checkpoint.snapshot(snapshot);
        }
        let meta = checkpoint.meta();
        self.inner.store.commit(checkpoint, head.version).await?;
        let mut head = self.lock();
        head.id = Some(meta.id.clone());
        head.version = meta.version;
        head.wrote = true;
        Ok(meta)
    }

    fn head(&self) -> Head {
        self.lock().clone()
    }

    fn lock(&self) -> MutexGuard<'_, Head> {
        self.inner.head.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Persister for ThreadWriter {
    fn save_checkpoint(&self, snapshot: Snapshot) -> BoxFuture<'static, Result<(), NodeError>> {
        let writer = self.clone();
        Box::pin(async move {
            let source = if writer.head().wrote {
                Source::Step
            } else {
                Source::Input
            };
            let state = snapshot.state.clone();
            let run_id = snapshot.run_id.clone();
            let step = snapshot.step;
            writer
                .commit(source, state, (&run_id, step), Some(snapshot))
                .await
                .map(drop)
                .map_err(node_error)
        })
    }

    fn save_task(
        &self,
        _step: usize,
        node: String,
        write: PendingWrite,
    ) -> BoxFuture<'static, Result<(), NodeError>> {
        let writer = self.clone();
        Box::pin(async move {
            let head = writer.head();
            let Some(checkpoint) = head.id.filter(|_| head.wrote) else {
                return Err(node_error(StorageError::new(
                    StorageErrorKind::CheckpointNotFound,
                    "no checkpoint of this run was saved yet",
                )));
            };
            let inner = &writer.inner;
            inner
                .store
                .put_pending(&inner.thread, &checkpoint, &node, write)
                .await
                .map_err(node_error)
        })
    }
}

fn node_error(error: StorageError) -> NodeError {
    NodeError::new(error.message.clone()).with_kind(error.code())
}
