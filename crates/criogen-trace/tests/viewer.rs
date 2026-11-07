//! The trace viewer (design 0023): the real exporter sending to it, the API,
//! bounds, tokens and the page. Local sockets only; real time (the exporter
//! batches on a short interval).

use std::time::Duration;

use criogen_agent::{Agent, ToolError, tool_fn};
use criogen_providers::{FakeModel, FakeReply, ToolSpec};
use criogen_telemetry::{OtlpConfig, OtlpExporter};
use criogen_trace::{CSP, Price, Prices, Viewer};
use serde_json::{Value, json};

struct Running {
    base: String,
    http: reqwest::Client,
}

async fn start(viewer: Viewer) -> Running {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(criogen_trace::serve(
        viewer,
        listener,
        std::future::pending(),
    ));
    Running {
        base,
        http: reqwest::Client::new(),
    }
}

impl Running {
    async fn get(&self, path: &str) -> (u16, Value) {
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn post(&self, body: &Value) -> u16 {
        self.http
            .post(format!("{}/v1/traces", self.base))
            .json(body)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }
}

fn agent(failing_tool: bool) -> Agent {
    let model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO)
        .then(FakeReply::tool_call("weather", json!({"city": "Paris"})))
        .then(FakeReply::text("21 °C"));
    let weather = tool_fn(
        ToolSpec::new(
            "weather",
            "Temperature",
            json!({"type": "object", "properties": {"city": {"type": "string"}}}),
        ),
        move |_, _| async move {
            if failing_tool {
                Err(ToolError::new("no data"))
            } else {
                Ok::<_, ToolError>(json!({"celsius": 21}))
            }
        },
    );
    Agent::builder(model).tool(weather).build().unwrap()
}

#[tokio::test]
async fn runs_exported_by_criogen_appear_with_their_steps() {
    let mut prices = Prices::new();
    prices.insert("fake".into(), Price::new(1.0, 2.0));
    let viewer = start(
        Viewer::builder()
            .memory()
            .prices(prices)
            .build()
            .await
            .unwrap(),
    )
    .await;
    let exporter = OtlpExporter::start(
        OtlpConfig::new(format!("{}/v1/traces", viewer.base))
            .batching(500, Duration::from_millis(20)),
    )
    .unwrap();
    for failing in [false, true] {
        let agent = agent(failing);
        agent
            .run_with("weather?", agent.run_options().observe(exporter.observer()))
            .await
            .unwrap();
    }
    exporter.flush().await;
    assert_eq!(exporter.stats().failed, 0, "{:?}", exporter.last_error());

    let (status, list) = viewer.get("/api/runs").await;
    assert_eq!(status, 200);
    let runs = list["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2, "{list}");
    for run in runs {
        assert_eq!(run["model_calls"], 2, "{run}");
        assert_eq!(run["tool_calls"], 1);
        assert!(run["duration_ms"].as_f64().unwrap() >= 0.0);
    }
    assert_eq!(list["costs"], true);

    let id = runs[0]["id"].as_str().unwrap();
    let (status, detail) = viewer.get(&format!("/api/runs/{id}")).await;
    assert_eq!(status, 200);
    let spans = detail["spans"].as_array().unwrap();
    let kinds: Vec<&str> = spans.iter().filter_map(|s| s["kind"].as_str()).collect();
    assert!(
        kinds.contains(&"run") && kinds.contains(&"model") && kinds.contains(&"tool"),
        "{kinds:?}"
    );
    // The exporter's default: no content.
    assert!(
        spans
            .iter()
            .all(|s| s["attributes"].get("langfuse.observation.input").is_none())
    );

    // The newest first: the run whose tool failed shows a warning.
    let tool = spans.iter().find(|s| s["kind"] == "tool").unwrap();
    assert_eq!(tool["status"], "warning", "{tool}");

    // Search, and the unknown run.
    let (_, found) = viewer.get(&format!("/api/runs?q={}", &id[..12])).await;
    assert_eq!(found["runs"].as_array().unwrap().len(), 1);
    let (status, _) = viewer.get(&format!("/api/runs/{}", "0".repeat(32))).await;
    assert_eq!(status, 404);
    let (status, _) = viewer.get("/api/runs/not-an-id").await;
    assert_eq!(status, 400);

    // Clear.
    viewer
        .http
        .delete(format!("{}/api/runs", viewer.base))
        .send()
        .await
        .unwrap();
    assert_eq!(viewer.get("/api/runs").await.1["runs"], json!([]));
}

fn span(trace: &str, id: &str, parent: Option<&str>, name: &str, start_s: u64) -> Value {
    long_span(trace, id, parent, name, start_s, 10)
}

fn long_span(
    trace: &str,
    id: &str,
    parent: Option<&str>,
    name: &str,
    start_s: u64,
    note: usize,
) -> Value {
    json!({
        "traceId": trace, "spanId": id, "parentSpanId": parent.unwrap_or(""), "name": name,
        "startTimeUnixNano": (start_s * 1_000_000_000).to_string(),
        "endTimeUnixNano": ((start_s + 1) * 1_000_000_000).to_string(),
        "attributes": [{"key": "note", "value": {"stringValue": "x".repeat(note)}}],
    })
}

fn request(spans: Vec<Value>) -> Value {
    json!({"resourceSpans": [{"scopeSpans": [{"spans": spans}]}]})
}

fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[tokio::test]
async fn what_arrives_is_bounded_and_checked() {
    let viewer = start(
        Viewer::builder()
            .memory()
            .keep_runs(3)
            .build()
            .await
            .unwrap(),
    )
    .await;
    let trace = "a".repeat(32);
    let now = now_s();
    // More spans than a run may hold; long attributes; a bad id.
    let mut spans = vec![long_span(
        &trace,
        &"1".repeat(16),
        None,
        "<script>alert(1)</script>",
        now,
        200_000,
    )];
    for i in 0..600 {
        spans.push(span(
            &trace,
            &format!("{:016x}", i + 2),
            Some(&"1".repeat(16)),
            "step",
            now,
        ));
    }
    spans.push(span("not-hex", &"2".repeat(16), None, "bad", now));
    assert_eq!(viewer.post(&request(spans)).await, 200);
    let (_, detail) = viewer.get(&format!("/api/runs/{trace}")).await;
    let spans = detail["spans"].as_array().unwrap();
    assert_eq!(spans.len(), criogen_trace::MAX_SPANS_PER_RUN);
    let root = spans.iter().find(|s| s["id"] == "1".repeat(16)).unwrap();
    let note = root["attributes"]["note"].as_str().unwrap();
    assert!(note.len() <= criogen_trace::MAX_ATTRIBUTE_BYTES + 16 && note.ends_with("(cut)"));
    // A name is data: kept as text, escaped by the page.
    assert_eq!(detail["run"]["name"], "<script>alert(1)</script>");

    // Protobuf is refused with a reason; a body that is not JSON too.
    let response = viewer
        .http
        .post(format!("{}/v1/traces", viewer.base))
        .header("content-type", "application/x-protobuf")
        .body(vec![1u8, 2, 3])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 415);
    let response = viewer
        .http
        .post(format!("{}/v1/traces", viewer.base))
        .header("content-type", "application/json")
        .body("{nope")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 400);

    // Retention: only the newest runs are kept.
    for i in 0..5u64 {
        let t = format!("{:032x}", i + 100);
        viewer
            .post(&request(vec![span(
                &t,
                &"3".repeat(16),
                None,
                "r",
                now + i,
            )]))
            .await;
    }
    let (_, list) = viewer.get("/api/runs").await;
    assert_eq!(list["runs"].as_array().unwrap().len(), 3);
    // Runs older than the retention period go too.
    let old = format!("{:032x}", 999);
    viewer
        .post(&request(vec![span(
            &old,
            &"4".repeat(16),
            None,
            "old",
            now - 30 * 86_400,
        )]))
        .await;
    assert_eq!(viewer.get(&format!("/api/runs/{old}")).await.0, 404);
}

