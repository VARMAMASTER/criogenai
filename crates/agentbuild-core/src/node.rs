use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::error::NodeError;
use crate::state::{State, Update};

/// A boxed, `Send` future. Used so [`Node`] and [`Router`] stay object-safe.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A unit of work in a workflow.
///
/// This trait is the boundary between the runtime and user code: language
/// bindings implement it by calling into Python or JavaScript. Most Rust code
/// should use [`node_fn`] instead of implementing it by hand.
///
/// Nodes run on the Tokio runtime and must not block. If the run is dropped or
/// fails, in-flight nodes are cancelled at their next `.await` point.
pub trait Node: Send + Sync + 'static {
    /// Runs the node against a read-only snapshot of the state.
    fn run(&self, state: Arc<State>) -> BoxFuture<'static, Result<Update, NodeError>>;
}

/// Picks the next step after a node by returning one of its declared labels.
///
/// Routers see the state *after* the step's updates have been merged.
pub trait Router: Send + Sync + 'static {
    /// Returns a label declared in [`WorkflowBuilder::add_router`](crate::WorkflowBuilder::add_router).
    fn route(&self, state: Arc<State>) -> BoxFuture<'static, Result<String, NodeError>>;
}

/// Adapts an async closure into a [`Node`].
pub fn node_fn<F, Fut>(f: F) -> impl Node
where
    F: Fn(Arc<State>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Update, NodeError>> + Send + 'static,
{
    FnNode(f)
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
    fn run(&self, state: Arc<State>) -> BoxFuture<'static, Result<Update, NodeError>> {
        Box::pin((self.0)(state))
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
