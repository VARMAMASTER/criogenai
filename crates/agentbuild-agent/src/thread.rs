//! Conversations that last across runs (design 0008 §2.2, §11).
//!
//! A [`Thread`] keeps an agent's transcript in the agent's
//! [`Storage`](agentbuild_storage::Storage): every call ends in a
//! checkpoint, and the next call starts from it. With
//! [`Durability::Step`] (the default) the run is also saved before every
//! step and after every tool call, so an interrupted run can be continued.
//! Approvals, failed and interrupted runs wait in the checkpoint until
//! someone answers or retries them.

use std::collections::BTreeMap;
use std::sync::Arc;

use agentbuild_core::{CancelToken, RunOptions, State};
use agentbuild_providers::Message;
use agentbuild_storage::{
    Checkpoint, CheckpointMeta, CheckpointStatus, Durability, Source, StorageError, ThreadLease,
    ThreadWriter,
};
use serde_json::Value;

use crate::agent::{
    AGENT, Agent, AgentError, AgentInput, AgentResult, HOOKS, MESSAGES, approval_requests,
    read_messages,
};
use crate::approval::{ApprovalRequest, Decision};

/// One conversation with an agent, kept in the agent's storage. Created by
/// [`Agent::thread`]; cheap to create and clone.
///
/// ```
/// use agentbuild_agent::Agent;
/// use agentbuild_providers::{FakeModel, FakeReply};
///
/// # tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap().block_on(async {
/// let model = FakeModel::new()
///     .then(FakeReply::text("Hello Ada"))
///     .then(FakeReply::text("Your name is Ada"));
/// let agent = Agent::builder(model).build().unwrap();
///
/// let chat = agent.thread("user-42");
/// chat.send("I am Ada").await.unwrap();
/// let reply = chat.send("What is my name?").await.unwrap();
/// assert_eq!(reply.text(), Some("Your name is Ada"));
/// assert_eq!(chat.messages().await.unwrap().len(), 4);
/// // Two checkpoints per call: before the first step, and the outcome.
/// assert_eq!(reply.version, 4);
/// # });
/// ```
#[derive(Debug, Clone)]
pub struct Thread {
    agent: Agent,
    id: String,
}

/// The result of a [`Thread`] call that did not fail.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Reply {
    /// The run's result (transcript, output, usage, status).
    pub result: AgentResult,
    /// The checkpoint the call wrote.
    pub checkpoint_id: String,
    /// The thread's version after the call.
    pub version: u64,
}

impl Reply {
    /// The final text, if any.
    pub fn text(&self) -> Option<&str> {
        self.result.text()
    }

    /// `true` if the run waits for approval ([`Thread::approve`],
    /// [`Thread::reject`], [`Thread::decide`]).
    pub fn needs_approval(&self) -> bool {
        self.result.is_suspended()
    }

    /// The calls waiting for approval.
    pub fn approvals(&self) -> Vec<ApprovalRequest> {
        self.result.approval_requests()
    }
}

impl Agent {
    /// The conversation `id` in this agent's
    /// [`storage`](crate::AgentBuilder::storage). Created on first use.
    pub fn thread(&self, id: impl Into<String>) -> Thread {
        Thread {
            agent: self.clone(),
            id: id.into(),
        }
    }
}

impl Thread {
    /// The thread's id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Runs one turn: the stored transcript plus `input`. Fails with
    /// `thread_suspended` while approvals are pending, `thread_interrupted`
    /// if the last run did not finish (continue it with
    /// [`retry`](Self::retry)), and `thread_busy` per the storage's
    /// [`OnBusy`](agentbuild_storage::OnBusy).
    ///
    /// A failed run is still saved, so [`retry`](Self::retry) can continue
    /// it; the next `send` starts from its state.
    pub async fn send(&self, input: impl Into<AgentInput>) -> Result<Reply, AgentError> {
        self.send_with(input, self.agent.run_options()).await
    }

