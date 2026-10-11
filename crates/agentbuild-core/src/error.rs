use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::State;

/// An error raised by user code inside a node or router.
///
/// It is deliberately language-neutral so bindings can carry, for example, a
/// Python exception's class name in `kind` and its attributes in `details`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeError {
    /// Human-readable description.
    pub message: String,
    /// Optional machine-readable category (e.g. `"ValueError"`, `"timeout"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Optional structured context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl NodeError {
    /// Creates an error with only a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: None,
            details: None,
        }
    }

    /// Sets the machine-readable category.
    pub fn with_kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }

    /// Attaches structured context.
    pub fn with_details(mut self, details: impl Into<Value>) -> Self {
        self.details = Some(details.into());
        self
    }
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            Some(kind) => write!(f, "{kind}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for NodeError {}

impl From<String> for NodeError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<&str> for NodeError {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

/// Returned when a JSON value that must be an object is something else.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("expected a JSON object, got {0}")]
pub struct NotAnObject(&'static str);

impl NotAnObject {
    pub(crate) fn of(value: &Value) -> Self {
        Self(match value {
            Value::Null => "null",
            Value::Bool(_) => "a boolean",
            Value::Number(_) => "a number",
            Value::String(_) => "a string",
            Value::Array(_) => "an array",
            Value::Object(_) => "an object",
        })
    }
}

/// A single problem found while validating a workflow graph.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ValidationIssue {
    /// The workflow has no nodes.
    #[error("workflow has no nodes")]
    NoNodes,
    /// `set_entry` was never called.
    #[error("no entry node; call set_entry")]
    MissingEntry,
    /// A node was given an empty name.
    #[error("node names must not be empty")]
    EmptyName,
    /// A node name starts with the reserved `__` prefix.
    #[error("node name `{0}` is reserved (names starting with `__` are reserved)")]
    ReservedName(String),
    /// Two nodes share a name.
    #[error("node `{0}` is defined more than once")]
    DuplicateNode(String),
    /// An edge, route, join, or the entry refers to a node that does not exist.
    #[error("{context} refers to unknown node `{node}`")]
    UnknownNode {
        /// The missing name.
        node: String,
        /// Where it was referenced.
        context: String,
    },
    /// A node has no way to continue or end.
    #[error("node `{0}` has no outgoing edges; add an edge to END if it should finish the run")]
    DeadEnd(String),
    /// A node can never be scheduled.
    #[error("node `{0}` is not reachable from the entry node")]
    Unreachable(String),
    /// More than one router was attached to a node.
    #[error("node `{0}` has more than one router")]
    MultipleRouters(String),
    /// A node has both a router and static edges.
    #[error("node `{0}` has both a router and static edges; use one or the other")]
    MixedEdges(String),
    /// A router declares no routes.
    #[error("router on `{0}` declares no routes")]
    EmptyRoutes(String),
    /// A router declares the same label twice.
    #[error("router on `{node}` declares label `{label}` more than once")]
    DuplicateRouteLabel {
        /// The routed node.
        node: String,
        /// The repeated label.
        label: String,
    },
    /// A join declares no sources.
    #[error("join into `{0}` declares no sources")]
    EmptyJoin(String),
    /// A join target is also scheduled some other way, which would make the
    /// "wait for all sources" guarantee meaningless.
    #[error(
        "join target `{0}` must not be the entry or the target of another edge, route, or join"
    )]
    JoinTargetHasOtherInbound(String),
}

/// Returned by [`WorkflowBuilder::build`](crate::WorkflowBuilder::build).
/// Lists every issue found, not just the first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildError {
    /// All validation issues, in a stable order.
    pub issues: Vec<ValidationIssue>,
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid workflow ({} issue(s))", self.issues.len())?;
        for issue in &self.issues {
            write!(f, "\n  - {issue}")?;
        }
        Ok(())
    }
}

impl std::error::Error for BuildError {}

/// Why a run failed. Each variant has a stable [`code`](Self::code).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum RunErrorKind {
    /// A node returned an error.
    #[error("node `{node}` failed: {source}")]
    NodeFailed {
        /// The failing node.
        node: String,
        /// The error it returned.
        source: NodeError,
    },
    /// A node panicked.
    #[error("node `{node}` panicked: {message}")]
    NodePanicked {
        /// The panicking node.
        node: String,
        /// The panic payload, if it was a string.
        message: String,
    },
    /// A router returned an error.
    #[error("router on `{node}` failed: {source}")]
    RouterFailed {
        /// The routed node.
        node: String,
        /// The error it returned.
        source: NodeError,
    },
    /// A router returned a label it did not declare.
    #[error("router on `{node}` returned unknown label `{label}` (expected one of {expected:?})")]
    UnknownRoute {
        /// The routed node.
        node: String,
        /// The label returned.
        label: String,
        /// The declared labels.
        expected: Vec<String>,
    },
    /// Two nodes in the same step wrote the same key.
    #[error("nodes `{first}` and `{second}` both wrote `{key}` in the same step")]
    UpdateConflict {
        /// The contested key.
        key: String,
        /// The first writer, in declaration order.
        first: String,
        /// The second writer.
        second: String,
    },
    /// The run did not finish within `max_steps`.
    #[error("run did not finish within {limit} steps")]
    StepLimitExceeded {
        /// The configured limit.
        limit: usize,
    },
}

impl RunErrorKind {
    /// A stable, snake_case identifier for this error, for SDKs and logs.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NodeFailed { .. } => "node_failed",
            Self::NodePanicked { .. } => "node_panicked",
            Self::RouterFailed { .. } => "router_failed",
            Self::UnknownRoute { .. } => "unknown_route",
            Self::UpdateConflict { .. } => "update_conflict",
            Self::StepLimitExceeded { .. } => "step_limit_exceeded",
        }
    }
}

/// A failed run.
///
/// Steps are all-or-nothing, so `state` is exactly the state after the last
/// step that completed, and `step` is the index of the step that failed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("step {step}: {kind}")]
pub struct RunError {
    /// What went wrong.
    #[source]
    pub kind: RunErrorKind,
    /// Zero-based index of the step that failed.
    pub step: usize,
    /// State as of the last completed step.
    pub state: State,
}

impl RunError {
    /// Shorthand for `self.kind.code()`.
    pub fn code(&self) -> &'static str {
        self.kind.code()
    }
}
