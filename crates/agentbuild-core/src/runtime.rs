use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;

use crate::cancel::CancelToken;
use crate::error::{NodeError, RunError, RunErrorKind};
use crate::event::{
    Event, EventKind, EventReceiver, EventSender, EventSink, Hub, Scope, event_channel,
};
use crate::graph::{Graph, Outgoing, Target, Workflow};
use crate::node::{Node, NodeContext, NodeOutput};
use crate::policy::NodePolicy;
use crate::reducer;
use crate::state::State;

/// The `format` value of snapshots produced by this version.
pub const SNAPSHOT_FORMAT: &str = "agentbuild.snapshot/1";

/// Channel capacity used by [`Workflow::stream`].
const STREAM_CAPACITY: usize = 256;

/// Per-run settings.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunOptions {
    /// Maximum step index (counted from the start of the run, across resumes)
    /// before the run fails with [`RunErrorKind::StepLimitExceeded`]. Guards
    /// against unbounded cycles.
    pub max_steps: usize,
    /// Time limit for this `run`/`resume` call, including retries and
    /// backoff. Exceeding it fails the run with
    /// [`RunErrorKind::DeadlineExceeded`].
    pub deadline: Option<Duration>,
    /// After cancellation, how long in-flight nodes get to return before
    /// they are dropped. Default 5 s.
    pub cancel_grace: Duration,
    pub(crate) events: Option<EventSender>,
    pub(crate) cancel: Option<CancelToken>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            max_steps: 64,
            deadline: None,
            cancel_grace: Duration::from_secs(5),
            events: None,
            cancel: None,
        }
    }
}

impl RunOptions {
    /// Sets [`max_steps`](Self::max_steps).
    pub fn max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
        self
    }

    /// Sets [`deadline`](Self::deadline).
    pub fn deadline(mut self, deadline: Duration) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Lets the caller cancel the run with `token.cancel()`. The run then
    /// fails with [`RunErrorKind::Cancelled`].
    pub fn cancel_token(mut self, token: CancelToken) -> Self {
        self.cancel = Some(token);
        self
    }

    /// Sets [`cancel_grace`](Self::cancel_grace).
    pub fn cancel_grace(mut self, grace: Duration) -> Self {
        self.cancel_grace = grace;
        self
    }

    /// Sends the run's [`Event`]s to `sender` (see [`event_channel`]).
    pub fn events(mut self, sender: EventSender) -> Self {
        self.events = Some(sender);
        self
    }
}

/// The result of a run (or a resumed segment of one) that did not fail.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct RunOutput {
    /// Whether the run completed or is waiting to be resumed.
    pub status: RunStatus,
    /// State after the last committed step.
    pub state: State,
    /// Which nodes ran in each step of this segment, in order.
    pub steps: Vec<StepRecord>,
    /// The run's id, shared by all of its segments.
    pub run_id: String,
}

impl RunOutput {
    /// `true` if the run paused and must be resumed to continue.
    pub fn is_suspended(&self) -> bool {
        matches!(self.status, RunStatus::Suspended { .. })
    }

    /// The snapshot to pass to [`Workflow::resume`], if suspended.
    pub fn snapshot(&self) -> Option<&Snapshot> {
        match &self.status {
            RunStatus::Suspended { snapshot, .. } => Some(snapshot.as_ref()),
            RunStatus::Completed => None,
        }
    }

    /// The suspension payloads, if suspended.
    pub fn interrupts(&self) -> &[Interrupt] {
        match &self.status {
            RunStatus::Suspended { interrupts, .. } => interrupts,
            RunStatus::Completed => &[],
        }
    }
}

/// How a non-failed run ended.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RunStatus {
    /// Nothing is left to run.
    Completed,
    /// One or more nodes returned [`NodeOutput::Suspend`].
    Suspended {
        /// One entry per suspended node, in declaration order.
        interrupts: Vec<Interrupt>,
        /// Everything needed to continue the run later. Boxed to keep
        /// `RunStatus` small.
        snapshot: Box<Snapshot>,
    },
}

/// A node's request to pause the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Interrupt {
    /// The suspended node.
    pub node: String,
    /// What the node asked for.
    pub payload: Value,
}