#[tokio::test]
async fn a_token_guards_the_api_and_remote_addresses_need_one() {
    let viewer = start(
        Viewer::builder()
            .memory()
            .token("trace-secret-123")
            .build()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(viewer.get("/api/runs").await.0, 401);
    assert_eq!(viewer.post(&request(vec![])).await, 401);
    let ok = viewer
        .http
        .get(format!("{}/api/runs", viewer.base))
        .bearer_auth("trace-secret-123")
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status().as_u16(), 200);
    let wrong = viewer
        .http
        .get(format!("{}/api/runs", viewer.base))
        .bearer_auth("trace-secret-124")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status().as_u16(), 401);
    assert!(!wrong.text().await.unwrap().contains("secret"));

    // Serving on every address without a token is refused.
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let open = Viewer::builder().memory().build().await.unwrap();
    let err = criogen_trace::serve(open, listener, std::future::pending())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("without a token"), "{err}");
}

#[tokio::test]
async fn the_page_is_served_alone_with_a_strict_policy() {
    let viewer = start(Viewer::builder().memory().build().await.unwrap()).await;
    let response = viewer.http.get(&viewer.base).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.headers()["content-security-policy"], CSP);
    let html = response.text().await.unwrap();
    assert!(html.contains("<title>Criogen traces</title>"));
    // Nothing is loaded from elsewhere, and spans are never read as HTML.
    assert!(!html.contains("src=\"http") && !html.contains("href=\"http"));
    assert!(!html.contains("innerHTML"));
}

#[tokio::test]
async fn runs_survive_a_restart_in_a_file() {
    let dir = std::env::temp_dir().join(format!("criogen-trace-{}", std::process::id()));
    let path = dir.join("traces.db");
    let trace = "b".repeat(32);
    {
        let built = Viewer::builder().sqlite(&path).build().await.unwrap();
        let viewer = start(built.clone()).await;
        viewer
            .post(&request(vec![span(
                &trace,
                &"5".repeat(16),
                None,
                "saved",
                now_s(),
            )]))
            .await;
        built.close().await;
    }
    let viewer = start(Viewer::builder().sqlite(&path).build().await.unwrap()).await;
    assert_eq!(viewer.get(&format!("/api/runs/{trace}")).await.0, 200);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn the_sdks_start_and_stop_it_from_a_spec() {
    let (builder, address) = criogen_trace::ViewerBuilder::from_spec(
        &json!({"listen": "127.0.0.1:0", "db": "memory", "prices": {"fake": {"input": 1, "output": 2}}}),
    )
    .unwrap();
    let running = criogen_trace::start(builder, address).await.unwrap();
    let url = running.url();
    assert_eq!(reqwest::get(&url).await.unwrap().status().as_u16(), 200);
    running.stop().await;
    assert!(reqwest::get(&url).await.is_err());

    let (builder, address) =
        criogen_trace::ViewerBuilder::from_spec(&json!({"listen": "0.0.0.0:0", "db": "memory"}))
            .unwrap();
    assert!(criogen_trace::start(builder, address).await.is_err());
    assert!(criogen_trace::ViewerBuilder::from_spec(&json!({"listen": "nope"})).is_err());
}
