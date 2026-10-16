//! Threads for workflows (design 0008 §18): the same durable, leased,
//! audited conversation as an agent's thread, for any [`Workflow`].

use std::fmt;
use std::sync::Arc;

use agentbuild_core::{CancelToken, Interrupt, RunError, RunOptions, RunOutput, State, Workflow};
use serde_json::Value;

use crate::audit::AuditEvent;
use crate::checkpoint::{Checkpoint, CheckpointMeta, CheckpointStatus, Source};
use crate::error::{StorageError, StorageErrorKind};
use crate::storage::{Durability, Storage, ThreadLease};
use crate::writer::ThreadWriter;

/// Why a workflow thread call failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum ThreadError {
    /// The run failed; it is saved, and [`WorkflowThread::retry`]
    /// continues it when the error has a snapshot.
    Run(Box<RunError>),
    /// Storage failed (the code says how: `thread_busy`, …).
    Storage(StorageError),
    /// The call does not fit the thread's state: `thread_suspended`,
    /// `thread_interrupted` or `resume_invalid`.
    State {
        /// The stable code.
        code: &'static str,
        /// What is wrong.
        message: String,
    },
}

impl ThreadError {
    /// The stable code (design 0013 catalog).
    pub fn code(&self) -> &str {
        match self {
            Self::Run(error) => error.kind.code(),
            Self::Storage(error) => error.code(),
            Self::State { code, .. } => code,
        }
    }

    fn state(code: &'static str, message: impl Into<String>) -> Self {
        Self::State {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for ThreadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Run(error) => write!(f, "{error}"),
            Self::Storage(error) => write!(f, "{error}"),
            Self::State { code, message } => write!(f, "{code}: {message}"),
        }
    }
}

impl std::error::Error for ThreadError {}

impl From<StorageError> for ThreadError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

/// The result of a workflow thread call that did not fail: the run, and
/// the checkpoint it wrote.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Turn {
    /// The run's output (state, steps, status).
    pub run: RunOutput,
    /// The checkpoint the call wrote.
    pub checkpoint_id: String,
    /// The thread's version after the call.
    pub version: u64,
}

impl Turn {
    /// The state the run ended with.
    pub fn state(&self) -> &State {
        &self.run.state
    }

    /// `true` if nodes wait for input ([`WorkflowThread::resume`]).
    pub fn is_suspended(&self) -> bool {
        self.run.is_suspended()
    }

    /// What the waiting nodes asked for.
    pub fn interrupts(&self) -> &[Interrupt] {
        self.run.interrupts()
    }
}

/// One conversation with a workflow, kept in [`Storage`]: from
/// [`Storage::workflow_thread`].
///
/// Each [`run`](Self::run) starts from the state the last one ended with,
/// plus the input's values. A run that suspends waits in storage for
/// [`resume`](Self::resume), from this process or another; a failed or
/// interrupted run waits for [`retry`](Self::retry).
#[derive(Clone)]
pub struct WorkflowThread {
    storage: Storage,
    workflow: Arc<Workflow>,
    id: String,
    actor: Option<String>,
    options: RunOptions,
}

impl fmt::Debug for WorkflowThread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkflowThread")
            .field("id", &self.id)
            .field("actor", &self.actor)
            .finish_non_exhaustive()
    }
}

impl Storage {
    /// The thread `id` of `workflow` (created on first use). Threads of
    /// different workflows must not share ids: a snapshot from another
    /// workflow is refused (`workflow_mismatch`).
    pub fn workflow_thread(&self, workflow: &Workflow, id: impl Into<String>) -> WorkflowThread {
        WorkflowThread {
            storage: self.clone(),
            workflow: Arc::new(workflow.clone()),
            id: id.into(),
            actor: None,
            options: RunOptions::default(),
        }
    }
}

