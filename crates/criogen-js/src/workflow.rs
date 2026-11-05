//! Workflows from TypeScript (design 0015): JavaScript nodes and routers,
//! agents as nodes, runs, resumes and threads.

use std::sync::Arc;
use std::time::Duration;

use criogen_core::{
    CancelToken, NodeContext, NodeError, NodeOutput, Reducer, RunError, RunErrorKind, RunOptions,
    RunOutput, Snapshot, State, Update, Workflow, WorkflowBuilder, node_with_context, router_fn,
};
use criogen_storage::{Storage, ThreadError, Turn, WorkflowThread};
use serde_json::{Value, json};

use crate::agent::agent_by_handle;
use crate::{
    Failure, JsFn, Registered, Result, bad, call_js, failure, parse, strings, text, to_json,
};

/// A JavaScript function as a node. Envelopes: `{"update"}`,
/// `{"suspend", "update"}`, `{"goto", "update"}` or `{"crash"}`.
fn js_node(name: String, function: JsFn) -> impl criogen_core::Node {
    node_with_context(move |ctx: NodeContext, state: Arc<State>| {
        let (name, function) = (name.clone(), function.clone());
        async move {
            let request = json!({
                "node": name,
                "state": state.as_map(),
                "ctx": {
                    "runId": ctx.run_id(), "step": ctx.step(), "node": ctx.node(),
                    "attempt": ctx.attempt(), "resume": ctx.resume(),
                },
            })
            .to_string();
            let envelope = call_js(function, request).await.map_err(NodeError::new)?;
            if let Some(crash) = envelope.get("crash").and_then(Value::as_str) {
                // The exception's key travels in the error (unique to it).
                return Err(NodeError::new(crash)
                    .with_details(json!({"cause_key": envelope.get("causeKey")})));
            }
            let update = match envelope.get("update") {
                Some(Value::Object(map)) => Update::from(map.clone()),
                Some(Value::Null) | None => Update::new(),
                Some(other) => {
                    return Err(NodeError::new(format!(
                        "a node returns an object (the update), not {other}"
                    )));
                }
            };
            if let Some(payload) = envelope.get("suspend") {
                return Ok(NodeOutput::suspend(update, payload.clone()));
            }
            if let Some(targets) = envelope.get("goto").and_then(Value::as_array) {
                let targets: Vec<String> = targets
                    .iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect();
                return Ok(NodeOutput::goto(update, targets));
            }
            Ok(NodeOutput::from(update))
        }
    })
}

/// A JavaScript function as a router: `{"label"}` or `{"crash"}`.
fn js_router(from: String, function: JsFn) -> impl criogen_core::Router {
    router_fn(move |state: Arc<State>| {
        let (from, function) = (from.clone(), function.clone());
        async move {
            let request = json!({"node": from, "state": state.as_map()}).to_string();
            let envelope = call_js(function, request).await.map_err(NodeError::new)?;
            if let Some(crash) = envelope.get("crash").and_then(Value::as_str) {
                // The exception's key travels in the error (unique to it).
                return Err(NodeError::new(crash)
                    .with_details(json!({"cause_key": envelope.get("causeKey")})));
            }
            envelope
                .get("label")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| NodeError::new("a router returns a label (a string)"))
        }
    })
}

fn run_failure(error: RunError) -> Failure {
    let code = error.kind.code().to_string();
    let mut details = json!({"step": error.step});
    if let Some(snapshot) = error.snapshot() {
        details["snapshot"] = to_json(snapshot);
        details["runId"] = json!(snapshot.run_id);
    }
    match &error.kind {
        RunErrorKind::NodeFailed { node, source } | RunErrorKind::RouterFailed { node, source } => {
            details["node"] = json!(node);
            details["cause_key"] = source
                .details
                .as_ref()
                .and_then(|d| d.get("cause_key"))
                .cloned()
                .unwrap_or(Value::Null);
        }
        RunErrorKind::NodeTimeout { node, .. } | RunErrorKind::NodePanicked { node, .. } => {
            details["node"] = json!(node);
        }
        _ => {}
    }
    failure(&code, error.to_string(), Some(details))
}

fn thread_failure(error: ThreadError) -> Failure {
    match error {
        ThreadError::Run(run) => run_failure(*run),
        other => failure(other.code(), other.to_string(), None),
    }
}

fn reducer(name: &str) -> Result<Reducer> {
    match name {
        "replace" => Ok(Reducer::Replace),
        "append" => Ok(Reducer::Append),
        "upsertById" | "upsert_by_id" => Ok(Reducer::UpsertById),
        other => Err(bad(format!("unknown reducer `{other}`"))),
    }
}

fn run_json(run: &RunOutput) -> Value {
    json!({
        "state": run.state,
        "steps": run.steps.len(),
        "suspended": run.is_suspended(),
        "interrupts": to_json(&run.interrupts()),
        "snapshot": run.snapshot().map(to_json),
        "runId": run.run_id,
    })
}

fn workflow_options(payload: &Value, cancel: &CancelToken) -> RunOptions {
    let mut options = RunOptions::default().cancel_token(cancel.clone());
    if let Some(n) = payload["maxSteps"].as_u64() {
        options = options.max_steps(n as usize);
    }
    if let Some(ms) = payload["deadlineMs"].as_u64() {
        options = options.deadline(Duration::from_millis(ms));
    }
    options
}

