//! A multi-step "LLM" workflow with simulated model calls (2–3 s each),
//! streaming live events as it runs.
//!
//! plan → { pros, cons, risks } in parallel → join → write
//!
//! Five model calls at 2–3 s each would take 10–15 s one after another. Here
//! the three middle calls run concurrently, so the run takes about 6–9 s.
//!
//! Run with: `cargo run -p criogen-core --example llm_workflow -- "remote work"`

#[path = "../tests/support/fake_llm.rs"]
mod fake_llm;

use std::sync::Arc;
use std::time::Instant;

use criogen_core::{END, EventKind, Node, State, Update, WorkflowBuilder, node_with_context};
use fake_llm::FakeLlm;
use serde_json::{Value, json};

fn text(state: &State, key: &str) -> String {
    state
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A node that sends `prompt(state)` to the model and stores the reply under
/// `key`. It reports the prompt as a custom event before calling.
fn ask<P>(llm: &Arc<FakeLlm>, key: &'static str, prompt: P) -> impl Node
where
    P: Fn(&State) -> String + Send + Sync + 'static,
{
    let llm = llm.clone();
    node_with_context(move |ctx, state| {
        let llm = llm.clone();
        let prompt = prompt(&state);
        async move {
            ctx.events().emit("model_call", json!({ "prompt": prompt }));
            let reply = llm.complete(&prompt).await?;
            Ok(Update::new().set(key, reply).into())
        }
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let topic = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "remote work".into());
    let llm = Arc::new(FakeLlm::new());

    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "plan",
            ask(&llm, "plan", |s| {
                format!("outline a report on {}", text(s, "topic"))
            }),
        )
        .add_node(
            "pros",
            ask(&llm, "pros", |s| format!("pros of {}", text(s, "topic"))),
        )
        .add_node(
            "cons",
            ask(&llm, "cons", |s| format!("cons of {}", text(s, "topic"))),
        )
        .add_node(
            "risks",
            ask(&llm, "risks", |s| format!("risks of {}", text(s, "topic"))),
        )
        .add_node(
            "write",
            ask(&llm, "report", |s| {
                format!(
                    "write the report from: {} | {} | {}",
                    text(s, "pros"),
                    text(s, "cons"),
                    text(s, "risks")
                )
            }),
        )
        .set_entry("plan")
        .add_edge("plan", "pros")
        .add_edge("plan", "cons")
        .add_edge("plan", "risks")
        .add_join(["pros", "cons", "risks"], "write")
        .add_edge("write", END);
    let workflow = builder.build()?;

    println!("topic: {topic}\n");
    let clock = Instant::now();
    let mut run = workflow.stream(State::new().with("topic", topic));
    while let Some(event) = run.next().await {
        let at = clock.elapsed().as_secs_f32();
        let node = event.node.as_deref().unwrap_or("");
        let detail = match &event.kind {
            EventKind::StepStarted { nodes } => format!("step {}: {nodes:?}", event.step.unwrap()),
            EventKind::NodeFinished { duration_ms, .. } => {
                format!("{node} done in {:.1}s", duration_ms / 1000.0)
            }
            EventKind::Custom { name, payload } => format!("{node} {name}: {}", payload["prompt"]),
            EventKind::RunFinished { status, steps, .. } => format!("{status} after {steps} steps"),
            _ => continue,
        };
        println!("{at:>5.1}s  #{:<3} {detail}", event.seq);
    }
    let out = run.finish().await?;

    let one_by_one: f32 = llm
        .prompts()
        .iter()
        .map(|p| llm.delay_for(p).as_secs_f32())
        .sum();
    println!(
        "\n{} model calls in {:.1}s (one after another: {one_by_one:.1}s)",
        llm.calls(),
        clock.elapsed().as_secs_f32()
    );
    println!("\nreport: {}", text(&out.state, "report"));
    Ok(())
}