impl WorkflowThread {
    /// The thread's id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Who acts through this handle, for the audit trail (design 0010).
    pub fn actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
        self
    }

    /// The options every run of this handle starts from (deadline, events,
    /// metadata, step limit).
    pub fn with_options(mut self, options: RunOptions) -> Self {
        self.options = options;
        self
    }

    /// Runs the workflow from its entry node, on the state the last run
    /// ended with plus `input`'s values (an input key replaces the stored
    /// one). Fails with `thread_suspended` while nodes wait for input and
    /// `thread_interrupted` after an unfinished run.
    pub async fn run(&self, input: State) -> Result<Turn, ThreadError> {
        let lease = self.lease().await?;
        let head = self.head().await?;
        let mut state = match &head {
            Some(head) if head.status() == CheckpointStatus::Suspended => {
                return Err(ThreadError::state(
                    "thread_suspended",
                    format!("thread `{}` waits for input: resume it first", self.id),
                ));
            }
            Some(head) if head.status() == CheckpointStatus::InProgress => {
                return Err(ThreadError::state(
                    "thread_interrupted",
                    format!(
                        "thread `{}` has a run that did not finish: continue it with retry first",
                        self.id
                    ),
                ));
            }
            Some(head) => head.state.clone().into_map(),
            None => Default::default(),
        };
        state.extend(input.into_map());
        let (writer, options) = self.prepare(&lease, head.as_ref());
        let result = self.workflow.run_with(State::from(state), options).await;
        self.commit(&writer, result, "run").await
    }

    /// Answers the waiting nodes: one value per node name (what each gets
    /// as `ctx.resume()`). Invalid values are refused before anything
    /// runs (`resume_invalid`), and the thread keeps waiting.
    pub async fn resume<I, K>(&self, values: I) -> Result<Turn, ThreadError>
    where
        I: IntoIterator<Item = (K, Value)>,
        K: Into<String>,
    {
        let values: Vec<(String, Value)> = values.into_iter().map(|(k, v)| (k.into(), v)).collect();
        let lease = self.lease().await?;
        let head = self.head().await?;
        let Some((head, snapshot)) = head
            .as_ref()
            .filter(|h| h.status() == CheckpointStatus::Suspended)
            .and_then(|h| Some((h, h.snapshot.clone()?)))
        else {
            return Err(ThreadError::state(
                "resume_invalid",
                format!("thread `{}` does not wait for input", self.id),
            ));
        };
        let nodes: Vec<String> = values.iter().map(|(node, _)| node.clone()).collect();
        let (writer, options) = self.prepare(&lease, Some(head));
        let result = self.workflow.resume_with(&snapshot, values, options).await;
        // Refused input changes nothing: no checkpoint, no record.
        if let Err(error) = &result
            && error.snapshot().is_none()
        {
            return Err(ThreadError::Run(Box::new(result.unwrap_err())));
        }
        for node in nodes {
            self.record(
                AuditEvent::new("run.resumed")
                    .run(Some(snapshot.run_id.clone()))
                    .subject(node)
                    .outcome("answered"),
            )
            .await?;
        }
        self.commit(&writer, result, "resume").await
    }

    /// Continues a failed or interrupted run from where it stopped; nodes
    /// that finished (and were saved) are not run again.
    pub async fn retry(&self) -> Result<Turn, ThreadError> {
        let lease = self.lease().await?;
        let head = self.head().await?;
        let Some((head, snapshot)) = head
            .as_ref()
            .filter(|h| {
                matches!(
                    h.status(),
                    CheckpointStatus::Failed | CheckpointStatus::InProgress
                )
            })
            .and_then(|h| Some((h, h.snapshot.clone()?)))
        else {
            return Err(ThreadError::state(
                "resume_invalid",
                format!(
                    "thread `{}` has no failed or interrupted run to retry",
                    self.id
                ),
            ));
        };
        let (writer, options) = self.prepare(&lease, Some(head));
        let result = self.workflow.retry_with(&snapshot, options).await;
        self.commit(&writer, result, "retry").await
    }

    /// Cancels this thread's active run in this process. `true` if one was
    /// running; it fails with `cancelled` and is saved for `retry`.
    pub fn cancel(&self) -> bool {
        self.storage.cancel(&self.id)
    }

    /// The latest checkpoint, if the thread exists.
    pub async fn head(&self) -> Result<Option<Checkpoint>, ThreadError> {
        Ok(self.storage.checkpointer().load(&self.id, None).await?)
    }

    /// The current state (empty for a new thread).
    pub async fn state(&self) -> Result<State, ThreadError> {
        Ok(self.head().await?.map(|h| h.state).unwrap_or_default())
    }

    /// What the waiting nodes asked for (empty unless suspended).
    pub async fn interrupts(&self) -> Result<Vec<Interrupt>, ThreadError> {
        Ok(self
            .head()
            .await?
            .filter(|h| h.status() == CheckpointStatus::Suspended)
            .and_then(|h| h.snapshot)
            .map(|s| s.suspended)
            .unwrap_or_default())
    }

    /// Checkpoints, newest first.
    pub async fn history(&self, limit: usize) -> Result<Vec<CheckpointMeta>, ThreadError> {
        Ok(self.storage.checkpointer().history(&self.id, limit).await?)
    }

    /// Makes an older checkpoint current again (design 0008 §5.3).
    pub async fn rewind(&self, checkpoint_id: &str) -> Result<CheckpointMeta, ThreadError> {
        let _lease = self.lease().await?;
        let store = self.storage.checkpointer();
        let target = store
            .load(&self.id, Some(checkpoint_id))
            .await?
            .ok_or_else(|| {
                StorageError::new(
                    StorageErrorKind::CheckpointNotFound,
                    format!("thread `{}` has no checkpoint `{checkpoint_id}`", self.id),
                )
            })?;
        let expected = self.head().await?.map_or(0, |h| h.version);
        let mut checkpoint = Checkpoint::new(&self.id, expected + 1, Source::Fork, target.state)
            .parent(target.id)
            .run(target.run_id, target.step);
        if let Some(snapshot) = target.snapshot {
            checkpoint = checkpoint.snapshot(snapshot);
        }
        let meta = checkpoint.meta();
        store.commit(checkpoint, expected).await?;
        self.record(
            AuditEvent::new("thread.rewound")
                .subject(checkpoint_id)
                .outcome("ok")
                .detail("version", meta.version),
        )
        .await?;
        Ok(meta)
    }

    /// Deletes the thread and its checkpoints. `true` if it existed.
    pub async fn delete(&self) -> Result<bool, ThreadError> {
        let _lease = self.lease().await?;
        let existed = self.storage.checkpointer().delete_thread(&self.id).await?;
        self.record(AuditEvent::new("thread.deleted").outcome(if existed {
            "deleted"
        } else {
            "not_found"
        }))
        .await?;
        Ok(existed)
    }

    async fn lease(&self) -> Result<ThreadLease, ThreadError> {
        Ok(self.storage.lease(&self.id).await?)
    }

    async fn record(&self, event: AuditEvent) -> Result<(), ThreadError> {
        Ok(self
            .storage
            .record(
                event
                    .thread(Some(self.id.clone()))
                    .actor(self.actor.clone()),
            )
            .await?)
    }

    /// A writer after `head`, and the run's options: its cancel token is
    /// registered with the lease, and with durability `step` progress is
    /// saved as the run goes.
    fn prepare(
        &self,
        lease: &ThreadLease,
        head: Option<&Checkpoint>,
    ) -> (ThreadWriter, RunOptions) {
        let writer = ThreadWriter::new(self.storage.checkpointer().clone(), &self.id, head);
        let options = self.options.clone();
        let token = options
            .cancellation()
            .map_or_else(CancelToken::new, CancelToken::child);
        lease.set_cancel(token.clone());
        let mut options = options.cancel_token(token);
        if self.storage.durability_mode() == Durability::Step {
            options = options.persist(Arc::new(writer.clone()));
        }
        (writer, options)
    }

    /// Saves the outcome as the new head and records it. A run error
    /// without a snapshot (refused before it started) saves nothing.
    async fn commit(
        &self,
        writer: &ThreadWriter,
        result: Result<RunOutput, RunError>,
        operation: &str,
    ) -> Result<Turn, ThreadError> {
        let saved = match &result {
            Ok(run) => match run.snapshot() {
                Some(snapshot) => {
                    writer
                        .commit(
                            Source::Suspend,
                            run.state.clone(),
                            (&snapshot.run_id, snapshot.step),
                            Some(snapshot.clone()),
                        )
                        .await
                }
                None => {
                    let step = run.steps.last().map_or(0, |s| s.step + 1);
                    writer
                        .commit(Source::Step, run.state.clone(), (&run.run_id, step), None)
                        .await
                }
            },
            Err(error) => match error.snapshot() {
                Some(snapshot) => {
                    writer
                        .commit(
                            Source::Failure,
                            snapshot.state.clone(),
                            (&snapshot.run_id, snapshot.step),
                            Some(snapshot.clone()),
                        )
                        .await
                }
                None => return Err(ThreadError::Run(Box::new(result.unwrap_err()))),
            },
        };
        let saved = saved?;
        let (run_id, outcome) = match &result {
            Ok(run) => (
                run.run_id.clone(),
                if run.is_suspended() {
                    "suspended"
                } else {
                    "completed"
                },
            ),
            Err(error) => (
                error
                    .snapshot()
                    .map(|s| s.run_id.clone())
                    .unwrap_or_default(),
                error.kind.code(),
            ),
        };
        self.record(
            AuditEvent::new("run.finished")
                .run(Some(run_id))
                .subject(operation)
                .outcome(outcome)
                .detail("version", saved.version),
        )
        .await?;
        match result {
            Ok(run) => Ok(Turn {
                run,
                checkpoint_id: saved.id,
                version: saved.version,
            }),
            Err(error) => Err(ThreadError::Run(Box::new(error))),
        }
    }
}
