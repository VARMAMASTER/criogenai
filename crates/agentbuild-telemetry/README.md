# agentbuild-telemetry

Observability for [AgentBuild](https://github.com/VARMAMASTER/agentbuild):
export every run as an OpenTelemetry trace, with a one-line preset for
[Langfuse](https://langfuse.com) (cloud, any region, or self-hosted).

- each run is a trace (the trace id is the run id, so a suspended run that is
  resumed later stays in the same trace)
- workflow nodes are spans; model calls are **generations** (model, chat
  input and output, token usage, time to first token); tool calls are
  **tool** observations (arguments, result, status); failures are errors;
  warnings and retries are events
- run metadata becomes trace fields: `user_id`, `session_id`, `tags`,
  `name`, `environment`, `release`, anything else as metadata
- never slows or breaks a run: events are queued and exported in the
  background; a full queue or an unreachable backend only drops telemetry
- sensitive data: `capture_content(false)` sends no messages, arguments or
  results; `mask(...)` rewrites them before they leave the process
- standard OTLP/HTTP (JSON), so it also works with Jaeger, Grafana Tempo,
  Datadog, Arize Phoenix or an OpenTelemetry Collector

## Langfuse

```rust,no_run
use agentbuild_core::RunOptions;
use agentbuild_telemetry::{Langfuse, OtlpExporter, TelemetryError};
use serde_json::json;

async fn traced() -> Result<(), TelemetryError> {
    // Reads LANGFUSE_PUBLIC_KEY, LANGFUSE_SECRET_KEY and LANGFUSE_BASE_URL.
    let langfuse = OtlpExporter::start(Langfuse::from_env()?)?;

    let options = RunOptions::default()
        .observe(langfuse.observer())
        .metadata(json!({"user_id": "u-42", "session_id": "chat-7"}));
    // workflow.run_with(input, options).await
    // or agent.run_with(input, options).await

    langfuse.flush().await; // before a short-lived program exits
    Ok(())
}
```

For sensitive data, start from `Langfuse::from_env()?.capture_content(false)`
(names, timings, usage and status only) or add `.mask(|value| ...)`.

Try it: `cargo run -p agentbuild-telemetry --example langfuse` with your
Langfuse keys set.

## Any OpenTelemetry backend

```rust,no_run
use agentbuild_telemetry::{OtlpConfig, OtlpExporter, TelemetryError};

fn exporter() -> Result<OtlpExporter, TelemetryError> {
    OtlpExporter::start(
        OtlpConfig::new("http://localhost:4318/v1/traces").service_name("my-app"),
    )
}
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
