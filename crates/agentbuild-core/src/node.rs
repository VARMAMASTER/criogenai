use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use crate::error::NodeError;
use crate::state::{State, Update};

/// A boxed, `Send` future. Used so [`Node`] and [`Router`] stay object-safe.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Information about the current execution, passed to every node.
///
/// Fields are read through methods so new ones (cancellation, deadline,
/// events) can be added without breaking node code.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct NodeContext {
    pub(crate) run_id: Arc<str>,
    pub(crate) step: usize,
    pub(crate) node: Arc<str>,
    pub(crate) attempt: u32,
    pub(crate) resume: Option<Value>,
}

impl NodeContext {
    /// The run this execution belongs to (a time-ordered UUID). Stable across
    /// suspend and resume.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Zero-based superstep index.
    pub fn step(&self) -> usize {
        self.step
    }

    /// The name of the node being executed.
    pub fn node(&self) -> &str {
        &self.node
    }

    /// Deterministic id for this execution: `"{run_id}:{step}:{node}"`.
    pub fn task_id(&self) -> String {
        format!("{}:{}:{}", self.run_id, self.step, self.node)
    }

    /// 1-based attempt number. Always 1 until retry policies exist.
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// The value supplied to [`Workflow::resume`](crate::Workflow::resume) if
    /// this node suspended in the previous segment of the run.
    pub fn resume(&self) -> Option<&Value> {
        self.resume.as_ref()
    }
}

/// What a node produces.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum NodeOutput {
    /// Change the state; outgoing edges decide what runs next.
    Update(Update),
    /// Change the state and additionally schedule `goto` targets. Targets must
    /// be declared with [`WorkflowBuilder::may_goto`](crate::WorkflowBuilder::may_goto).
    Command {
        /// State changes.
        update: Update,
        /// Node names (or [`END`](crate::END)) to schedule in the next step.
        goto: Vec<String>,
    },
    /// Commit `update`, then pause the run and return `payload` to the caller.
    /// The node is invoked again on resume with
    /// [`NodeContext::resume`] set. A node must not cause side effects before
    /// suspending.
    Suspend {
        /// State changes committed with the suspension.
        update: Update,
        /// Information for whoever resumes the run (e.g. an approval request).
        payload: Value,
    },
}

impl NodeOutput {
    /// Shorthand for [`NodeOutput::Command`].
    pub fn goto<I, S>(update: Update, targets: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::Command {
            update,
            goto: targets.into_iter().map(Into::into).collect(),
        }
    }

    /// Shorthand for [`NodeOutput::Suspend`].
    pub fn suspend(update: Update, payload: impl Into<Value>) -> Self {
        Self::Suspend {
            update,
            payload: payload.into(),
        }
    }
}

impl From<Update> for NodeOutput {
    fn from(update: Update) -> Self {
        Self::Update(update)
    }
}

/// A unit of work in a workflow.
///
/// This trait is the boundary between the runtime and user code: language
/// bindings implement it by calling into Python or JavaScript. Most Rust code
/// should use [`node_fn`] or [`node_with_context`] instead.
///
/// Nodes run on the Tokio runtime and must not block. If the run is dropped or
/// fails, in-flight nodes are cancelled at their next `.await` point.
pub trait Node: Send + Sync + 'static {
    /// Runs the node against a read-only snapshot of the state.
    fn run(
        &self,
        ctx: NodeContext,
        state: Arc<State>,
    ) -> BoxFuture<'static, Result<NodeOutput, NodeError>>;
}

/// Picks the next step after a node by returning one of its declared labels.
///
/// Routers see the state *after* the step's updates have been merged.
pub trait Router: Send + Sync + 'static {
    /// Returns a label declared in [`WorkflowBuilder::add_router`](crate::WorkflowBuilder::add_router).
    fn route(&self, state: Arc<State>) -> BoxFuture<'static, Result<String, NodeError>>;
}

/// Adapts an async closure that returns an [`Update`] into a [`Node`].
pub fn node_fn<F, Fut>(f: F) -> impl Node
where
    F: Fn(Arc<State>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Update, NodeError>> + Send + 'static,
{
    FnNode(f)
}

/// Adapts an async closure that receives the [`NodeContext`] and returns a
/// [`NodeOutput`] into a [`Node`]. Use it for `Command`, `Suspend`, or when the
/// node needs run information.
pub fn node_with_context<F, Fut>(f: F) -> impl Node
where
    F: Fn(NodeContext, Arc<State>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<NodeOutput, NodeError>> + Send + 'static,
{
    CtxNode(f)
}

/// Adapts an async closure into a [`Router`].
pub fn router_fn<F, Fut, L>(f: F) -> impl Router
where
    F: Fn(Arc<State>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<L, NodeError>> + Send + 'static,
    L: Into<String>,
{
    FnRouter(f)
}

struct FnNode<F>(F);

impl<F, Fut> Node for FnNode<F>
where
    F: Fn(Arc<State>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Update, NodeError>> + Send + 'static,
{
    fn run(
        &self,
        _ctx: NodeContext,
        state: Arc<State>,
    ) -> BoxFuture<'static, Result<NodeOutput, NodeError>> {
        let fut = (self.0)(state);
        Box::pin(async move { fut.await.map(NodeOutput::Update) })
    }
}

struct CtxNode<F>(F);

impl<F, Fut> Node for CtxNode<F>
where
    F: Fn(NodeContext, Arc<State>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<NodeOutput, NodeError>> + Send + 'static,
{
    fn run(
        &self,
        ctx: NodeContext,
        state: Arc<State>,
    ) -> BoxFuture<'static, Result<NodeOutput, NodeError>> {
        Box::pin((self.0)(ctx, state))
    }
}

struct FnRouter<F>(F);

impl<F, Fut, L> Router for FnRouter<F>
where
    F: Fn(Arc<State>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<L, NodeError>> + Send + 'static,
    L: Into<String>,
{
    fn route(&self, state: Arc<State>) -> BoxFuture<'static, Result<String, NodeError>> {
        let fut = (self.0)(state);
        Box::pin(async move { fut.await.map(Into::into) })
    }
}
