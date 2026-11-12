//! Sends one agent run to Langfuse.
//!
//! Needs `LANGFUSE_PUBLIC_KEY`, `LANGFUSE_SECRET_KEY` and (unless you use
//! the EU cloud) `LANGFUSE_BASE_URL`. The model is simulated unless
//! `CRIOGEN_MODEL` is set, in which case an OpenAI-compatible server is
//! used (default Ollama: `CRIOGEN_BASE_URL=http://localhost:11434/v1`).
//!
//! Run with: `cargo run -p criogenai-telemetry --example langfuse`
//!
//! Not run in CI (it needs Langfuse credentials).

use std::sync::Arc;
use std::time::Duration;

use criogenai_agent::{Agent, ToolError, tool_fn};
use criogenai_providers::{Endpoint, FakeModel, FakeReply, Model, ProviderModel, ToolSpec};
use criogenai_telemetry::{Langfuse, OtlpExporter};
use serde_json::json;

#[tokio::main]
async fn main() {
    let config = match Langfuse::from_env() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("{err}. Set LANGFUSE_PUBLIC_KEY and LANGFUSE_SECRET_KEY.");
            std::process::exit(2);
        }
    };
    let langfuse = OtlpExporter::start(config).expect("exporter");

    let model: Arc<dyn Model> = match std::env::var("CRIOGEN_MODEL") {
        Ok(name) => {
            let base = std::env::var("CRIOGEN_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:11434/v1".into());
            let key = std::env::var("CRIOGEN_API_KEY").ok();
            Arc::new(
                ProviderModel::new(Endpoint::openai_compatible("local", base, key), name)
                    .expect("model"),
            )
        }
        Err(_) => Arc::new(
            FakeModel::with_delay(Duration::from_millis(400), Duration::from_millis(900))
                .then(FakeReply::tool_call("weather", json!({"city": "Paris"})))
                .then(FakeReply::text("It is 21 °C and sunny in Paris.")),
        ),
    };
    let weather = tool_fn(
        ToolSpec::new(
            "weather",
            "Current weather for a city",
            json!({"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}),
        ),
        |args, _ctx| async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            Ok::<_, ToolError>(json!({"city": args["city"], "celsius": 21, "sky": "sunny"}))
        },
    );
    let agent = Agent::builder_shared(model)
        .instructions("Answer weather questions with the weather tool.")
        .tool(weather)
        .build()
        .expect("agent");

    let options = agent
        .run_options()
        .observe(langfuse.observer())
        .metadata(json!({
            "name": "criogenai-example",
            "user_id": "example-user",
            "session_id": "example-session",
            "tags": ["criogenai", "example"],
        }));
    match agent
        .run_with("What's the weather in Paris?", options)
        .await
    {
        Ok(result) => println!(
            "answer: {}\nrun id (= Langfuse trace id): {}",
            result.text().unwrap_or("(none)"),
            result.run.run_id.replace('-', "")
        ),
        Err(err) => println!("run failed: {err}"),
    }

    langfuse.flush().await;
    let stats = langfuse.stats();
    println!(
        "exported {} spans ({} failed, {} dropped)",
        stats.exported, stats.failed, stats.dropped_events
    );
    if let Some(error) = langfuse.last_error() {
        println!("last export error: {error}");
    }
}
