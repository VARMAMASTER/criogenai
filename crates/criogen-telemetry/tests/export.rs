//! Export behaviour against a fake collector on localhost: what is sent,
//! with which headers, retries, failures and back-pressure. Real time (the
//! paused clock does not mix with sockets), short intervals.

#[path = "support/collector.rs"]
mod collector;

use std::time::Duration;

use collector::Collector;
use criogen_agent::{Agent, ToolError, tool_fn};
use criogen_core::{
    END, NodeOutput, RunOptions, State, Update, WorkflowBuilder, node_with_context,
};
use criogen_providers::{FakeModel, FakeReply, ToolSpec};
use criogen_telemetry::{Langfuse, OtlpConfig, OtlpExporter};
use serde_json::json;

fn quick_agent() -> Agent {
    let model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO)
        .then(FakeReply::tool_call("weather", json!({"city": "Paris"})))
        .then(FakeReply::text("21 °C"));
    let weather = tool_fn(
        ToolSpec::new(
            "weather",
            "Temperature",
            json!({"type": "object", "properties": {"city": {"type": "string"}}}),
        ),
        |_, _| async { Ok::<_, ToolError>(json!({"celsius": 21})) },
    );
    Agent::builder(model).tool(weather).build().unwrap()
}

fn fast(config: OtlpConfig) -> OtlpConfig {
    config.batching(500, Duration::from_millis(50))
}

#[tokio::test]
async fn langfuse_requests_carry_auth_and_the_ingestion_header() {
    let collector = Collector::start(vec![]).await;
    let base = collector.url.trim_end_matches("/v1/traces").to_string();
    let exporter =
        OtlpExporter::start(fast(Langfuse::config("pk-lf-test", "sk-lf-test", &base))).unwrap();
    let agent = quick_agent();
    agent
        .run_with("weather?", agent.run_options().observe(exporter.observer()))
        .await
        .unwrap();
    exporter.flush().await;

    let requests = collector.requests();
    assert!(!requests.is_empty());
    let first = &requests[0];
    assert!(first.head.starts_with("POST /api/public/otel/v1/traces"));
    assert_eq!(
        first.header("authorization").as_deref(),
        Some("Basic cGstbGYtdGVzdDpzay1sZi10ZXN0") // base64("pk-lf-test:sk-lf-test")
    );
    assert_eq!(
        first.header("x-langfuse-ingestion-version").as_deref(),
        Some("4")
    );
    assert_eq!(
        first.header("content-type").as_deref(),
        Some("application/json")
    );
    let stats = exporter.stats();
    assert_eq!(stats.exported as usize, collector.spans().len());
    assert_eq!((stats.failed, stats.dropped_events), (0, 0));
}

#[tokio::test]
async fn transient_errors_are_retried() {
    let collector = Collector::start(vec![503, 429]).await;
    let exporter = OtlpExporter::start(fast(OtlpConfig::new(&collector.url))).unwrap();
    let agent = quick_agent();
    agent
        .run_with("weather?", agent.run_options().observe(exporter.observer()))
        .await
        .unwrap();
    exporter.flush().await;
    // The first batch is sent three times (two failures, then success).
    // A fast run may send its spans in more than one batch.
    let requests = collector.requests();
    assert!(requests.len() >= 3, "{} requests", requests.len());
    assert_eq!(requests[0].body, requests[1].body);
    assert_eq!(requests[1].body, requests[2].body);
    let stats = exporter.stats();
    assert!(stats.exported > 0);
    assert_eq!(stats.failed, 0);
}

#[tokio::test]
async fn client_errors_are_not_retried_and_never_fail_the_run() {
    let collector = Collector::start(vec![400]).await;
    let exporter = OtlpExporter::start(fast(OtlpConfig::new(&collector.url))).unwrap();
    let agent = quick_agent();
    let result = agent
        .run_with("weather?", agent.run_options().observe(exporter.observer()))
        .await;
    assert!(result.is_ok());
    exporter.flush().await;
    // The refused batch is never sent again (a fast run may send its
    // spans in more than one batch; later batches succeed).
    let requests = collector.requests();
    assert!(!requests.is_empty());
    assert!(requests[1..].iter().all(|r| r.body != requests[0].body));
    let stats = exporter.stats();
    assert!(stats.failed > 0);
    assert!(exporter.last_error().unwrap().starts_with("HTTP 400"));
}

#[tokio::test]
async fn unreachable_backend_never_fails_the_run() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/traces", listener.local_addr().unwrap());
    drop(listener);
    let exporter = OtlpExporter::start(fast(OtlpConfig::new(url))).unwrap();
    let agent = quick_agent();
    let result = agent
        .run_with("weather?", agent.run_options().observe(exporter.observer()))
        .await
        .unwrap();
    assert_eq!(result.text(), Some("21 °C"));
    exporter.flush().await;
    assert!(exporter.stats().failed > 0);
}

