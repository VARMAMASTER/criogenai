use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime::Snapshot;
use crate::state::State;

/// An error raised by user code inside a node or router.
///
/// It is deliberately language-neutral so bindings can carry, for example, a
/// Python exception's class name in `kind` and its attributes in `details`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct NodeError {
    /// Human-readable description.
    pub message: String,
    /// Optional machine-readable category (e.g. `"ValueError"`, `"timeout"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Optional structured context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// `true` if trying again may succeed (e.g. a network hiccup). Only
    /// retryable errors are retried by a node's
    /// [`RetryPolicy`](crate::RetryPolicy).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retryable: bool,
}

impl NodeError {
    /// Creates an error with only a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: None,
            details: None,
            retryable: false,
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

    /// Marks the error as safe to retry.
    pub fn retryable(mut self) -> Self {
        self.retryable = true;
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
    /// A node declares `may_goto` targets and also has a router.
    #[error("node `{0}` has both a router and `may_goto` targets; use one or the other")]
    GotoWithRouter(String),
    /// A state key's reducer is declared more than once.
    #[error("reducer for state key `{0}` is declared more than once")]
    DuplicateReducer(String),
    /// A node's policy is declared more than once.
    #[error("policy for node `{0}` is declared more than once")]
    DuplicatePolicy(String),
    /// A policy has an impossible setting.
    #[error("policy for node `{node}` is invalid: {reason}")]
    InvalidPolicy {
        /// The node.
        node: String,
        /// What is wrong.
        reason: String,
    },
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
    /// A write could not be applied by the key's reducer.
    #[error("node `{node}` wrote an invalid value to `{key}`: {message}")]
    InvalidValue {
        /// The writing node.
        node: String,
        /// The state key.
        key: String,
        /// What was wrong.
        message: String,
    },
    /// A node that can only continue through `goto` returned no target.
    #[error("node `{node}` has no outgoing edges and returned no `goto` target")]
    MissingGoto {
        /// The node.
        node: String,
    },
    /// Resume input does not match the snapshot.
    #[error("cannot resume: {reason}")]
    ResumeInvalid {
        /// What was wrong.
        reason: String,
    },
    /// A node attempt exceeded its policy's timeout.
    #[error("node `{node}` timed out after {timeout_ms} ms")]
    NodeTimeout {
        /// The node.
        node: String,
        /// The configured per-attempt limit.
        timeout_ms: u64,
    },
    /// The run's deadline passed.
    #[error("run exceeded its deadline of {deadline_ms} ms")]
    DeadlineExceeded {
        /// The configured deadline.
        deadline_ms: u64,
    },
    /// The run was cancelled through its [`CancelToken`](crate::CancelToken).
    #[error("run was cancelled")]
    Cancelled,
    /// The snapshot was taken from a workflow with a different structure.
    #[error(
        "snapshot was taken from a different workflow (expected fingerprint {expected}, found {found})"
    )]
    WorkflowMismatch {
        /// This workflow's fingerprint.
        expected: String,
        /// The snapshot's fingerprint.
        found: String,
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
            Self::InvalidValue { .. } => "invalid_value",
            Self::MissingGoto { .. } => "missing_goto",
            Self::ResumeInvalid { .. } => "resume_invalid",
            Self::WorkflowMismatch { .. } => "workflow_mismatch",
            Self::NodeTimeout { .. } => "node_timeout",
            Self::DeadlineExceeded { .. } => "deadline_exceeded",
            Self::Cancelled => "cancelled",
        }
    }
}

/// A failed run.
///
/// Steps are all-or-nothing, so `state` is exactly the state after the last
/// step that completed, and `step` is the index of the step that failed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("step {step}: {kind}")]
#[non_exhaustive]
pub struct RunError {
    /// What went wrong.
    #[source]
    pub kind: RunErrorKind,
    /// Zero-based index of the step that failed.
    pub step: usize,
    /// State as of the last completed step.
    pub state: State,
    /// Everything needed to continue the run after fixing the cause, with
    /// [`Workflow::retry`](crate::Workflow::retry). Finished nodes of the
    /// failed step are not run again. `None` only when resume input was
    /// rejected before anything ran (the caller still has its snapshot).
    pub snapshot: Option<Box<Snapshot>>,
}

impl RunError {
    /// Shorthand for `self.kind.code()`.
    pub fn code(&self) -> &'static str {
        self.kind.code()
    }

    /// Shorthand for `self.snapshot.as_deref()`.
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_deref()
    }
}
