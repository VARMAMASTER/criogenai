//! Input → Transform → Conditional Branch → Output, with no LLM involved.
//!
//! Run with: `cargo run -p agentbuild-core --example basic_workflow -- "some text"`

use agentbuild_core::{END, NodeError, State, Update, WorkflowBuilder, node_fn, router_fn};
use serde_json::Value;

const SUMMARY_WORDS: usize = 5;

#[tokio::main]
async fn main() {
    let mut builder = WorkflowBuilder::new();
    builder
        // Transform: normalise the raw input and measure it.
        .add_node(
            "normalize",
            node_fn(|state| async move {
                let text = state
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| NodeError::new("input must contain a `text` string"))?;
                let words: Vec<String> = text.split_whitespace().map(str::to_lowercase).collect();
                Ok(Update::new()
                    .set("word_count", words.len())
                    .set("words", words))
            }),
        )
        // Conditional branch: long inputs are summarised, short ones echoed.
        .add_router(
            "normalize",
            router_fn(|state| async move {
                let count = state.get("word_count").and_then(Value::as_u64).unwrap_or(0);
                Ok::<_, NodeError>(if count as usize > SUMMARY_WORDS {
                    "long"
                } else {
                    "short"
                })
            }),
            [("long", "summarize"), ("short", "echo")],
        )
        // Outputs.
        .add_node(
            "summarize",
            node_fn(|state| async move {
                let words = words(&state);
                let head = words[..SUMMARY_WORDS].join(" ");
                Ok(Update::new().set(
                    "output",
                    format!("{head} … (+{} words)", words.len() - SUMMARY_WORDS),
                ))
            }),
        )
        .add_node(
            "echo",
            node_fn(
                |state| async move { Ok(Update::new().set("output", words(&state).join(" "))) },
            ),
        )
        .set_entry("normalize")
        .add_edge("summarize", END)
        .add_edge("echo", END);

    let workflow = match builder.build() {
        Ok(workflow) => workflow,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    };

    let text = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let text = if text.is_empty() {
        "AgentBuild runs workflow graphs on a Rust runtime with Python and TypeScript SDKs"
            .to_string()
    } else {
        text
    };

    match workflow.run(State::new().with("text", text)).await {
        Ok(output) => {
            for step in &output.steps {
                println!("step {}: {}", step.step, step.nodes.join(", "));
            }
            println!("{}", serde_json::to_string_pretty(&output.state).unwrap());
        }
        Err(err) => {
            eprintln!("run failed [{}]: {err}", err.code());
            std::process::exit(1);
        }
    }
}

fn words(state: &State) -> Vec<String> {
    match state.get("words") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|w| w.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    }
}