#[tokio::test]
async fn a_full_queue_drops_events_instead_of_blocking() {
    let collector = Collector::start(vec![]).await;
    let exporter =
        OtlpExporter::start(fast(OtlpConfig::new(&collector.url)).queue_capacity(4)).unwrap();
    // One node emits 500 events without yielding: on this single-threaded
    // runtime the export task cannot drain the queue meanwhile.
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "chatty",
            node_with_context(|ctx, _| async move {
                for i in 0..500 {
                    ctx.events().emit("tick", json!(i));
                }
                Ok(NodeOutput::from(Update::new().set("done", true)))
            }),
        )
        .set_entry("chatty")
        .add_edge("chatty", END);
    let workflow = builder.build().unwrap();
    let output = workflow
        .run_with(
            State::new(),
            RunOptions::default().observe(exporter.observer()),
        )
        .await
        .unwrap();
    assert_eq!(output.state.get("done").unwrap(), true);
    assert!(
        exporter.stats().dropped_events >= 490,
        "{:?}",
        exporter.stats()
    );
}

#[tokio::test]
async fn shutdown_exports_unfinished_runs_as_incomplete() {
    let collector = Collector::start(vec![]).await;
    let exporter = OtlpExporter::start(fast(OtlpConfig::new(&collector.url))).unwrap();
    let observer = exporter.observer();
    // A run that is still going when the program shuts down.
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "slow",
            node_with_context(|_, _| async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(NodeOutput::from(Update::new()))
            }),
        )
        .set_entry("slow")
        .add_edge("slow", END);
    let workflow = builder.build().unwrap();
    let run = tokio::spawn(async move {
        workflow
            .run_with(State::new(), RunOptions::default().observe(observer))
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    exporter.shutdown().await;
    run.abort();

    let spans = collector.spans();
    let root = spans
        .iter()
        .find(|s| s.get("parentSpanId").is_none())
        .expect("root span exported");
    assert_eq!(root["status"]["code"], 2);
    assert!(
        spans
            .iter()
            .any(|s| s["name"] == "slow" && s["status"]["code"] == 2)
    );
}

fn attr<'a>(span: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    span["attributes"]
        .as_array()?
        .iter()
        .find(|a| a["key"] == key)
        .map(|a| &a["value"])
}

fn str_attr(span: &serde_json::Value, key: &str) -> Option<String> {
    attr(span, key).and_then(|v| v["stringValue"].as_str().map(str::to_string))
}

