type Error = Box<dyn std::error::Error>;
use criogen_agent::Agent;

pub async fn telemetry(agent: Agent) -> Result<(), Error> {
    // [show]
    use criogen_telemetry::{Langfuse, OtlpConfig, OtlpExporter};
    use serde_json::json;

    // LANGFUSE_PUBLIC_KEY, LANGFUSE_SECRET_KEY, LANGFUSE_BASE_URL
    let langfuse = OtlpExporter::start(Langfuse::from_env()?)?;
    // Or any OpenTelemetry collector, without message content:
    let otel =
        OtlpExporter::start(OtlpConfig::new("http://collector:4318").capture_content(false))?;

    let options = agent
        .run_options()
        .observe(langfuse.observer())
        .metadata(json!({"user_id": "u-42", "session_id": "chat-7"}));
    agent.run_with("Where is order A-1001?", options).await?;
    langfuse.flush().await;
    // [/show]
    Ok(())
}

pub async fn local(agent: Agent) -> Result<(), Error> {
    // [show local]
    // In a terminal: criogen-trace   (then open http://127.0.0.1:4318)
    use criogen_telemetry::{OtlpConfig, OtlpExporter};

    let traces = OtlpExporter::start(OtlpConfig::local())?; // .capture_content(true) for prompts
    agent
        .run_with(
            "Where is order A-1001?",
            agent.run_options().observe(traces.observer()),
        )
        .await?;
    traces.flush().await;
    // [/show local]
    Ok(())
}
