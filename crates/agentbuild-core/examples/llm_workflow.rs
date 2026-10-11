//! A multi-step "LLM" workflow with simulated model calls (2–3 s each).
//!
//! plan → { pros, cons, risks } in parallel → join → write
//!
//! Five model calls at 2–3 s each would take 10–15 s one after another. Here
//! the three middle calls run concurrently, so the run takes about 6–9 s.
//!
//! Run with: `cargo run -p agentbuild-core --example llm_workflow -- "remote work"`

#[path = "../tests/support/fake_llm.rs"]
mod fake_llm;

use std::sync::Arc;
use std::time::Instant;

use agentbuild_core::{END, Node, State, Update, WorkflowBuilder, node_with_context};
use fake_llm::FakeLlm;
use serde_json::Value;

fn text(state: &State, key: &str) -> String {
    state
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A node that sends `prompt(state)` to the model, stores the reply under
/// `key`, and prints when the call started and finished.
fn ask<P>(llm: &Arc<FakeLlm>, clock: Instant, key: &'static str, prompt: P) -> impl Node
where
    P: Fn(&State) -> String + Send + Sync + 'static,
{
    let llm = llm.clone();
    node_with_context(move |ctx, state| {
        let llm = llm.clone();
        let prompt = prompt(&state);
        async move {
            let started = clock.elapsed().as_secs_f32();
            let reply = llm.complete(&prompt).await?;
            println!(
                "  step {} {:<6} {started:>4.1}s → {:>4.1}s",
                ctx.step(),
                ctx.node(),
                clock.elapsed().as_secs_f32()
            );
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
    let clock = Instant::now();

    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "plan",
            ask(&llm, clock, "plan", |s| {
                format!("outline a report on {}", text(s, "topic"))
            }),
        )
        .add_node(
            "pros",
            ask(&llm, clock, "pros", |s| {
                format!("pros of {}", text(s, "topic"))
            }),
        )
        .add_node(
            "cons",
            ask(&llm, clock, "cons", |s| {
                format!("cons of {}", text(s, "topic"))
            }),
        )
        .add_node(
            "risks",
            ask(&llm, clock, "risks", |s| {
                format!("risks of {}", text(s, "topic"))
            }),
        )
        .add_node(
            "write",
            ask(&llm, clock, "report", |s| {
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
    let out = workflow.run(State::new().with("topic", topic)).await?;
    let total = clock.elapsed().as_secs_f32();

    let one_by_one: f32 = llm
        .prompts()
        .iter()
        .map(|p| llm.delay_for(p).as_secs_f32())
        .sum();
    println!(
        "\n{} model calls, {} steps, {total:.1}s (one after another: {one_by_one:.1}s)",
        llm.calls(),
        out.steps.len()
    );
    println!("\nreport: {}", text(&out.state, "report"));
    Ok(())
}