    /// [`send`](Self::send) with options (events, deadline, metadata). A
    /// cancel token in `options` still works: the run uses a child of it.
    pub async fn send_with(
        &self,
        input: impl Into<AgentInput>,
        options: RunOptions,
    ) -> Result<Reply, AgentError> {
        let lease = self.lease().await?;
        let head = self.load().await?;
        let state = match &head {
            Some(head) if head.status() == CheckpointStatus::Suspended => {
                return Err(AgentError::new(
                    "thread_suspended",
                    format!(
                        "thread `{}` is waiting for approval: answer it with approve, reject or decide first",
                        self.id
                    ),
                ));
            }
            Some(head) if head.status() == CheckpointStatus::InProgress => {
                return Err(AgentError::new(
                    "thread_interrupted",
                    format!(
                        "thread `{}` has a run that did not finish (interrupted, or running in \
                         another process): continue it with retry first",
                        self.id
                    ),
                ));
            }
            Some(head) => head.state.clone(),
            None => State::new(),
        };
        let state = next_turn(state, input.into())?;
        let (writer, options) = self.prepare(&lease, head.as_ref(), options);
        let result = self.agent.run_state(state, options).await;
        self.commit(&writer, result).await
    }

    /// Approves every pending call and continues the run.
    pub async fn approve(&self) -> Result<Reply, AgentError> {
        self.answer_all(Decision::approve).await
    }

    /// Rejects every pending call (the model sees `message`) and continues.
    pub async fn reject(&self, message: impl Into<String>) -> Result<Reply, AgentError> {
        let message = message.into();
        self.answer_all(|| Decision::reject(message.clone())).await
    }

    /// Continues with one [`Decision`] per pending call, keyed by `call_id`.
    /// Invalid decisions fail with `resume_invalid` and leave the approval
    /// pending.
    pub async fn decide<I>(&self, decisions: I) -> Result<Reply, AgentError>
    where
        I: IntoIterator<Item = (String, Decision)>,
    {
        self.decide_with(decisions, self.agent.run_options()).await
    }

    /// [`decide`](Self::decide) with options.
    pub async fn decide_with<I>(
        &self,
        decisions: I,
        options: RunOptions,
    ) -> Result<Reply, AgentError>
    where
        I: IntoIterator<Item = (String, Decision)>,
    {
        let decisions: BTreeMap<String, Decision> = decisions.into_iter().collect();
        let lease = self.lease().await?;
        self.resume(&lease, |_| decisions, options).await
    }

    async fn answer_all(&self, decision: impl Fn() -> Decision) -> Result<Reply, AgentError> {
        let lease = self.lease().await?;
        let decide = |requests: &[ApprovalRequest]| {
            requests
                .iter()
                .map(|r| (r.call_id.clone(), decision()))
                .collect()
        };
        self.resume(&lease, decide, self.agent.run_options()).await
    }

    async fn resume(
        &self,
        lease: &ThreadLease,
        decide: impl FnOnce(&[ApprovalRequest]) -> BTreeMap<String, Decision>,
        options: RunOptions,
    ) -> Result<Reply, AgentError> {
        let head = self.load().await?;
        let Some((head, snapshot)) = head
            .as_ref()
            .filter(|h| h.status() == CheckpointStatus::Suspended)
            .and_then(|h| Some((h, h.snapshot.as_ref()?)))
        else {
            return Err(AgentError::resume_invalid(format!(
                "thread `{}` has no pending approval",
                self.id
            )));
        };
        let decisions = decide(&approval_requests(&snapshot.suspended));
        let (writer, options) = self.prepare(lease, Some(head), options);
        let result = self.agent.resume_with(snapshot, decisions, options).await;
        self.commit(&writer, result).await
    }