/// A paused run, as plain JSON-serializable data. Store it anywhere and pass it
/// to [`Workflow::resume`], from this process or another one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Snapshot {
    /// Always [`SNAPSHOT_FORMAT`] for snapshots produced by this version.
    pub format: String,
    /// [`Workflow::fingerprint`] of the workflow that produced it.
    pub workflow_fingerprint: String,
    /// The run's id.
    pub run_id: String,
    /// Index of the next step to execute.
    pub step: usize,
    /// Committed state.
    pub state: State,
    /// Nodes scheduled for the next step, in declaration order. Includes the
    /// suspended nodes.
    pub frontier: Vec<String>,
    /// For each join target, the sources that completed since it last fired.
    pub join_progress: BTreeMap<String, Vec<String>>,
    /// The suspended nodes and their payloads.
    pub suspended: Vec<Interrupt>,
    /// The [`seq`](Event::seq) the run's next event gets, so event numbering
    /// continues after resume.
    #[serde(default)]
    pub event_seq: u64,
}

/// A run executing in the background, with its events. Created by
/// [`Workflow::stream`].
///
/// Read events with [`next`](Self::next) until it returns `None`, then call
/// [`finish`](Self::finish) for the result. [`cancel`](Self::cancel) stops the
/// run gracefully; dropping a `RunStream` stops it at once.
#[derive(Debug)]
pub struct RunStream {
    events: Option<EventReceiver>,
    handle: Option<JoinHandle<Result<RunOutput, RunError>>>,
    cancel: CancelToken,
}

#[allow(clippy::result_large_err)]
impl RunStream {
    /// Cancels the run (see [`RunOptions::cancel_token`]). Keep reading
    /// events, or call [`finish`](Self::finish), to see it end.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// The next event, or `None` after the run's last event
    /// ([`EventKind::RunFinished`]).
    pub async fn next(&mut self) -> Option<Event> {
        match &mut self.events {
            Some(events) => events.recv().await,
            None => None,
        }
    }

    /// Waits for the run to end and returns its result. Events not yet read
    /// are discarded.
    pub async fn finish(mut self) -> Result<RunOutput, RunError> {
        // Stop delivery first, so unread events cannot hold the run back.
        self.events = None;
        let handle = self.handle.take().expect("finish consumes the stream");
        match handle.await {
            Ok(result) => result,
            Err(err) if err.is_panic() => std::panic::resume_unwind(err.into_panic()),
            Err(_) => panic!("run task was cancelled (is the Tokio runtime shutting down?)"),
        }
    }
}

impl Drop for RunStream {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

/// The nodes that ran in one step, in declaration order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct StepRecord {
    /// Zero-based step index, counted from the start of the run.
    pub step: usize,
    /// Node names.
    pub nodes: Vec<String>,
}

// `RunError` carries the last committed state, so it is large by design. It is
// only built on the failure path, and boxing it would complicate the public API.
#[allow(clippy::result_large_err)]
impl Workflow {
    /// Runs the workflow with default [`RunOptions`].
    ///
    /// Must be called within a Tokio runtime. Dropping the returned future
    /// cancels the run, including any nodes in flight.
    pub async fn run(&self, input: State) -> Result<RunOutput, RunError> {
        self.run_with(input, RunOptions::default()).await
    }

    /// Runs the workflow with explicit options.
    pub async fn run_with(&self, input: State, options: RunOptions) -> Result<RunOutput, RunError> {
        let run_id = uuid::Uuid::now_v7().to_string();
        let run = Run::new(&self.graph, run_id, input, 0, &options);
        let start = BTreeSet::from([self.graph.entry]);
        run.execute(start, 0, HashMap::new(), options, false).await
    }

    /// Starts a run in the background (on the current Tokio runtime) and
    /// returns its events as they happen.
    ///
    /// ```
    /// # use agentbuild_core::{END, State, Update, WorkflowBuilder, node_fn};
    /// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
    /// # let mut builder = WorkflowBuilder::new();
    /// # builder.add_node("a", node_fn(|_| async { Ok(Update::new()) })).set_entry("a").add_edge("a", END);
    /// # let workflow = builder.build().unwrap();
    /// let mut run = workflow.stream(State::new());
    /// while let Some(event) = run.next().await {
    ///     println!("{} {:?}", event.type_name(), event.node);
    /// }
    /// let output = run.finish().await.unwrap();
    /// # });
    /// ```
    pub fn stream(&self, input: State) -> RunStream {
        self.stream_with(input, RunOptions::default())
    }

