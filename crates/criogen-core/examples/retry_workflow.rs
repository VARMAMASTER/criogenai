//! Retries, timeouts and deadlines with a simulated LLM (2–3 s per call).
//!
//! 1. A flaky model fails twice with a retryable error, then answers. A retry
//!    policy recovers.
//! 2. The same call with a 1 s timeout fails with `node_timeout`.
//!
//! Run with: `cargo run -p criogen-core --example retry_workflow`

#[path = "../tests/support/fake_llm.rs"]
mod fake_llm;

use std::sync::Arc;
use std::time::{Duration, Instant};

use criogen_core::{
    END, EventKind, NodePolicy, RetryPolicy, RunOptions, State, Update, Workflow, WorkflowBuilder,
    node_fn,
};
use fake_llm::FakeLlm;

fn workflow(llm: Arc<FakeLlm>, policy: NodePolicy) -> Workflow {
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "model",
            node_fn(move |_| {
                let llm = llm.clone();
                async move { Ok(Update::new().set("answer", llm.complete("summarise the news").await?)) }
            }),
        )
        .set_entry("model")
        .add_edge("model", END)
        .policy("model", policy);
    builder.build().expect("valid workflow")
}

async fn watch(title: &str, workflow: &Workflow) {
    println!("== {title}");
    let clock = Instant::now();
    let options = RunOptions::default().deadline(Duration::from_secs(30));
    let mut run = workflow.stream_with(State::new(), options);
    while let Some(event) = run.next().await {
        let at = clock.elapsed().as_secs_f32();
        let line = match &event.kind {
            EventKind::NodeStarted { attempt } => format!("attempt {attempt} started"),
            EventKind::AttemptFailed {
                attempt,
                message,
                retry_in_ms,
                ..
            } => format!("attempt {attempt} failed ({message}); retrying in {retry_in_ms} ms"),
            EventKind::NodeFinished { .. } => "answered".to_string(),
            EventKind::NodeFailed { code, message, .. } => format!("{code}: {message}"),
            _ => continue,
        };
        println!("{at:>5.1}s  {line}");
    }
    match run.finish().await {
        Ok(out) => println!("result: {}\n", out.state.get("answer").unwrap()),
        Err(err) => println!("error: {} ({})\n", err, err.code()),
    }
}

#[tokio::main]
async fn main() {
    let retry = RetryPolicy::new()
        .max_attempts(3)
        .backoff(Duration::from_millis(500), 2.0, Duration::from_secs(5))
        .jitter(false);

    let flaky = Arc::new(FakeLlm::new().flaky_first(2));
    watch(
        "flaky model, retried",
        &workflow(
            flaky,
            NodePolicy::new()
                .timeout(Duration::from_secs(5))
                .retry(retry),
        ),
    )
    .await;

    let slow = Arc::new(FakeLlm::new());
    watch(
        "1 s timeout on a 2–3 s call",
        &workflow(slow, NodePolicy::new().timeout(Duration::from_secs(1))),
    )
    .await;
}
