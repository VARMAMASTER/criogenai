//! Recovering a failed run without repeating side effects, with a simulated
//! LLM (2–3 s per call).
//!
//! `email` and `report` run in parallel. `report` saves a draft with
//! `ctx.memo()`, then crashes on a bug. The failure carries a snapshot; we
//! store it as JSON, "fix the bug", and retry from it:
//!
//! - `email` is not run again (its output was saved),
//! - `report` runs again, but its draft comes from the memo (no second call).
//!
//! Run with: `cargo run -p agentbuild-core --example recover_workflow`

#[path = "../tests/support/fake_llm.rs"]
mod fake_llm;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use agentbuild_core::{
    END, NodeError, Snapshot, State, Update, Workflow, WorkflowBuilder, node_fn, node_with_context,
};
use fake_llm::FakeLlm;

fn workflow(llm: Arc<FakeLlm>, bug: Arc<AtomicBool>) -> Workflow {
    let email_llm = llm.clone();
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node("start", node_fn(|_| async { Ok(Update::new()) }))
        .add_node(
            "email",
            node_fn(move |_| {
                let llm = email_llm.clone();
                async move {
                    let sent = llm.complete("send the customer email").await?;
                    println!("    email: sent");
                    Ok(Update::new().set("email", sent))
                }
            }),
        )
        .add_node(
            "report",
            node_with_context(move |ctx, _| {
                let (llm, bug) = (llm.clone(), bug.clone());
                async move {
                    let draft: String = ctx
                        .memo()
                        .once("draft", || async {
                            let draft = llm.complete("draft the report").await?;
                            println!("    report: drafted");
                            Ok(draft)
                        })
                        .await?;
                    if bug.load(Ordering::SeqCst) {
                        return Err(NodeError::new("formatting bug"));
                    }
                    println!("    report: formatted");
                    Ok(Update::new().set("report", draft).into())
                }
            }),
        )
        .set_entry("start")
        .add_edge("start", "email")
        .add_edge("start", "report")
        .add_edge("email", END)
        .add_edge("report", END);
    builder.build().expect("valid workflow")
}

#[tokio::main]
async fn main() {
    let llm = Arc::new(FakeLlm::new());
    let bug = Arc::new(AtomicBool::new(true));
    let wf = workflow(llm.clone(), bug.clone());

    println!("== first run (has a bug)");
    let clock = Instant::now();
    let err = wf
        .run(State::new())
        .await
        .expect_err("the bug fails the run");
    println!(
        "  failed after {:.1}s: {} ({})",
        clock.elapsed().as_secs_f32(),
        err,
        err.code()
    );
    let saved = serde_json::to_string_pretty(err.snapshot().expect("snapshot")).unwrap();
    println!(
        "  snapshot ({} bytes of JSON), stored for later",
        saved.len()
    );

    println!("== bug fixed, retry from the snapshot");
    bug.store(false, Ordering::SeqCst);
    let snapshot: Snapshot = serde_json::from_str(&saved).unwrap();
    let clock = Instant::now();
    let done = wf.retry(&snapshot).await.expect("fixed");
    println!(
        "  completed after {:.1}s with keys {:?}",
        clock.elapsed().as_secs_f32(),
        done.state.as_map().keys().collect::<Vec<_>>()
    );
    println!("  model calls in total: {} (no repeats)", llm.calls());
}
