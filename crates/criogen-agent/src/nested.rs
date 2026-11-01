//! Agents inside other things (design 0004 §1): a node of a larger workflow,
//! or a tool of another agent.

use std::sync::Arc;

use criogen_core::{BoxFuture, Node, NodeContext, NodeError, NodeOutput, Snapshot, State, Update};
use criogen_providers::{Message, ToolSpec};
use serde_json::{Map, Value, json};

use crate::agent::{Agent, AgentError, AgentResult};
use crate::approval::Decision;
use crate::tool::{Tool, ToolContext, ToolError, ToolOutput};

/// Memo keys of an [`AgentNode`]: the nested run's snapshot while it waits
/// for approval, or after it failed.
const SUSPENDED: &str = "agent:suspended";
const FAILED: &str = "agent:failed";

/// The interrupt payload `kind` of an agent node waiting for approval.
pub const AGENT_APPROVAL_INTERRUPT: &str = "agent_approval";

/// An [`Agent`] as one node of a larger workflow. Built with
/// [`Agent::as_node`].
///
/// It reads the conversation from `messages_key` (a list of messages, or a
/// string taken as one user message), runs the agent, and writes the full
/// transcript back to `messages_key` and the output to `output_key`.
///
/// If the agent waits for approval, the workflow suspends with
/// `{"kind": "agent_approval", "requests": [...]}`; resume the workflow with
/// this node's value set to `{"<call_id>": <Decision>, ...}`. If the agent
/// fails, retrying the workflow retries the agent from its own failure
/// snapshot, so its finished tool calls are not repeated.
#[derive(Debug, Clone)]
pub struct AgentNode {
    agent: Agent,
    messages_key: String,
    output_key: Option<String>,
}

impl AgentNode {
    /// Reads and writes the transcript under `key` (default `messages`).
    pub fn messages_key(mut self, key: impl Into<String>) -> Self {
        self.messages_key = key.into();
        self
    }

    /// Writes the agent's output under `key` (default: not written).
    pub fn output_key(mut self, key: impl Into<String>) -> Self {
        self.output_key = Some(key.into());
        self
    }

    async fn execute(self, ctx: NodeContext, state: Arc<State>) -> Result<NodeOutput, NodeError> {
        let memo = ctx.memo().clone();
        let mut options = self
            .agent
            .run_options()
            .cancel_token(ctx.cancel_token().child());
        if let Some(left) = ctx.remaining_time() {
            options = options.deadline(left);
        }

        let saved = |key: &str| -> Option<Snapshot> {
            memo.get(key).and_then(|v| serde_json::from_value(v).ok())
        };
        let result = if let Some(failed) = saved(FAILED) {
            self.agent.retry_with(&failed, options).await
        } else if let (Some(paused), Some(answer)) = (saved(SUSPENDED), ctx.resume()) {
            let decisions: Map<String, Value> = answer.as_object().cloned().ok_or_else(|| {
                NodeError::new("resume with an object of decisions keyed by call id")
                    .with_kind("resume_invalid")
            })?;
            let decisions = decisions
                .into_iter()
                .map(|(id, d)| serde_json::from_value::<Decision>(d).map(|d| (id, d)))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| {
                    NodeError::new(format!("invalid decision: {err}")).with_kind("resume_invalid")
                })?;
            self.agent.resume_with(&paused, decisions, options).await
        } else {
            let input = match state.get(&self.messages_key) {
                Some(Value::String(text)) => vec![Message::user(text.clone())],
                Some(value) => {
                    serde_json::from_value::<Vec<Message>>(value.clone()).map_err(|err| {
                        NodeError::new(format!(
                            "`{}` is not a list of messages: {err}",
                            self.messages_key
                        ))
                        .with_kind("invalid_state")
                    })?
                }
                None => {
                    return Err(NodeError::new(format!(
                        "no conversation under `{}`",
                        self.messages_key
                    ))
                    .with_kind("invalid_state"));
                }
            };
            self.agent.run_with(input, options).await
        };

