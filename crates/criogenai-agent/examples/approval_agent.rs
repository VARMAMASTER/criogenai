//! An agent whose `place_order` tool needs a doctor's approval, with a
//! simulated model (2–3 s per call).
//!
//! The run suspends before the order is placed; the snapshot is stored as
//! JSON (as an app would store it in a database); then the "doctor" edits
//! the dose and approves, and the run continues from the stored JSON.
//!
//! Run with: `cargo run -p criogenai-agent --example approval_agent -- approve`
//! (or `edit`, `reject`).

use std::time::Instant;

use criogenai_agent::{Agent, Approval, ApprovalRule, Decision, ToolError, tool_fn};
use criogenai_core::Snapshot;
use criogenai_providers::{FakeModel, FakeReply, ToolSpec};
use serde_json::json;

fn agent() -> Agent {
    let model = FakeModel::new()
        .then(FakeReply::tool_call(
            "place_order",
            json!({"patient": "P-1042", "drug": "amoxicillin", "dose_mg": 1000}),
        ))
        .then(FakeReply::text(
            "The order has been handled as the doctor decided.",
        ));
    let place_order = tool_fn(
        ToolSpec::new(
            "place_order",
            "Place a medication order",
            json!({
                "type": "object",
                "properties": {
                    "patient": {"type": "string"},
                    "drug": {"type": "string"},
                    "dose_mg": {"type": "integer", "minimum": 1}
                },
                "required": ["patient", "drug", "dose_mg"],
                "additionalProperties": false
            }),
        ),
        |args, _ctx| async move {
            println!(
                "    pharmacy system: order placed {}",
                serde_json::Value::Object(args)
            );
            Ok::<_, ToolError>("order #5531 placed")
        },
    );
    Agent::builder(model)
        .instructions("You help doctors place orders.")
        .tool(place_order)
        .approval(
            Approval::new().rule(ApprovalRule::new("place_order").describe(|_, args| {
                format!(
                    "Order {} {} mg for patient {}",
                    args["drug"].as_str().unwrap_or("?"),
                    args["dose_mg"],
                    args["patient"].as_str().unwrap_or("?")
                )
            })),
        )
        .build()
        .expect("valid agent")
}

#[tokio::main]
async fn main() {
    let choice = std::env::args().nth(1).unwrap_or_else(|| "approve".into());
    let clock = Instant::now();
    // In an app the snapshot would be resumed by another request, maybe in
    // another process; an agent built the same way (and the same model
    // conversation) continues it.
    let agent = agent();

    println!("== the agent prepares an order");
    let paused = agent
        .run("Start amoxicillin for P-1042")
        .await
        .expect("run");
    let requests = paused.approval_requests();
    for request in &requests {
        println!(
            "  {:.1}s waiting for approval: {} (call {})",
            clock.elapsed().as_secs_f32(),
            request.description.as_deref().unwrap_or(&request.tool),
            request.call_id
        );
    }
    let stored = serde_json::to_string(paused.snapshot().expect("suspended")).unwrap();
    println!(
        "  snapshot stored ({} bytes); nothing has been ordered yet",
        stored.len()
    );

    let decision = match choice.as_str() {
        "edit" => {
            let mut args = requests[0].args.clone();
            args.insert("dose_mg".into(), json!(500));
            Decision::edit(args)
        }
        "reject" => Decision::reject("Patient is allergic to penicillin."),
        _ => Decision::approve(),
    };
    println!("== the doctor decides: {choice}");
    let snapshot: Snapshot = serde_json::from_str(&stored).unwrap();
    let done = agent
        .resume(&snapshot, [(requests[0].call_id.clone(), decision)])
        .await
        .expect("resume");
    for message in done.messages.iter().filter(|m| m.call_id.is_some()) {
        println!(
            "  tool result ({:?}): {}",
            message.status.unwrap(),
            message.text()
        );
    }
    println!(
        "  {:.1}s answer: {}",
        clock.elapsed().as_secs_f32(),
        done.text().unwrap_or("(none)")
    );
}