    /// Continues a failed or interrupted run from where it stopped. Tool
    /// calls that finished (and were saved) are not run again; with
    /// [`Durability::Step`] that includes calls finished just before a
    /// crash.
    pub async fn retry(&self) -> Result<Reply, AgentError> {
        let lease = self.lease().await?;
        let head = self.load().await?;
        let Some((head, snapshot)) = head
            .as_ref()
            .filter(|h| {
                matches!(
                    h.status(),
                    CheckpointStatus::Failed | CheckpointStatus::InProgress
                )
            })
            .and_then(|h| Some((h, h.snapshot.as_ref()?)))
        else {
            return Err(AgentError::resume_invalid(format!(
                "thread `{}` has no failed or interrupted run to retry",
                self.id
            )));
        };
        let (writer, options) = self.prepare(&lease, Some(head), self.agent.run_options());
        let result = self.agent.retry_with(snapshot, options).await;
        self.commit(&writer, result).await
    }

    /// Cancels this thread's active run in this process. `true` if one was
    /// running; it fails with `cancelled` (and is saved for `retry`).
    pub fn cancel(&self) -> bool {
        self.agent.storage().cancel(&self.id)
    }

    /// The latest checkpoint, if the thread exists.
    pub async fn head(&self) -> Result<Option<Checkpoint>, AgentError> {
        self.load().await
    }

    /// The latest state (empty for a new thread).
    pub async fn state(&self) -> Result<State, AgentError> {
        Ok(self.load().await?.map(|h| h.state).unwrap_or_default())
    }

    /// The latest transcript.
    pub async fn messages(&self) -> Result<Vec<Message>, AgentError> {
        read_messages(&self.state().await?).map_err(AgentError::invalid_state)
    }

    /// The calls waiting for approval (empty if none).
    pub async fn pending_approvals(&self) -> Result<Vec<ApprovalRequest>, AgentError> {
        Ok(self
            .load()
            .await?
            .and_then(|h| h.snapshot)
            .map(|s| approval_requests(&s.suspended))
            .unwrap_or_default())
    }

    /// Checkpoints of the current branch, newest first, at most `limit`.
    pub async fn history(&self, limit: usize) -> Result<Vec<CheckpointMeta>, AgentError> {
        self.agent
            .storage()
            .checkpointer()
            .history(&self.id, limit)
            .await
            .map_err(storage_error)
    }

    /// Makes an older checkpoint current again: writes a new head (source
    /// `fork`) with its state, and its pending approval or failure if it
    /// had one. The newer checkpoints stay stored but leave the history.
    /// The next steps run again; recorded results are not replayed.
    pub async fn rewind(&self, checkpoint_id: &str) -> Result<CheckpointMeta, AgentError> {
        let _lease = self.lease().await?;
        let store = self.agent.storage().checkpointer();
        let target = store
            .load(&self.id, Some(checkpoint_id))
            .await
            .map_err(storage_error)?
            .ok_or_else(|| {
                storage_error(StorageError::new(
                    agentbuild_storage::StorageErrorKind::CheckpointNotFound,
                    format!("thread `{}` has no checkpoint `{checkpoint_id}`", self.id),
                ))
            })?;
        let expected = self.load().await?.map_or(0, |h| h.version);
        let mut checkpoint = Checkpoint::new(&self.id, expected + 1, Source::Fork, target.state)
            .parent(target.id)
            .run(target.run_id, target.step);
        if let Some(snapshot) = target.snapshot {
            checkpoint = checkpoint.snapshot(snapshot);
        }
        let meta = checkpoint.meta();
        store
            .commit(checkpoint, expected)
            .await
            .map_err(storage_error)?;
        Ok(meta)
    }

    /// Deletes the thread and its checkpoints (waits for an active run per
    /// `on_busy`). `true` if it existed.
    pub async fn delete(&self) -> Result<bool, AgentError> {
        let _lease = self.lease().await?;
        self.agent
            .storage()
            .checkpointer()
            .delete_thread(&self.id)
            .await
            .map_err(storage_error)
    }

    async fn lease(&self) -> Result<ThreadLease, AgentError> {
        self.agent
            .storage()
            .lease(&self.id)
            .await
            .map_err(storage_error)
    }