    /// [`stream`](Self::stream) with explicit options. Any event sender
    /// already in `options` is replaced.
    pub fn stream_with(&self, input: State, mut options: RunOptions) -> RunStream {
        let (sender, events) = event_channel(STREAM_CAPACITY);
        let cancel = options.cancel.get_or_insert_with(CancelToken::new).clone();
        let workflow = self.clone();
        let handle =
            tokio::spawn(async move { workflow.run_with(input, options.events(sender)).await });
        RunStream {
            events: Some(events),
            handle: Some(handle),
            cancel,
        }
    }

    /// Continues a suspended run. `values` maps each suspended node's name to
    /// the value it receives as [`NodeContext::resume`]; every suspended node
    /// needs exactly one value.
    ///
    /// Invalid input is rejected before anything runs, so the snapshot stays
    /// usable.
    pub async fn resume<I, K>(&self, snapshot: &Snapshot, values: I) -> Result<RunOutput, RunError>
    where
        I: IntoIterator<Item = (K, Value)>,
        K: Into<String>,
    {
        self.resume_with(snapshot, values, RunOptions::default())
            .await
    }

    /// [`resume`](Self::resume) with explicit options.
    pub async fn resume_with<I, K>(
        &self,
        snapshot: &Snapshot,
        values: I,
        options: RunOptions,
    ) -> Result<RunOutput, RunError>
    where
        I: IntoIterator<Item = (K, Value)>,
        K: Into<String>,
    {
        let graph = &*self.graph;
        let reject = |kind: RunErrorKind| RunError {
            kind,
            step: snapshot.step,
            state: snapshot.state.clone(),
        };
        let invalid = |reason: String| reject(RunErrorKind::ResumeInvalid { reason });

        if snapshot.format != SNAPSHOT_FORMAT {
            return Err(invalid(format!(
                "unsupported snapshot format `{}` (expected `{SNAPSHOT_FORMAT}`)",
                snapshot.format
            )));
        }
        if snapshot.workflow_fingerprint != graph.fingerprint {
            return Err(reject(RunErrorKind::WorkflowMismatch {
                expected: graph.fingerprint.clone(),
                found: snapshot.workflow_fingerprint.clone(),
            }));
        }

        let id_of = |name: &str| {
            graph
                .index_of(name)
                .ok_or_else(|| invalid(format!("snapshot refers to unknown node `{name}`")))
        };

        let mut values: HashMap<String, Value> =
            values.into_iter().map(|(k, v)| (k.into(), v)).collect();
        let mut resume = HashMap::new();
        for interrupt in &snapshot.suspended {
            let value = values.remove(&interrupt.node).ok_or_else(|| {
                invalid(format!(
                    "missing resume value for suspended node `{}`",
                    interrupt.node
                ))
            })?;
            resume.insert(id_of(&interrupt.node)?, value);
        }
        if let Some(extra) = values.keys().min() {
            return Err(invalid(format!("node `{extra}` is not suspended")));
        }

        let mut frontier = BTreeSet::new();
        for name in &snapshot.frontier {
            frontier.insert(id_of(name)?);
        }
        let mut join_progress = vec![HashSet::new(); graph.joins.len()];
        for (target, sources) in &snapshot.join_progress {
            let target = id_of(target)?;
            let j = graph
                .joins
                .iter()
                .position(|join| join.target == target)
                .ok_or_else(|| invalid(format!("no join into `{}`", graph.names[target])))?;
            for source in sources {
                join_progress[j].insert(id_of(source)?);
            }
        }

        let mut run = Run::new(
            graph,
            snapshot.run_id.clone(),
            snapshot.state.clone(),
            snapshot.event_seq,
            &options,
        );
        run.join_progress = join_progress;
        run.execute(frontier, snapshot.step, resume, options, true)
            .await
    }
}

struct Run<'g> {
    graph: &'g Graph,
    run_id: Arc<str>,
    state: Arc<State>,
    /// For each join, the sources that have completed since it last fired.
    join_progress: Vec<HashSet<usize>>,
    /// Event delivery; `None` without a subscriber.
    hub: Option<Arc<Hub>>,
    /// `seq` of this segment's first event.
    first_seq: u64,
    /// Fires when the caller cancels; parent of every node's token.
    cancel: CancelToken,
    deadline: Option<Deadline>,
    cancel_grace: Duration,
}

