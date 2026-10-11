//! Execution engine for AgentBuild workflows.
//!
//! A [`Workflow`] is a validated graph of named [`Node`]s. It runs in
//! *supersteps*: every node scheduled for a step runs concurrently against the
//! same [`State`] snapshot, their [`Update`]s are merged atomically, and the
//! graph's edges decide which nodes run in the next step. See
//! `docs/design/0001-execution-model.md` for the full contract.
//!
//! ```
//! use agentbuild_core::{END, State, Update, WorkflowBuilder, node_fn};
//!
//! # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
//! let mut builder = WorkflowBuilder::new();
//! builder
//!     .add_node("greet", node_fn(|state| async move {
//!         let name = state.get("name").and_then(|v| v.as_str()).unwrap_or("world");
//!         Ok(Update::new().set("greeting", format!("hello, {name}")))
//!     }))
//!     .set_entry("greet")
//!     .add_edge("greet", END);
//!
//! let workflow = builder.build().unwrap();
//! let output = workflow.run(State::new().with("name", "ada")).await.unwrap();
//! assert_eq!(output.state.get("greeting").unwrap(), "hello, ada");
//! # });
//! ```

mod cancel;
mod error;
mod event;
mod graph;
mod memo;
mod node;
mod policy;
mod reducer;
mod runtime;
mod state;

pub use cancel::CancelToken;
pub use error::{BuildError, NodeError, NotAnObject, RunError, RunErrorKind, ValidationIssue};
pub use event::{Event, EventKind, EventReceiver, EventSender, EventSink, event_channel};
pub use graph::{Workflow, WorkflowBuilder};
pub use memo::TaskMemo;
pub use node::{
    BoxFuture, Node, NodeContext, NodeOutput, Router, node_fn, node_with_context, router_fn,
};
pub use policy::{NodePolicy, RetryPolicy};
pub use reducer::Reducer;
pub use runtime::{
    Interrupt, PendingWrite, RunOptions, RunOutput, RunStatus, RunStream, SNAPSHOT_FORMAT,
    Snapshot, SnapshotError, StepRecord,
};
pub use state::{State, Update};

// Compiles and runs the README's example with the doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

/// Target name that terminates a path through the workflow.
///
/// Names starting with `__` are reserved, so this cannot clash with a node.
pub const END: &str = "__end__";
