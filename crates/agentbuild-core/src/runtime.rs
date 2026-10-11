use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use serde::Serialize;
use tokio::task::JoinSet;

use crate::error::{RunError, RunErrorKind};
use crate::graph::{Graph, Outgoing, Target, Workflow};
use crate::state::{State, Update};

/// Per-run settings.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunOptions {
    /// Maximum number of steps before the run fails with
    /// [`RunErrorKind::StepLimitExceeded`]. Guards against unbounded cycles.
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

/// The result of a successful run.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct RunOutput {
    /// Final state.
    pub state: State,
    /// Which nodes ran in each step, in order.
    pub steps: Vec<StepRecord>,
}

/// The nodes that ran in one step, in declaration order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct StepRecord {
    /// Zero-based step index.
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
        Run {
            graph: &self.graph,
            state: Arc::new(input),
            join_progress: vec![HashSet::new(); self.graph.joins.len()],
        }
        .execute(options)
        .await
    }
}

struct Run<'g> {
    graph: &'g Graph,
    state: Arc<State>,
    /// For each join, the sources that have completed since it last fired.
    join_progress: Vec<HashSet<usize>>,
}

#[allow(clippy::result_large_err)]
impl Run<'_> {
    async fn execute(mut self, options: RunOptions) -> Result<RunOutput, RunError> {
        // BTreeSet keeps the frontier deduplicated and in declaration order.
        let mut frontier = BTreeSet::from([self.graph.entry]);
        let mut steps = Vec::new();

        while !frontier.is_empty() {
            let step = steps.len();
            if step >= options.max_steps {
                return Err(self.fail(
                    step,
                    RunErrorKind::StepLimitExceeded {
                        limit: options.max_steps,
                    },
                ));
            }
            match self.step(&frontier).await {
                Ok(next) => {
                    let nodes = frontier
                        .iter()
                        .map(|&id| self.graph.names[id].clone())
                        .collect();
                    steps.push(StepRecord { step, nodes });
                    frontier = next;
                }
                Err(kind) => return Err(self.fail(step, kind)),
            }
        }

        let state = Arc::try_unwrap(self.state).unwrap_or_else(|shared| (*shared).clone());
        Ok(RunOutput { state, steps })
    }

    /// Runs one superstep. On success the step's updates and join progress
    /// are committed and the next frontier is returned; on failure nothing
    /// is committed.
    async fn step(&mut self, frontier: &BTreeSet<usize>) -> Result<BTreeSet<usize>, RunErrorKind> {
        let graph = self.graph;
        let updates = self.run_nodes(frontier).await?;

        // Detect conflicts before touching state so a failed step leaves no trace.
        let mut writers: HashMap<&str, usize> = HashMap::new();
        for (id, update) in &updates {
            for key in update.keys() {
                if let Some(first) = writers.insert(key, *id) {
                    return Err(RunErrorKind::UpdateConflict {
                        key: key.clone(),
                        first: graph.names[first].clone(),
                        second: graph.names[*id].clone(),
                    });
                }
            }
        }
        let mut next_state = (*self.state).clone();
        for (_, update) in updates {
            next_state.apply(update);
        }
        let next_state = Arc::new(next_state);

        let mut next = BTreeSet::new();
        for &id in frontier {
            match &graph.outgoing[id] {
                Outgoing::Static(targets) => next.extend(targets.iter().filter_map(node_id)),
                Outgoing::Routed { router, routes } => {
                    let node = || graph.names[id].clone();
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
        }

        let mut join_progress = self.join_progress.clone();
        for &id in frontier {
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
        Ok(next)
    }

    /// Runs the frontier's nodes concurrently and returns their updates in
    /// declaration order. Fails fast: the first error aborts the rest.
    async fn run_nodes(
        &self,
        frontier: &BTreeSet<usize>,
    ) -> Result<Vec<(usize, Update)>, RunErrorKind> {
        let mut tasks = JoinSet::new();
        let mut task_nodes = HashMap::new();
        for &id in frontier {
            let fut = self.graph.nodes[id].run(self.state.clone());
            let handle = tasks.spawn(async move { (id, fut.await) });
            task_nodes.insert(handle.id(), id);
        }

        let mut updates = Vec::with_capacity(frontier.len());
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok((id, Ok(update))) => updates.push((id, update)),
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
        updates.sort_by_key(|(id, _)| *id);
        Ok(updates)
    }

    fn fail(self, step: usize, kind: RunErrorKind) -> RunError {
        let state = Arc::try_unwrap(self.state).unwrap_or_else(|shared| (*shared).clone());
        RunError { kind, step, state }
    }
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