#[derive(Clone, Copy)]
struct Deadline {
    at: Instant,
    limit_ms: u64,
}

impl Deadline {
    fn exceeded(self) -> RunErrorKind {
        RunErrorKind::DeadlineExceeded {
            deadline_ms: self.limit_ms,
        }
    }
}

/// Why the run had to stop waiting.
#[derive(Clone, Copy)]
enum Stop {
    Cancelled,
    Deadline,
}

struct StepOutcome {
    next: BTreeSet<usize>,
    suspended: Vec<(usize, Value)>,
}

#[allow(clippy::result_large_err)]
impl<'g> Run<'g> {
    fn new(
        graph: &'g Graph,
        run_id: String,
        state: State,
        first_seq: u64,
        options: &RunOptions,
    ) -> Self {
        let run_id: Arc<str> = run_id.into();
        let hub = options
            .events
            .clone()
            .map(|sender| Hub::new(run_id.clone(), sender, first_seq));
        let deadline = options.deadline.map(|limit| Deadline {
            at: Instant::now() + limit,
            limit_ms: u64::try_from(limit.as_millis()).unwrap_or(u64::MAX),
        });
        Self {
            graph,
            run_id,
            state: Arc::new(state),
            join_progress: vec![HashSet::new(); graph.joins.len()],
            hub,
            first_seq,
            cancel: options.cancel.clone().unwrap_or_default(),
            deadline,
            cancel_grace: options.cancel_grace,
        }
    }

    /// Runs `fut` unless the run is cancelled or its deadline passes first.
    async fn guard<F: Future>(&self, fut: F) -> Result<F::Output, Stop> {
        let stop = async {
            match self.deadline {
                Some(deadline) => {
                    match race(
                        self.cancel.cancelled(),
                        tokio::time::sleep_until(deadline.at),
                    )
                    .await
                    {
                        Either::Left(()) => Stop::Cancelled,
                        Either::Right(()) => Stop::Deadline,
                    }
                }
                None => {
                    self.cancel.cancelled().await;
                    Stop::Cancelled
                }
            }
        };
        match race(stop, fut).await {
            Either::Left(stop) => Err(stop),
            Either::Right(output) => Ok(output),
        }
    }

    /// The error for a [`Stop`].
    fn stopped(&self, stop: Stop) -> RunErrorKind {
        match (stop, self.deadline) {
            (Stop::Deadline, Some(deadline)) => deadline.exceeded(),
            _ => RunErrorKind::Cancelled,
        }
    }

    /// `Some` if the run must not start another step.
    fn check_stop(&self) -> Option<RunErrorKind> {
        if self.cancel.is_cancelled() {
            return Some(RunErrorKind::Cancelled);
        }
        self.deadline
            .filter(|deadline| Instant::now() >= deadline.at)
            .map(Deadline::exceeded)
    }

    /// Emits an engine event. `kind` is only built if someone is listening.
    fn emit(&self, step: Option<usize>, node: Option<usize>, kind: impl FnOnce() -> EventKind) {
        if let Some(hub) = &self.hub
            && hub.is_enabled()
        {
            let scope = Scope {
                step,
                node: node.map(|id| self.graph.names[id].as_str().into()),
            };
            hub.emit(&scope, "internal", kind());
        }
    }

    /// Waits until the subscriber has received every event emitted so far.
    async fn flush(&self) {
        if let Some(hub) = &self.hub {
            hub.flush().await;
        }
    }

    fn names(&self, ids: &BTreeSet<usize>) -> Vec<String> {
        ids.iter().map(|&id| self.graph.names[id].clone()).collect()
    }

