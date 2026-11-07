//! The trace viewer with a few runs of a scripted agent in it:
//! `cargo run -p criogen-trace --example demo`, then open
//! http://127.0.0.1:4319. No model is called.

use std::time::Duration;

use criogen_agent::{Agent, ToolError, tool_fn};
use criogen_providers::{FakeModel, FakeReply, ToolSpec};
use criogen_telemetry::{OtlpConfig, OtlpExporter};
use criogen_trace::{Price, Prices, Viewer};
use serde_json::json;

fn agent(city: &str, fail: bool) -> Agent {
    let model = FakeModel::with_delay(Duration::from_millis(300), Duration::from_millis(700))
        .then(FakeReply::tool_call("weather", json!({ "city": city })))
        .then(FakeReply::tool_call(
            "calendar",
            json!({ "day": "Saturday" }),
        ))
        .then(FakeReply::text(format!(
            "Saturday in {city} looks good: 24 °C and free."
        )));
    let weather = tool_fn(
        ToolSpec::new(
            "weather",
            "Today's forecast for a city.",
            json!({"type": "object", "properties": {"city": {"type": "string"}}}),
        ),
        move |_, _| async move {
            tokio::time::sleep(Duration::from_millis(120)).await;
            if fail {
                Err(ToolError::new("the weather service is down"))
            } else {
                Ok::<_, ToolError>(json!({"celsius": 24, "sky": "sunny"}))
            }
        },
    );
    let calendar = tool_fn(
        ToolSpec::new(
            "calendar",
            "Free slots on a day.",
            json!({"type": "object", "properties": {"day": {"type": "string"}}}),
        ),
        |_, _| async {
            tokio::time::sleep(Duration::from_millis(60)).await;
            Ok::<_, ToolError>(json!({"free": ["10:00", "15:00"]}))
        },
    );
    Agent::builder(model)
        .tool(weather)
        .tool(calendar)
        .build()
        .expect("a valid agent")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut prices = Prices::new();
    prices.insert("fake".into(), Price::new(0.25, 2.0));
    let viewer = Viewer::builder().memory().prices(prices).build().await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:4319").await?;
    let server = tokio::spawn(criogen_trace::serve(
        viewer,
        listener,
        std::future::pending(),
    ));
    let exporter = OtlpExporter::start(
        OtlpConfig::new("http://127.0.0.1:4319/v1/traces").capture_content(true),
    )?;
    for (city, fail) in [("Lisbon", false), ("Porto", true), ("Faro", false)] {
        let agent = agent(city, fail);
        agent
            .run_with(
                format!("Picnic weather in {city} this weekend?"),
                agent.run_options().observe(exporter.observer()),
            )
            .await?;
    }
    exporter.flush().await;
    println!("open http://127.0.0.1:4319 (Ctrl+C to stop)");
    server.await??;
    Ok(())
}
