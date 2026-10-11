use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task::JoinSet;

use crate::error::{RunError, RunErrorKind};
use crate::graph::{Graph, Outgoing, Target, Workflow};
use crate::node::{NodeContext, NodeOutput};
use crate::reducer;
use crate::state::State;

/// The `format` value of snapshots produced by this version.
pub const SNAPSHOT_FORMAT: &str = "agentbuild.snapshot/1";

/// Per-run settings.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunOptions {
    /// Maximum step index (counted from the start of the run, across resumes)
    /// before the run fails with [`RunErrorKind::StepLimitExceeded`]. Guards
    /// against unbounded cycles.
    pub max_steps: usize,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self { max_steps: 64 }
    }
}

impl RunOptions {
    /// Sets [`max_steps`](Self::max_steps).
    pub fn max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
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
            RunStatus::Suspended { snapshot, .. } => Some(snapshot),
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
        /// Everything needed to continue the run later.
        snapshot: Snapshot,
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
        let run = Run::new(&self.graph, uuid::Uuid::now_v7().to_string(), input);
        let start = BTreeSet::from([self.graph.entry]);
        run.execute(start, 0, HashMap::new(), options).await
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

        let mut run = Run::new(graph, snapshot.run_id.clone(), snapshot.state.clone());
        run.join_progress = join_progress;
        run.execute(frontier, snapshot.step, resume, options).await
    }
}

struct Run<'g> {
    graph: &'g Graph,
    run_id: Arc<str>,
    state: Arc<State>,
    /// For each join, the sources that have completed since it last fired.
    join_progress: Vec<HashSet<usize>>,
}

struct StepOutcome {
    next: BTreeSet<usize>,
    suspended: Vec<(usize, Value)>,
}

#[allow(clippy::result_large_err)]
impl<'g> Run<'g> {
    fn new(graph: &'g Graph, run_id: String, state: State) -> Self {
        Self {
            graph,
            run_id: run_id.into(),
            state: Arc::new(state),
            join_progress: vec![HashSet::new(); graph.joins.len()],
        }
    }

    async fn execute(
        mut self,
        mut frontier: BTreeSet<usize>,
        first_step: usize,
        mut resume: HashMap<usize, Value>,
        options: RunOptions,
    ) -> Result<RunOutput, RunError> {
        let mut steps = Vec::new();
        let mut step = first_step;

        while !frontier.is_empty() {
            if step >= options.max_steps {
                return Err(self.fail(
                    step,
                    RunErrorKind::StepLimitExceeded {
                        limit: options.max_steps,
                    },
                ));
            }
            // Resume values apply only to the first step of a segment.
            let outcome = match self
                .step(step, &frontier, std::mem::take(&mut resume))
                .await
            {
                Ok(outcome) => outcome,
                Err(kind) => return Err(self.fail(step, kind)),
            };
            steps.push(StepRecord {
                step,
                nodes: frontier
                    .iter()
                    .map(|&id| self.graph.names[id].clone())
                    .collect(),
            });
            step += 1;

            if !outcome.suspended.is_empty() {
                return Ok(self.suspend(step, outcome, steps));
            }
            frontier = outcome.next;
        }

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
                    let label = router.route(next_state.clone()).await.map_err(|source| {
                        RunErrorKind::RouterFailed {
                            node: node(),
                            source,
                        }
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
        let mut tasks = JoinSet::new();
        let mut task_nodes = HashMap::new();
        for &id in frontier {
            let ctx = NodeContext {
                run_id: self.run_id.clone(),
                step,
                node: self.graph.names[id].as_str().into(),
                attempt: 1,
                resume: resume.remove(&id),
            };
            let fut = self.graph.nodes[id].run(ctx, self.state.clone());
            let handle = tasks.spawn(async move { (id, fut.await) });
            task_nodes.insert(handle.id(), id);
        }

        let mut outputs = Vec::with_capacity(frontier.len());
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok((id, Ok(output))) => outputs.push((id, output)),
                Ok((id, Err(source))) => {
                    return Err(RunErrorKind::NodeFailed {
                        node: self.graph.names[id].clone(),
                        source,
                    });
                }
                Err(err) => {
                    let node = self.graph.names[task_nodes[&err.id()]].clone();
                    let message = if err.is_panic() {
                        panic_message(err.into_panic())
                    } else {
                        "task was cancelled".to_string()
                    };
                    return Err(RunErrorKind::NodePanicked { node, message });
                }
            }
        }
        outputs.sort_by_key(|(id, _)| *id);
        Ok(outputs)
    }

    fn suspend(self, next_step: usize, outcome: StepOutcome, steps: Vec<StepRecord>) -> RunOutput {
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
        };
        RunOutput {
            status: RunStatus::Suspended {
                interrupts,
                snapshot,
            },
            run_id: self.run_id.to_string(),
            state,
            steps,
        }
    }

    fn fail(self, step: usize, kind: RunErrorKind) -> RunError {
        RunError {
            kind,
            step,
            state: into_state(self.state),
        }
    }
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