    async fn execute(
        mut self,
        mut frontier: BTreeSet<usize>,
        first_step: usize,
        mut resume: HashMap<usize, Value>,
        options: RunOptions,
        resumed: bool,
    ) -> Result<RunOutput, RunError> {
        let mut steps = Vec::new();
        let mut step = first_step;
        self.emit(None, None, || EventKind::RunStarted { resumed });

        while !frontier.is_empty() {
            // Step boundary: the only place a slow subscriber holds the run back.
            // Cancellation and the deadline still apply while waiting.
            let _ = self.guard(self.flush()).await;
            if let Some(kind) = self.check_stop() {
                return Err(self.fail(step, kind, steps.len()).await);
            }
            if step >= options.max_steps {
                let kind = RunErrorKind::StepLimitExceeded {
                    limit: options.max_steps,
                };
                return Err(self.fail(step, kind, steps.len()).await);
            }
            self.emit(Some(step), None, || EventKind::StepStarted {
                nodes: self.names(&frontier),
            });
            // Resume values apply only to the first step of a segment.
            let outcome = match self
                .step(step, &frontier, std::mem::take(&mut resume))
                .await
            {
                Ok(outcome) => outcome,
                Err(kind) => return Err(self.fail(step, kind, steps.len()).await),
            };
            steps.push(StepRecord {
                step,
                nodes: self.names(&frontier),
            });
            self.emit(Some(step), None, || EventKind::StepCommitted {
                next: self.names(&outcome.next),
            });
            step += 1;

            if !outcome.suspended.is_empty() {
                return Ok(self.suspend(step, outcome, steps).await);
            }
            frontier = outcome.next;
        }

        self.emit(None, None, || EventKind::RunFinished {
            status: "completed".into(),
            error: None,
            steps: steps.len(),
        });
        // Cancellation or the deadline also end the wait for a slow subscriber.
        let _ = self.guard(self.flush()).await;
        Ok(RunOutput {
            status: RunStatus::Completed,
            run_id: self.run_id.to_string(),
            state: into_state(self.state),
            steps,
        })
    }

    /// Runs one superstep. On success the step's updates and join progress
    /// are committed; on failure nothing is committed.
    async fn step(
        &mut self,
        step: usize,
        frontier: &BTreeSet<usize>,
        mut resume: HashMap<usize, Value>,
    ) -> Result<StepOutcome, RunErrorKind> {
        let graph = self.graph;
        let outputs = self.run_nodes(step, frontier, &mut resume).await?;

        let mut updates = Vec::with_capacity(outputs.len());
        let mut gotos: HashMap<usize, Vec<String>> = HashMap::new();
        let mut suspended = Vec::new();
        for (id, output) in outputs {
            let name = graph.names[id].clone();
            match output {
                NodeOutput::Update(update) => updates.push((name, update)),
                NodeOutput::Command { update, goto } => {
                    updates.push((name, update));
                    gotos.insert(id, goto);
                }
                NodeOutput::Suspend { update, payload } => {
                    updates.push((name, update));
                    suspended.push((id, payload));
                }
            }
        }

        // Merge into a copy so a failed step leaves no trace.
        let mut next_state = (*self.state).clone();
        reducer::merge(&mut next_state, &graph.reducers, updates)?;
        let next_state = Arc::new(next_state);

        let is_suspended = |id: usize| suspended.iter().any(|(s, _)| *s == id);
        let mut next = BTreeSet::new();
        for &id in frontier {
            if is_suspended(id) {
                continue; // continues on resume, not now
            }
            let node = || graph.names[id].clone();
            match &graph.outgoing[id] {
                Outgoing::Static(targets) => next.extend(targets.iter().filter_map(node_id)),
                Outgoing::Routed { router, routes } => {
                    let routed = self
                        .guard(router.route(next_state.clone()))
                        .await
                        .map_err(|stop| self.stopped(stop))?;
                    let label = routed.map_err(|source| RunErrorKind::RouterFailed {
                        node: node(),
                        source,
                    })?;
                    match routes.iter().find(|(l, _)| *l == label) {
                        Some((_, target)) => next.extend(node_id(target)),
                        None => {
                            return Err(RunErrorKind::UnknownRoute {
                                node: node(),
                                label,
                                expected: routes.iter().map(|(l, _)| l.clone()).collect(),
                            });
                        }
                    }
                }
            }

            let declared = &graph.goto[id];
            match gotos.remove(&id) {
                Some(targets) if !targets.is_empty() => {
                    for target in targets {
                        match declared.iter().find(|(name, _)| *name == target) {
                            Some((_, t)) => next.extend(node_id(t)),
                            None => {
                                return Err(RunErrorKind::UnknownRoute {
                                    node: node(),
                                    label: target,
                                    expected: declared.iter().map(|(n, _)| n.clone()).collect(),
                                });
                            }
                        }
                    }
                }
                _ => {
                    let only_goto = !declared.is_empty()
                        && matches!(&graph.outgoing[id], Outgoing::Static(t) if t.is_empty())
                        && graph.feeds_joins[id].is_empty();
                    if only_goto {
                        return Err(RunErrorKind::MissingGoto { node: node() });
                    }
                }
            }
        }

        let mut join_progress = self.join_progress.clone();
        for &id in frontier {
            if is_suspended(id) {
                continue; // a suspended node has not completed
            }
            for &j in &graph.feeds_joins[id] {
                join_progress[j].insert(id);
            }
        }
        for (j, join) in graph.joins.iter().enumerate() {
            if join_progress[j].len() == join.sources.len() {
                join_progress[j].clear();
                next.insert(join.target);
            }
        }

        self.state = next_state;
        self.join_progress = join_progress;
        Ok(StepOutcome { next, suspended })
    }