/// Builds the graph. `nodes`: one function per name in `config.nodes`;
/// `routers`: one per entry of `config.routes`; agents come by handle in
/// `config.agents`.
pub fn build_workflow(config: &str, nodes: Vec<JsFn>, routers: Vec<JsFn>) -> Result<Workflow> {
    let config = parse(config)?;
    let mut builder = WorkflowBuilder::new();
    let names = strings(&config["nodes"]);
    if names.len() != nodes.len() {
        return Err(bad("one function per node"));
    }
    for (name, function) in names.into_iter().zip(nodes) {
        builder.add_node(name.clone(), js_node(name, function));
    }
    for (name, handle) in config["agents"].as_object().cloned().unwrap_or_default() {
        let agent = agent_by_handle(handle.as_u64().unwrap_or_default() as u32)
            .ok_or_else(|| bad(format!("node `{name}`: the agent is gone")))?;
        builder.add_node(name, agent.as_node());
    }
    if let Some(entry) = text(&config, "entry") {
        builder.set_entry(entry);
    }
    for edge in config["edges"].as_array().cloned().unwrap_or_default() {
        builder.add_edge(
            edge[0].as_str().unwrap_or_default(),
            edge[1].as_str().unwrap_or_default(),
        );
    }
    let routes = config["routes"].as_array().cloned().unwrap_or_default();
    if routes.len() != routers.len() {
        return Err(bad("one function per router"));
    }
    for (route, function) in routes.iter().zip(routers) {
        let from = text(route, "from").unwrap_or_default().to_string();
        let table: Vec<(String, String)> = route["routes"]
            .as_object()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|(label, target)| (label, target.as_str().unwrap_or_default().to_string()))
            .collect();
        builder.add_router(from.clone(), js_router(from, function), table);
    }
    for join in config["joins"].as_array().cloned().unwrap_or_default() {
        builder.add_join(strings(&join[0]), join[1].as_str().unwrap_or_default());
    }
    for goto in config["gotos"].as_array().cloned().unwrap_or_default() {
        builder.may_goto(goto[0].as_str().unwrap_or_default(), strings(&goto[1]));
    }
    for (key, name) in config["reducers"].as_object().cloned().unwrap_or_default() {
        builder.reducer(key, reducer(name.as_str().unwrap_or_default())?);
    }
    builder.build().map_err(|err| {
        failure(
            "invalid_workflow",
            err.to_string(),
            Some(json!({
                "issues": err.issues.iter().map(ToString::to_string).collect::<Vec<_>>(),
            })),
        )
    })
}

/// Runs once: payload `{input, maxSteps, deadlineMs}`.
pub async fn workflow_run(workflow: Workflow, payload: String, cancel_id: u32) -> Result<String> {
    let payload = parse(&payload)?;
    let cancel = Registered::take(cancel_id);
    let input = State::try_from(payload["input"].clone())
        .map_err(|_| bad("the input must be an object"))?;
    let run = workflow
        .run_with(input, workflow_options(&payload, cancel.token()))
        .await
        .map_err(run_failure)?;
    Ok(run_json(&run).to_string())
}

/// Continues a snapshot (no storage): payload `{snapshot, values}`.
pub async fn workflow_resume(
    workflow: Workflow,
    payload: String,
    cancel_id: u32,
) -> Result<String> {
    let payload = parse(&payload)?;
    let cancel = Registered::take(cancel_id);
    let snapshot: Snapshot =
        serde_json::from_value(payload["snapshot"].clone()).map_err(|err| {
            failure(
                "resume_invalid",
                format!("unreadable snapshot: {err}"),
                None,
            )
        })?;
    let values = payload["values"].as_object().cloned().unwrap_or_default();
    let run = workflow
        .resume_with(
            &snapshot,
            values,
            workflow_options(&payload, cancel.token()),
        )
        .await
        .map_err(run_failure)?;
    Ok(run_json(&run).to_string())
}

/// A thread operation: payload `{threadId, actor, …}`.
pub async fn workflow_thread(
    workflow: Workflow,
    storage: Storage,
    op: String,
    payload: String,
    cancel_id: u32,
) -> Result<String> {
    let payload = parse(&payload)?;
    let cancel = Registered::take(cancel_id);
    let mut thread = storage
        .workflow_thread(&workflow, text(&payload, "threadId").unwrap_or_default())
        .with_options(workflow_options(&payload, cancel.token()));
    if let Some(actor) = text(&payload, "actor") {
        thread = thread.actor(actor);
    }
    workflow_thread_operation(thread, &op, payload).await
}

fn turn_json(turn: &Turn) -> Value {
    let mut value = run_json(&turn.run);
    value["version"] = json!(turn.version);
    value["checkpointId"] = json!(turn.checkpoint_id);
    value
}

async fn workflow_thread_operation(
    thread: WorkflowThread,
    op: &str,
    payload: Value,
) -> Result<String> {
    let turn = match op {
        "run" => {
            let input = State::try_from(payload["input"].clone())
                .map_err(|_| bad("the input must be an object"))?;
            thread.run(input).await
        }
        "resume" => {
            let values = payload["values"].as_object().cloned().unwrap_or_default();
            thread.resume(values).await
        }
        "retry" => thread.retry().await,
        "state" => return Ok(to_json(&thread.state().await.map_err(thread_failure)?).to_string()),
        "interrupts" => {
            return Ok(to_json(&thread.interrupts().await.map_err(thread_failure)?).to_string());
        }
        "history" => {
            let limit = payload["limit"].as_u64().unwrap_or(20) as usize;
            return Ok(to_json(&thread.history(limit).await.map_err(thread_failure)?).to_string());
        }
        "rewind" => {
            let id = text(&payload, "checkpointId").unwrap_or_default();
            return Ok(to_json(&thread.rewind(id).await.map_err(thread_failure)?).to_string());
        }
        "delete" => return Ok(json!(thread.delete().await.map_err(thread_failure)?).to_string()),
        other => return Err(bad(format!("unknown workflow thread operation `{other}`"))),
    };
    turn.map(|t| turn_json(&t).to_string())
        .map_err(thread_failure)
}
