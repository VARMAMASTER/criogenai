//! Human-in-the-loop: pause for approval, store the run as JSON, resume later.
//!
//! Run with: `cargo run -p criogenai-core --example approval_workflow -- yes`
//! (or `-- no`)

use criogenai_core::{
    END, NodeOutput, Reducer, Snapshot, State, Update, WorkflowBuilder, node_fn, node_with_context,
};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let decision = std::env::args().nth(1).unwrap_or_else(|| "yes".to_string());

    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "draft",
            node_fn(|_| async {
                Ok(Update::new()
                    .set("email", "Your refund of $49 has been issued.")
                    .set("log", json!(["drafted"])))
            }),
        )
        // Suspends the first time; on resume it receives the reviewer's decision.
        .add_node(
            "review",
            node_with_context(|ctx, state| async move {
                match ctx.resume() {
                    None => Ok(NodeOutput::suspend(
                        Update::new(),
                        json!({ "action": "send_email", "email": state.get("email") }),
                    )),
                    Some(decision) => {
                        let decision = decision.as_str().unwrap_or("invalid");
                        let approved = decision == "yes";
                        Ok(NodeOutput::goto(
                            Update::new().set("log", json!([format!("reviewed: {decision}")])),
                            [if approved { "send" } else { END }],
                        ))
                    }
                }
            }),
        )
        .add_node(
            "send",
            node_fn(|_| async { Ok(Update::new().set("log", json!(["sent"]))) }),
        )
        .set_entry("draft")
        .add_edge("draft", "review")
        .may_goto("review", ["send", END])
        .add_edge("send", END)
        .reducer("log", Reducer::Append);
    let workflow = builder.build()?;

    // First segment: runs until the review node asks for approval.
    let paused = workflow.run(State::new()).await?;
    let interrupt = &paused.interrupts()[0];
    println!(
        "paused at `{}` with request:\n{}\n",
        interrupt.node, interrupt.payload
    );

    // The snapshot is plain JSON: store it in a database, a file, a queue...
    let stored = serde_json::to_string_pretty(paused.snapshot().expect("suspended"))?;
    println!("stored snapshot ({} bytes)\n", stored.len());

    // ...later, possibly in another process:
    let snapshot: Snapshot = serde_json::from_str(&stored)?;
    let done = workflow
        .resume(&snapshot, [("review", json!(decision))])
        .await?;

    println!("resumed run {} → {:?}", done.run_id, done.status);
    println!("log: {}", done.state.get("log").expect("log"));
    Ok(())
}