    /// Runs the frontier's nodes concurrently and returns their outputs in
    /// declaration order. Fails fast: the first error aborts the rest.
    async fn run_nodes(
        &self,
        step: usize,
        frontier: &BTreeSet<usize>,
        resume: &mut HashMap<usize, Value>,
    ) -> Result<Vec<(usize, NodeOutput)>, RunErrorKind> {
        // Parent of this step's node tokens: fired on any early exit, so
        // in-flight nodes (and language bridges) learn they must stop.
        let step_token = self.cancel.child();
        let mut tasks = JoinSet::new();
        let mut task_nodes = HashMap::new();
        let mut started = HashMap::new();
        for &id in frontier {
            let task = NodeTask {
                node: self.graph.nodes[id].clone(),
                state: self.state.clone(),
                policy: self.graph.policies[id].clone(),
                run_id: self.run_id.clone(),
                step,
                name: self.graph.names[id].as_str().into(),
                resume: resume.remove(&id),
                hub: self.hub.clone(),
                cancel: step_token.child(),
                run_deadline: self.deadline,
            };
            self.emit(Some(step), Some(id), || EventKind::NodeStarted {
                attempt: 1,
            });
            started.insert(id, Instant::now());
            let handle = tasks.spawn(async move { (id, task.execute().await) });
            task_nodes.insert(handle.id(), id);
        }
        let elapsed_ms = |id: usize| started[&id].elapsed().as_secs_f64() * 1000.0;

        let mut outputs = Vec::with_capacity(frontier.len());
        loop {
            let joined = match self.guard(tasks.join_next()).await {
                Ok(Some(joined)) => joined,
                Ok(None) => break,
                Err(stop) => {
                    step_token.cancel();
                    let reason = match stop {
                        Stop::Cancelled => {
                            // Grace period: nodes may finish cleaning up.
                            // Their results are discarded either way.
                            let drain = async { while tasks.join_next().await.is_some() {} };
                            let _ = tokio::time::timeout(self.cancel_grace, drain).await;
                            "run_cancelled"
                        }
                        Stop::Deadline => "deadline_exceeded",
                    };
                    self.emit_cancelled(step, frontier, &outputs, None, reason);
                    return Err(self.stopped(stop));
                }
            };
            let (id, failure) = match joined {
                Ok((id, Ok(output))) => {
                    self.emit(Some(step), Some(id), || {
                        let (update_keys, goto, suspended) = summarize(&output);
                        EventKind::NodeFinished {
                            duration_ms: elapsed_ms(id),
                            update_keys,
                            goto,
                            suspended,
                        }
                    });
                    outputs.push((id, output));
                    continue;
                }
                Ok((id, Err(failure))) => {
                    let node = self.graph.names[id].clone();
                    let kind = match failure {
                        Failure::Error(source) => RunErrorKind::NodeFailed { node, source },
                        Failure::Timeout(timeout_ms) => {
                            RunErrorKind::NodeTimeout { node, timeout_ms }
                        }
                        Failure::Deadline => self.stopped(Stop::Deadline),
                        Failure::Cancelled => RunErrorKind::Cancelled,
                    };
                    (id, kind)
                }
                Err(err) => {
                    let id = task_nodes[&err.id()];
                    let message = if err.is_panic() {
                        panic_message(err.into_panic())
                    } else {
                        "task was cancelled".to_string()
                    };
                    let node = self.graph.names[id].clone();
                    (id, RunErrorKind::NodePanicked { node, message })
                }
            };

            // Fail fast: the remaining nodes are aborted when `tasks` drops.
            step_token.cancel();
            self.emit(Some(step), Some(id), || EventKind::NodeFailed {
                duration_ms: elapsed_ms(id),
                code: failure.code().to_string(),
                message: match &failure {
                    RunErrorKind::NodeFailed { source, .. } => source.to_string(),
                    RunErrorKind::NodePanicked { message, .. } => message.clone(),
                    other => other.to_string(),
                },
            });
            self.emit_cancelled(step, frontier, &outputs, Some(id), "sibling_failed");
            return Err(failure);
        }
        outputs.sort_by_key(|(id, _)| *id);
        Ok(outputs)
    }