        match result {
            Ok(result) if result.is_suspended() => {
                let snapshot = result.snapshot().expect("suspended runs have a snapshot");
                memo.put(
                    SUSPENDED,
                    serde_json::to_value(snapshot).expect("snapshots serialize"),
                );
                Ok(NodeOutput::suspend(
                    Update::new(),
                    json!({
                        "kind": AGENT_APPROVAL_INTERRUPT,
                        "requests": result.approval_requests(),
                    }),
                ))
            }
            Ok(result) => Ok(self.write(result).into()),
            Err(error) => {
                if let Some(snapshot) = error.snapshot() {
                    memo.put(
                        FAILED,
                        serde_json::to_value(snapshot).expect("snapshots serialize"),
                    );
                }
                let mut node_error = NodeError::new(error.message).with_kind(error.code);
                if let Some(details) = error.details {
                    node_error = node_error.with_details(details);
                }
                Err(node_error)
            }
        }
    }

    fn write(&self, result: AgentResult) -> Update {
        let mut update = Update::new().set(
            self.messages_key.clone(),
            serde_json::to_value(&result.messages).expect("messages serialize"),
        );
        if let Some(key) = &self.output_key {
            update = update.set(key.clone(), result.output.unwrap_or(Value::Null));
        }
        update
    }
}

impl Node for AgentNode {
    fn run(
        &self,
        ctx: NodeContext,
        state: Arc<State>,
    ) -> BoxFuture<'static, Result<NodeOutput, NodeError>> {
        Box::pin(self.clone().execute(ctx, state))
    }
}

/// An [`Agent`] as a tool of another agent. Built with [`Agent::as_tool`].
///
/// The tool takes `{"input": "<task>"}`. The sub-agent's output (text or
/// JSON) is the result. If the sub-agent stops on a limit, invalid output,
/// a deadline or a transient model failure, the calling model gets a tool
/// error and can react; configuration problems (authentication, a broken
/// tool or hook) fail the calling run (0003 E16, 0004 A12).
#[derive(Debug, Clone)]
pub struct AgentTool {
    agent: Agent,
    spec: ToolSpec,
}

impl Tool for AgentTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn call(
        &self,
        args: Map<String, Value>,
        ctx: ToolContext,
    ) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            let input = args
                .get("input")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let options = self.agent.run_options().cancel_token(ctx.cancel.child());
            match self.agent.run_at_depth(input, ctx.depth + 1, options).await {
                Ok(result) if result.is_suspended() => Err(ToolError::fatal(
                    "the sub-agent needs approval; use it as a workflow node \
                     (Agent::as_node) to pause for a reviewer",
                )),
                Ok(result) => Ok(match result.output {
                    Some(Value::String(text)) => ToolOutput::Text(text),
                    Some(value) => ToolOutput::Json(value),
                    None => ToolOutput::Text(
                        result
                            .messages
                            .last()
                            .map(Message::text)
                            .unwrap_or_default(),
                    ),
                }),
                Err(error) => Err(tool_error(error)),
            }
        })
    }
}

/// Which sub-agent failures the calling model may react to.
fn tool_error(error: AgentError) -> ToolError {
    let message = format!("sub-agent stopped: {}", error.message);
    match error.code.as_str() {
        "limit_exceeded" | "output_invalid" | "deadline_exceeded" | "cancelled" => {
            ToolError::new(message)
        }
        "model_failed" => {
            let transient = error
                .details
                .as_ref()
                .and_then(|d| d["model_error"]["kind"].as_str())
                .is_some_and(|kind| {
                    matches!(
                        kind,
                        "rate_limited" | "overloaded" | "server" | "timeout" | "network"
                    )
                });
            if transient {
                ToolError::new(message).retryable()
            } else {
                ToolError::fatal(message)
            }
        }
        _ => ToolError::fatal(message),
    }
}

impl Agent {
    /// This agent as a node of a larger workflow (see [`AgentNode`]).
    pub fn as_node(&self) -> AgentNode {
        AgentNode {
            agent: self.clone(),
            messages_key: crate::agent::MESSAGES.to_string(),
            output_key: None,
        }
    }

    /// This agent as a tool another agent can call (see [`AgentTool`]).
    pub fn as_tool(&self, name: impl Into<String>, description: impl Into<String>) -> AgentTool {
        AgentTool {
            agent: self.clone(),
            spec: ToolSpec::new(
                name,
                description,
                json!({
                    "type": "object",
                    "properties": {
                        "input": {"type": "string", "description": "The task, in plain language."}
                    },
                    "required": ["input"],
                    "additionalProperties": false
                }),
            ),
        }
    }
}
