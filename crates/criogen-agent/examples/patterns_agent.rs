//! Ready-made patterns and specialists (design 0019), with a simulated model:
//!
//! ```sh
//! cargo run -p criogen-agent --example patterns_agent
//! ```

use std::sync::Arc;
use std::time::Duration;

use criogen_agent::patterns::{self, Step};
use criogen_agent::{Agent, ReviewDecision};
use criogen_providers::{FakeModel, FakeReply, Model};

fn model(replies: &[&str]) -> FakeModel {
    let mut model = FakeModel::with_delay(Duration::from_millis(50), Duration::from_millis(100));
    for reply in replies {
        model = model.then(FakeReply::text(*reply));
    }
    model
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Registered specialists, one after the other: "researcher > writer".
    let shared: Arc<dyn Model> = Arc::new(model(&[]));
    let flow = patterns::quick("researcher > writer", shared)?;
    let result = flow.run("Write about solar panels").await?;
    println!("{}\n", result.explain());

    // 2. A writer and a critic, until the critic approves (at most 3 rounds).
    let writer = Agent::builder(model(&["First draft.", "Better draft."])).build()?;
    let critic = Agent::builder(model(&["REVISE: add a number.", "APPROVED"])).build()?;
    let flow = patterns::review_and_critique(writer, critic, 3)?;
    let result = flow.run("Write a tagline").await?;
    println!(
        "{}\n=> {} ({})\n",
        result.explain(),
        result,
        result.stop_reason
    );

    // 3. A person looks at the draft before it goes out.
    let flow = patterns::human_review(Step::from(
        Agent::builder(model(&["Release 120 rows."])).build()?,
    ))?;
    let paused = flow.run("Release the dataset?").await?;
    println!(
        "waiting for a person: {}",
        paused.waiting_for.as_ref().unwrap()
    );
    let done = flow.resume(&paused, ReviewDecision::Approve).await?;
    println!("=> {} ({})", done, done.stop_reason);
    Ok(())
}