    /// `node_cancelled` for every node in `frontier` that neither finished
    /// nor is `failed`.
    fn emit_cancelled(
        &self,
        step: usize,
        frontier: &BTreeSet<usize>,
        finished: &[(usize, NodeOutput)],
        failed: Option<usize>,
        reason: &str,
    ) {
        for &id in frontier {
            if Some(id) != failed && !finished.iter().any(|(done, _)| *done == id) {
                self.emit(Some(step), Some(id), || EventKind::NodeCancelled {
                    reason: reason.into(),
                });
            }
        }
    }

    async fn suspend(
        self,
        next_step: usize,
        outcome: StepOutcome,
        steps: Vec<StepRecord>,
    ) -> RunOutput {
        let graph = self.graph;
        let mut frontier = outcome.next;
        frontier.extend(outcome.suspended.iter().map(|(id, _)| *id));

        let interrupts: Vec<Interrupt> = outcome
            .suspended
            .into_iter()
            .map(|(id, payload)| Interrupt {
                node: graph.names[id].clone(),
                payload,
            })
            .collect();
        let join_progress = graph
            .joins
            .iter()
            .zip(&self.join_progress)
            .filter(|(_, done)| !done.is_empty())
            .map(|(join, done)| {
                let mut sources: Vec<String> =
                    done.iter().map(|&s| graph.names[s].clone()).collect();
                sources.sort_unstable();
                (graph.names[join.target].clone(), sources)
            })
            .collect();

        self.emit(Some(next_step - 1), None, || EventKind::Suspended {
            interrupts: interrupts.clone(),
        });
        self.emit(None, None, || EventKind::RunFinished {
            status: "suspended".into(),
            error: None,
            steps: steps.len(),
        });
        let _ = self.guard(self.flush()).await;
        let event_seq = self
            .hub
            .as_ref()
            .map_or(self.first_seq, |hub| hub.next_seq());

        let state = into_state(self.state);
        let snapshot = Snapshot {
            format: SNAPSHOT_FORMAT.to_string(),
            workflow_fingerprint: graph.fingerprint.clone(),
            run_id: self.run_id.to_string(),
            step: next_step,
            state: state.clone(),
            frontier: frontier.iter().map(|&id| graph.names[id].clone()).collect(),
            join_progress,
            suspended: interrupts.clone(),
            event_seq,
        };
        RunOutput {
            status: RunStatus::Suspended {
                interrupts,
                snapshot: Box::new(snapshot),
            },
            run_id: self.run_id.to_string(),
            state,
            steps,
        }
    }

    async fn fail(self, step: usize, kind: RunErrorKind, steps: usize) -> RunError {
        self.emit(None, None, || EventKind::RunFinished {
            status: "failed".into(),
            error: Some(kind.code().to_string()),
            steps,
        });
        if matches!(
            kind,
            RunErrorKind::Cancelled | RunErrorKind::DeadlineExceeded { .. }
        ) {
            // The run is stopping: give a slow subscriber the grace period, no more.
            let _ = tokio::time::timeout(self.cancel_grace, self.flush()).await;
        } else {
            self.flush().await;
        }
        RunError {
            kind,
            step,
            state: into_state(self.state),
        }
    }
}