    async fn load(&self) -> Result<Option<Checkpoint>, AgentError> {
        self.agent
            .storage()
            .checkpointer()
            .load(&self.id, None)
            .await
            .map_err(storage_error)
    }

    /// A writer that continues from `head`, and `options` set up for the
    /// run: cancellable through the lease, and saved per step if the
    /// storage's durability says so.
    fn prepare(
        &self,
        lease: &ThreadLease,
        head: Option<&Checkpoint>,
        options: RunOptions,
    ) -> (ThreadWriter, RunOptions) {
        let storage = self.agent.storage();
        let writer = ThreadWriter::new(storage.checkpointer().clone(), &self.id, head);
        let mut options = watch(lease, options);
        if storage.durability_mode() == Durability::Step {
            options = options.persist(Arc::new(writer.clone()));
        }
        (writer, options)
    }

    /// Saves the run's outcome as the new head. Errors before the run
    /// started (no snapshot) save nothing.
    async fn commit(
        &self,
        writer: &ThreadWriter,
        result: Result<AgentResult, AgentError>,
    ) -> Result<Reply, AgentError> {
        if let Err(error) = &result
            && error.snapshot().is_none()
        {
            return Err(result.unwrap_err());
        }
        let saved = match &result {
            Ok(done) => match done.snapshot() {
                Some(snapshot) => {
                    writer
                        .commit(
                            Source::Suspend,
                            done.run.state.clone(),
                            (&snapshot.run_id, snapshot.step),
                            Some(snapshot.clone()),
                        )
                        .await
                }
                None => {
                    let step = done.run.steps.last().map_or(0, |s| s.step + 1);
                    writer
                        .commit(
                            Source::Step,
                            done.run.state.clone(),
                            (&done.run.run_id, step),
                            None,
                        )
                        .await
                }
            },
            Err(error) => {
                let snapshot = error.snapshot().expect("checked above");
                writer
                    .commit(
                        Source::Failure,
                        snapshot.state.clone(),
                        (&snapshot.run_id, snapshot.step),
                        Some(snapshot.clone()),
                    )
                    .await
            }
        };
        match (saved, result) {
            (Ok(meta), Ok(result)) => Ok(Reply {
                result,
                checkpoint_id: meta.id,
                version: meta.version,
            }),
            (Ok(_), Err(error)) => Err(error),
            (Err(err), result) => {
                let mut error = storage_error(err);
                if let Err(run) = result {
                    error.details = Some(serde_json::json!({
                        "run_error": {"code": run.code, "message": run.message}
                    }));
                    error.run_error = run.run_error;
                }
                Err(error)
            }
        }
    }
}

/// The state a new turn starts from: the stored state without the last
/// run's bookkeeping, plus the input messages.
fn next_turn(state: State, input: AgentInput) -> Result<State, AgentError> {
    input.check()?;
    let (new_messages, values) = input.into_parts();
    let mut map = state.into_map();
    map.remove(AGENT);
    map.remove(HOOKS);
    let mut messages = match map.remove(MESSAGES) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(messages)) => messages,
        Some(_) => {
            return Err(AgentError::new(
                "invalid_state",
                format!("`{MESSAGES}` is not a list of messages"),
            ));
        }
    };
    if let Some(Value::Array(new)) = Agent::initial_state(new_messages).get(MESSAGES) {
        messages.extend(new.iter().cloned());
    }
    map.extend(values);
    map.insert(MESSAGES.to_string(), Value::Array(messages));
    Ok(State::from(map))
}

/// Registers the run's cancel token with the lease (a child of the
/// caller's token, if any).
fn watch(lease: &ThreadLease, options: RunOptions) -> RunOptions {
    let token = options
        .cancellation()
        .map_or_else(CancelToken::new, CancelToken::child);
    lease.set_cancel(token.clone());
    options.cancel_token(token)
}

fn storage_error(error: StorageError) -> AgentError {
    AgentError::new(error.code(), error.message)
}