#[tokio::test]
async fn an_agent_run_becomes_a_langfuse_trace() {
    let collector = Collector::start(vec![]).await;
    let exporter =
        OtlpExporter::start(fast(OtlpConfig::new(&collector.url).capture_content(true))).unwrap();
    let agent = quick_agent();
    let options = agent
        .run_options()
        .observe(exporter.observer())
        .metadata(json!({"name": "weather-bot", "user_id": "u-1", "session_id": "s-1"}));
    let result = agent.run_with("weather?", options).await.unwrap();
    exporter.flush().await;

    let spans = collector.spans();
    let trace_id = result.run.run_id.replace('-', "");
    assert!(spans.iter().all(|s| s["traceId"] == trace_id.as_str()));
    let of_type = |t: &str| -> Vec<&serde_json::Value> {
        spans
            .iter()
            .filter(|s| str_attr(s, "langfuse.observation.type").as_deref() == Some(t))
            .collect()
    };
    let root = of_type("agent");
    assert_eq!(root.len(), 1);
    let root = root[0];
    assert_eq!(root["name"], "weather-bot");
    assert!(root.get("parentSpanId").is_none());
    assert_eq!(str_attr(root, "langfuse.user.id").as_deref(), Some("u-1"));

    let nodes: Vec<&str> = spans
        .iter()
        .filter(|s| s["parentSpanId"] == root["spanId"])
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(nodes.iter().filter(|n| **n == "model").count(), 2);
    assert_eq!(nodes.iter().filter(|n| **n == "tools").count(), 1);

    let generations = of_type("generation");
    assert_eq!(generations.len(), 2);
    let tools = of_type("tool");
    assert_eq!(tools.len(), 1);
    let tool = tools[0];
    assert_eq!(tool["name"], "weather");
    assert_eq!(
        str_attr(tool, "langfuse.observation.input").as_deref(),
        Some(r#"{"city":"Paris"}"#)
    );
    let tool_parent = spans
        .iter()
        .find(|s| s["spanId"] == tool["parentSpanId"])
        .unwrap();
    assert_eq!(tool_parent["name"], "tools");
    let output: serde_json::Value =
        serde_json::from_str(&str_attr(root, "langfuse.observation.output").unwrap()).unwrap();
    assert_eq!(output["content"], "21 °C");
}

#[tokio::test]
async fn masks_rewrite_content_before_export() {
    let collector = Collector::start(vec![]).await;
    let config = fast(OtlpConfig::new(&collector.url).capture_content(true)).mask(|value| {
        let text = value.to_string().replace("Paris", "[CITY]");
        serde_json::from_str(&text).unwrap_or(value)
    });
    let exporter = OtlpExporter::start(config).unwrap();
    let agent = quick_agent();
    agent
        .run_with(
            "Weather in Paris?",
            agent.run_options().observe(exporter.observer()),
        )
        .await
        .unwrap();
    exporter.flush().await;
    let body = serde_json::to_string(
        &collector
            .requests()
            .iter()
            .map(|r| &r.body)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(!body.contains("Paris"), "masked everywhere");
    assert!(body.contains("[CITY]"));
}

#[tokio::test]
async fn a_failed_node_is_an_error_and_the_trace_still_arrives() {
    let collector = Collector::start(vec![]).await;
    let exporter =
        OtlpExporter::start(fast(OtlpConfig::new(&collector.url).capture_content(true))).unwrap();
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "broken",
            node_with_context(|_, _| async {
                Err::<NodeOutput, _>(criogen_core::NodeError::new("database is down"))
            }),
        )
        .set_entry("broken")
        .add_edge("broken", END);
    let err = builder
        .build()
        .unwrap()
        .run_with(
            State::new(),
            RunOptions::default().observe(exporter.observer()),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "node_failed");
    exporter.flush().await;
    let spans = collector.spans();
    let broken = spans.iter().find(|s| s["name"] == "broken").unwrap();
    assert_eq!(broken["status"]["code"], 2);
    assert_eq!(
        str_attr(broken, "langfuse.observation.level").as_deref(),
        Some("ERROR")
    );
    assert!(
        str_attr(broken, "langfuse.observation.status_message")
            .unwrap()
            .contains("database is down")
    );
    let root = spans
        .iter()
        .find(|s| s.get("parentSpanId").is_none())
        .unwrap();
    assert_eq!(root["status"]["code"], 2);
}

#[tokio::test]
async fn suspend_and_resume_share_one_trace() {
    let collector = Collector::start(vec![]).await;
    let exporter = OtlpExporter::start(fast(OtlpConfig::new(&collector.url))).unwrap();
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "approve",
            node_with_context(|ctx, _| async move {
                Ok(match ctx.resume() {
                    None => NodeOutput::suspend(Update::new(), json!({"question": "ok?"})),
                    Some(answer) => Update::new().set("answer", answer.clone()).into(),
                })
            }),
        )
        .set_entry("approve")
        .add_edge("approve", END);
    let workflow = builder.build().unwrap();
    let options = || RunOptions::default().observe(exporter.observer());
    let paused = workflow.run_with(State::new(), options()).await.unwrap();
    let done = workflow
        .resume_with(
            paused.snapshot().unwrap(),
            [("approve", json!(true))],
            options(),
        )
        .await
        .unwrap();
    exporter.flush().await;
    let spans = collector.spans();
    let roots: Vec<&serde_json::Value> = spans
        .iter()
        .filter(|s| s.get("parentSpanId").is_none())
        .collect();
    assert_eq!(roots.len(), 2, "one root span per segment");
    let trace = done.run_id.replace('-', "");
    assert!(
        spans.iter().all(|s| s["traceId"] == trace.as_str()),
        "one trace"
    );
    assert_ne!(roots[0]["spanId"], roots[1]["spanId"]);
}

#[tokio::test]
async fn by_default_no_error_text_and_no_identity_leaves_the_process() {
    // The defaults protect: a failing node's message (it echoes what
    // failed), the run's user and session ids and custom metadata are not
    // exported; names, timings and the error code still are.
    let collector = Collector::start(vec![]).await;
    let exporter = OtlpExporter::start(fast(OtlpConfig::new(&collector.url))).unwrap();
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "broken",
            node_with_context(|_, _| async {
                Err::<NodeOutput, _>(criogen_core::NodeError::new(
                    "no record for patient Jane Doe, dsn postgres://admin:hunter2@db/prod",
                ))
            }),
        )
        .set_entry("broken")
        .add_edge("broken", END);
    let options = RunOptions::default()
        .observe(exporter.observer())
        .metadata(json!({"user_id": "patient-4711", "session_id": "s-9", "ward": "oncology"}));
    builder
        .build()
        .unwrap()
        .run_with(State::new(), options)
        .await
        .unwrap_err();
    exporter.flush().await;
    let all = serde_json::to_string(&collector.spans()).unwrap();
    for secret in ["Jane Doe", "hunter2", "patient-4711", "s-9", "oncology"] {
        assert!(!all.contains(secret), "`{secret}` was exported: {all}");
    }
    let spans = collector.spans();
    let broken = spans.iter().find(|s| s["name"] == "broken").unwrap();
    assert_eq!(broken["status"]["code"], 2, "still an error");
    assert_eq!(
        str_attr(broken, "langfuse.observation.status_message").as_deref(),
        Some("node_failed")
    );
}