/// `(update keys, goto targets, suspended)` for a `node_finished` event.
fn summarize(output: &NodeOutput) -> (Vec<String>, Vec<String>, bool) {
    let keys = |update: &crate::state::Update| update.keys().cloned().collect();
    match output {
        NodeOutput::Update(update) => (keys(update), Vec::new(), false),
        NodeOutput::Command { update, goto } => (keys(update), goto.clone(), false),
        NodeOutput::Suspend { update, .. } => (keys(update), Vec::new(), true),
    }
}

/// Why a node execution ended without output.
enum Failure {
    Error(NodeError),
    Timeout(u64),
    /// The next retry would start after the run deadline.
    Deadline,
    Cancelled,
}

/// One node's execution within a step, including its retries.
struct NodeTask {
    node: Arc<dyn Node>,
    state: Arc<State>,
    policy: NodePolicy,
    run_id: Arc<str>,
    step: usize,
    name: Arc<str>,
    resume: Option<Value>,
    hub: Option<Arc<Hub>>,
    cancel: CancelToken,
    run_deadline: Option<Deadline>,
}

impl NodeTask {
    fn scope(&self) -> Scope {
        Scope {
            step: Some(self.step),
            node: Some(self.name.clone()),
        }
    }

    fn emit(&self, kind: impl FnOnce() -> EventKind) {
        if let Some(hub) = &self.hub
            && hub.is_enabled()
        {
            hub.emit(&self.scope(), "internal", kind());
        }
    }

    async fn execute(self) -> Result<NodeOutput, Failure> {
        let task_id = format!("{}:{}:{}", self.run_id, self.step, self.name);
        let mut attempt = 1;
        loop {
            if attempt > 1 {
                self.emit(|| EventKind::NodeStarted { attempt });
            }
            let attempt_end = self
                .policy
                .timeout_ms
                .map(|ms| Instant::now() + Duration::from_millis(ms));
            let run_end = self.run_deadline.map(|deadline| deadline.at);
            let ctx = NodeContext {
                run_id: self.run_id.clone(),
                step: self.step,
                node: self.name.clone(),
                attempt,
                resume: self.resume.clone(),
                events: EventSink::new(self.hub.clone(), self.scope()),
                cancel: self.cancel.clone(),
                deadline: match (attempt_end, run_end) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                },
            };
            let fut = self.node.run(ctx, self.state.clone());
            let result = match (attempt_end, self.policy.timeout_ms) {
                (Some(end), Some(ms)) => match tokio::time::timeout_at(end, fut).await {
                    Ok(result) => result,
                    // `run_nodes` fires this node's token (via the step token).
                    Err(_) => return Err(Failure::Timeout(ms)),
                },
                _ => fut.await,
            };
            let error = match result {
                Ok(output) => return Ok(output),
                Err(error) => error,
            };

            let retry = self.policy.retry.as_ref();
            let Some(retry) = retry.filter(|r| error.retryable && attempt < r.max_attempts) else {
                return Err(Failure::Error(error));
            };
            let delay = retry.delay(attempt, &task_id);
            if run_end.is_some_and(|end| Instant::now() + delay >= end) {
                return Err(Failure::Deadline);
            }
            self.emit(|| EventKind::AttemptFailed {
                attempt,
                code: "node_failed".into(),
                message: error.to_string(),
                retry_in_ms: u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            });
            if let Either::Left(()) = race(self.cancel.cancelled(), tokio::time::sleep(delay)).await
            {
                return Err(Failure::Cancelled);
            }
            attempt += 1;
        }
    }
}

enum Either<A, B> {
    Left(A),
    Right(B),
}

/// Waits for whichever future finishes first, preferring `a` when both are
/// ready. (A two-way `select!` without tokio's `macros` feature.)
async fn race<A: Future, B: Future>(a: A, b: B) -> Either<A::Output, B::Output> {
    let mut a = pin!(a);
    let mut b = pin!(b);
    poll_fn(|cx| {
        if let Poll::Ready(out) = a.as_mut().poll(cx) {
            return Poll::Ready(Either::Left(out));
        }
        if let Poll::Ready(out) = b.as_mut().poll(cx) {
            return Poll::Ready(Either::Right(out));
        }
        Poll::Pending
    })
    .await
}

fn into_state(state: Arc<State>) -> State {
    Arc::try_unwrap(state).unwrap_or_else(|shared| (*shared).clone())
}

fn node_id(target: &Target) -> Option<usize> {
    match target {
        Target::Node(id) => Some(*id),
        Target::End => None,
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}
