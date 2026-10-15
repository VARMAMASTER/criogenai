//! A support conversation kept in a thread, with a simulated model (2–3 s
//! per call).
//!
//! The customer writes twice; the agent remembers the first message. The
//! refund needs approval: the "first process" stops while it is pending,
//! and a second agent instance on the same storage (another process, in a
//! real app) approves it. Then the history is printed.
//!
//! Run with: `cargo run -p agentbuild-agent --example thread_agent`

use std::time::Instant;

use agentbuild_agent::{Agent, Approval, ToolError, tool_fn};
use agentbuild_providers::{FakeModel, FakeReply, ToolSpec};
use agentbuild_storage::Storage;
use serde_json::json;

fn agent(model: FakeModel, storage: &Storage) -> Agent {
    let refund = tool_fn(
        ToolSpec::new(
            "refund",
            "Refund an order",
            json!({
                "type": "object",
                "properties": {"order": {"type": "string"}},
                "required": ["order"]
            }),
        ),
        |args, _ctx| async move {
            println!("    payments: refunded {}", args["order"]);
            Ok::<_, ToolError>("refund issued")
        },
    );
    Agent::builder(model)
        .instructions("You are a support agent.")
        .tool(refund)
        .approval(Approval::new().tool("refund"))
        .storage(storage.clone())
        .build()
        .expect("valid agent")
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let started = Instant::now();
    // In-memory here; `Storage::connect("sqlite://…")` or "postgres://…"
    // with the `sqlite` / `postgres` feature for real use.
    let storage = Storage::in_memory();

    let first = agent(
        FakeModel::new()
            .then(FakeReply::text(
                "Sorry to hear that. What is the order number?",
            ))
            .then(FakeReply::tool_call("refund", json!({"order": "A-1001"}))),
        &storage,
    );
    let chat = first.thread("customer-42");
    for text in ["My order arrived broken", "It is A-1001, please refund it"] {
        println!("customer: {text}");
        let reply = chat.send(text).await.expect("send");
        if let Some(answer) = reply.text() {
            println!("agent:    {answer}");
        }
        for request in reply.approvals() {
            println!("approval needed: {} {}", request.tool, json!(request.args));
        }
    }
    drop(first);
    println!("(first process stops; the approval waits in storage)");

    let second = agent(
        FakeModel::new().then(FakeReply::text("Your refund for A-1001 is on its way.")),
        &storage,
    );
    let chat = second.thread("customer-42");
    println!(
        "pending in storage: {}",
        chat.pending_approvals().await.expect("read").len()
    );
    let done = chat.approve().await.expect("approve");
    println!("agent:    {}", done.text().unwrap_or_default());

    println!("history (newest first):");
    for checkpoint in chat.history(10).await.expect("history") {
        println!(
            "  v{} {:?} {:?}",
            checkpoint.version, checkpoint.source, checkpoint.status
        );
    }
    println!(
        "{} messages, {:.1} s",
        chat.messages().await.expect("messages").len(),
        started.elapsed().as_secs_f64()
    );
}
