//! An order agent whose run survives a crash, kept in a SQLite file, with
//! a simulated model (2–3 s per call).
//!
//! The model asks to charge the card and ship the order at once. Charging
//! finishes; shipping hangs, and the "process" is killed. A new process
//! opens the same file, sees the interrupted run, and continues it with
//! `retry`: the card is not charged again, only shipping runs.
//!
//! Run with: `cargo run -p criogenai-agent --features sqlite --example durable_agent`

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use criogenai_agent::{Agent, ToolError, tool_fn};
use criogenai_providers::{FakeModel, FakeReply, ToolSpec};
use criogenai_storage::Storage;
use serde_json::json;

/// The order agent. `ship_works` decides whether shipping hangs.
fn agent(model: FakeModel, storage: Storage, ship_works: Arc<AtomicBool>) -> Agent {
    let charge = tool_fn(
        ToolSpec::new("charge", "Charge the card", json!({"type": "object"})),
        |_args, _ctx| async {
            println!("    payments: card charged $40");
            Ok::<_, ToolError>("charged $40, receipt R-1")
        },
    );
    let ship = tool_fn(
        ToolSpec::new("ship", "Ship the order", json!({"type": "object"})),
        move |_args, _ctx| {
            let works = ship_works.load(Ordering::SeqCst);
            async move {
                if !works {
                    println!("    shipping: waiting for the warehouse…");
                    std::future::pending::<()>().await;
                }
                println!("    shipping: label created");
                Ok::<_, ToolError>("shipped, tracking T-9")
            }
        },
    );
    Agent::builder(model)
        .instructions("You process orders.")
        .tool(charge)
        .tool(ship)
        .storage(storage)
        .build()
        .expect("valid agent")
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let started = Instant::now();
    let path = std::env::temp_dir().join(format!("criogenai-durable-{}.db", std::process::id()));
    let url = format!("sqlite://{}", path.display());

    println!("process 1: customer orders");
    let storage = Storage::connect(&url).await.expect("open database");
    let first = agent(
        FakeModel::new().then(FakeReply::tool_calls([
            ("charge", json!({})),
            ("ship", json!({})),
        ])),
        storage.clone(),
        Arc::new(AtomicBool::new(false)),
    );
    let run = tokio::spawn(async move { first.thread("order-7").send("Buy the blue mug").await });
    // Kill the process once the charge's result is in the database.
    loop {
        let head = storage
            .checkpointer()
            .load("order-7", None)
            .await
            .expect("read");
        let saved = head
            .and_then(|h| h.snapshot)
            .and_then(|s| s.pending_writes.get("tools").map(|w| w.memo.len()));
        if saved == Some(1) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    run.abort();
    let _ = run.await;
    println!("process 1: killed while shipping");

    println!("process 2: opens {}", path.display());
    let storage = Storage::connect(&url).await.expect("open database");
    let second = agent(
        FakeModel::new().then(FakeReply::text("Your mug is paid and on its way (T-9).")),
        storage,
        Arc::new(AtomicBool::new(true)),
    );
    let chat = second.thread("order-7");
    let head = chat.head().await.expect("read").expect("the thread exists");
    println!("  head checkpoint: v{} {:?}", head.version, head.status());
    let reply = chat.retry().await.expect("continue");
    println!("agent: {}", reply.text().unwrap_or_default());

    println!("history (oldest first):");
    let mut history = chat.history(20).await.expect("history");
    history.reverse();
    for checkpoint in history {
        println!(
            "  v{} {:?} {:?} (step {})",
            checkpoint.version, checkpoint.source, checkpoint.status, checkpoint.step
        );
    }
    println!("{:.1} s", started.elapsed().as_secs_f64());
    drop(second);
    for suffix in ["", "-wal", "-shm"] {
        let mut file = path.clone().into_os_string();
        file.push(suffix);
        let _ = std::fs::remove_file(file);
    }
}
