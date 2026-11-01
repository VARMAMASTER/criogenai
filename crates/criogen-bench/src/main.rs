//! Criogen's benchmarks (design 0014): what the framework itself costs,
//! with simulated models (no network), so the numbers measure our code.
//!
//! `cargo run -p criogen-bench --release [-- quick]` prints a Markdown
//! table; `docs/benchmarks.md` records a run with its machine.

use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use criogen_agent::{Agent, ToolError, tool_fn};
use criogen_core::{END, State, Update, Workflow, WorkflowBuilder, node_fn};
use criogen_gateway::Gateway;
use criogen_providers::{
    BoxFuture, Capabilities, ContentBlock, FinishReason, Message, Model, ModelError, ModelRequest,
    ModelResponse, Role, ToolSpec, Usage,
};
use criogen_storage::Storage;
use serde_json::json;

/// Timings of one scenario, in microseconds.
struct Result {
    name: &'static str,
    unit: &'static str,
    /// How many units one run does (e.g. 10 steps).
    per_run: f64,
    samples: Vec<f64>,
}

impl Result {
    fn percentile(&self, p: f64) -> f64 {
        let mut sorted = self.samples.clone();
        sorted.sort_by(f64::total_cmp);
        let at = ((sorted.len() - 1) as f64 * p).round() as usize;
        sorted[at]
    }
}

/// Runs `f` `warmup` times, then `runs` times, timing each.
async fn measure<F, Fut>(
    name: &'static str,
    unit: &'static str,
    per_run: f64,
    (warmup, runs): (usize, usize),
    mut f: F,
) -> Result
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    for _ in 0..warmup {
        f().await;
    }
    let mut samples = Vec::with_capacity(runs);
    for _ in 0..runs {
        let started = Instant::now();
        f().await;
        samples.push(started.elapsed().as_secs_f64() * 1e6);
    }
    Result {
        name,
        unit,
        per_run,
        samples,
    }
}

/// `n` trivial nodes in a row.
fn chain(n: usize) -> Workflow {
    let mut builder = WorkflowBuilder::new();
    for i in 0..n {
        builder.add_node(
            format!("n{i}"),
            node_fn(move |_| async move { Ok(Update::new().set(format!("k{i}"), i)) }),
        );
        if i > 0 {
            builder.add_edge(format!("n{}", i - 1), format!("n{i}"));
        }
    }
    builder.set_entry("n0").add_edge(format!("n{}", n - 1), END);
    builder.build().expect("valid chain")
}

/// One node fanning out to `n` parallel nodes, joined into one.
fn fan_out(n: usize) -> Workflow {
    let mut builder = WorkflowBuilder::new();
    builder.add_node("start", node_fn(|_| async { Ok(Update::new()) }));
    builder.add_node(
        "end",
        node_fn(|_| async { Ok(Update::new().set("done", true)) }),
    );
    let mut names = Vec::new();
    for i in 0..n {
        let name = format!("p{i}");
        builder.add_node(
            name.clone(),
            node_fn(move |_| async move { Ok(Update::new().set(format!("p{i}"), i)) }),
        );
        builder.add_edge("start", name.clone());
        names.push(name);
    }
    builder
        .set_entry("start")
        .add_join(names, "end")
        .add_edge("end", END);
    builder.build().expect("valid fan-out")
}

/// A model that answers at once: `tools` tool calls first, then text once
/// the transcript has tool results. Reusable, unlike a scripted model.
struct BenchModel {
    tools: usize,
}

impl Model for BenchModel {
    fn provider(&self) -> &str {
        "bench"
    }

    fn model(&self) -> &str {
        "instant"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    fn invoke<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> BoxFuture<'a, std::result::Result<ModelResponse, ModelError>> {
        let answered = request.messages.iter().any(|m| m.role == Role::Tool);
        let response = if self.tools == 0 || answered {
            ModelResponse::new(
                Message::assistant("done"),
                Usage::new(10, 1),
                FinishReason::Stop,
            )
        } else {
            let calls = (0..self.tools)
                .map(|i| {
                    ContentBlock::tool_call(format!("call_{i}"), "echo", serde_json::Map::new())
                })
                .collect();
            ModelResponse::new(
                Message::with_content(Role::Assistant, calls),
                Usage::new(10, 5),
                FinishReason::ToolCalls,
            )
        };
        Box::pin(async move { Ok(response) })
    }
}

/// An agent on [`BenchModel`] with an `echo` tool.
fn agent(tools: usize, storage: Option<Storage>) -> Agent {
    let mut builder = Agent::builder(BenchModel { tools }).tool(tool_fn(
        ToolSpec::new("echo", "Echoes", json!({"type": "object"})),
        |args, _ctx| async move { Ok::<_, ToolError>(serde_json::Value::Object(args)) },
    ));
    if let Some(storage) = storage {
        builder = builder.storage(storage);
    }
    builder.build().expect("valid agent")
}

async fn gateway_base() -> (String, Arc<dyn Model>) {
    // The upstream answers at once: what is left is the gateway.
    let upstream: Arc<dyn Model> = Arc::new(BenchModel { tools: 0 });
    let gateway = Gateway::builder()
        .route("bench", upstream.clone())
        .build()
        .expect("gateway");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let router = gateway.router();
    tokio::spawn(async move { axum::serve(listener, router).await });
    (base, upstream)
}

#[tokio::main]
async fn main() {
    let quick = std::env::args().any(|a| a == "quick");
    let n = |full: usize| if quick { (full / 10).max(5) } else { full };
    let mut results = Vec::new();

    let wf = chain(10);
    results.push(
        measure(
            "workflow: 10 nodes in a row",
            "step",
            10.0,
            (n(200), n(2000)),
            || async {
                wf.run(State::new()).await.expect("run");
            },
        )
        .await,
    );
    let wf = fan_out(100);
    results.push(
        measure(
            "workflow: 100 parallel nodes + join",
            "node",
            102.0,
            (n(50), n(500)),
            || async {
                wf.run(State::new()).await.expect("run");
            },
        )
        .await,
    );
    let answering = agent(0, None);
    results.push(
        measure(
            "agent: one model call",
            "run",
            1.0,
            (n(200), n(2000)),
            || async {
                answering.run("hi").await.expect("run");
            },
        )
        .await,
    );
    let calling = agent(10, None);
    results.push(
        measure(
            "agent: 10 tool calls + 2 model calls",
            "run",
            1.0,
            (n(100), n(1000)),
            || async {
                calling.run("go").await.expect("run");
            },
        )
        .await,
    );

    let in_memory = agent(0, Some(Storage::in_memory()));
    let mut turn = 0;
    results.push(
        measure(
            "thread: send, in memory",
            "turn",
            1.0,
            (n(100), n(1000)),
            || {
                turn += 1;
                let thread = in_memory.thread(format!("t{turn}"));
                async move {
                    thread.send("hi").await.expect("send");
                }
            },
        )
        .await,
    );
    let file = std::env::temp_dir().join(format!("criogen-bench-{}.db", std::process::id()));
    let sqlite = Storage::connect(&format!("sqlite://{}", file.display()))
        .await
        .expect("sqlite");
    let on_sqlite = agent(0, Some(sqlite.clone()));
    results.push(
        measure(
            "thread: send, SQLite (saved per step)",
            "turn",
            1.0,
            (n(20), n(300)),
            || {
                turn += 1;
                let thread = on_sqlite.thread(format!("t{turn}"));
                async move {
                    thread.send("hi").await.expect("send");
                }
            },
        )
        .await,
    );

    let (base, upstream) = gateway_base().await;
    let http = reqwest::Client::new();
    let body = json!({"model": "bench", "messages": [{"role": "user", "content": "hi"}]});
    let request =
        criogen_providers::ModelRequest::new(vec![criogen_providers::Message::user("hi")]);
    results.push(
        measure(
            "model call, direct (baseline)",
            "call",
            1.0,
            (n(200), n(2000)),
            || async {
                upstream.invoke(&request).await.expect("answer");
            },
        )
        .await,
    );
    results.push(
        measure(
            "gateway: chat completion over HTTP",
            "request",
            1.0,
            (n(200), n(2000)),
            || async {
                http.post(format!("{base}/v1/chat/completions"))
                    .json(&body)
                    .send()
                    .await
                    .expect("send")
                    .bytes()
                    .await
                    .expect("body");
            },
        )
        .await,
    );
    let mut streamed = body.clone();
    streamed["stream"] = json!(true);
    results.push(
        measure(
            "gateway: streamed chat completion",
            "request",
            1.0,
            (n(200), n(2000)),
            || async {
                http.post(format!("{base}/v1/chat/completions"))
                    .json(&streamed)
                    .send()
                    .await
                    .expect("send")
                    .bytes()
                    .await
                    .expect("body");
            },
        )
        .await,
    );

    println!("| Scenario | Runs | Median per run | p95 per run | Median per unit |");
    println!("|---|---|---|---|---|");
    for r in &results {
        let median = r.percentile(0.5);
        println!(
            "| {} | {} | {} | {} | {} per {} |",
            r.name,
            r.samples.len(),
            show(median),
            show(r.percentile(0.95)),
            show(median / r.per_run),
            r.unit
        );
    }
    drop(sqlite);
    for suffix in ["", "-wal", "-shm"] {
        let mut path = file.clone().into_os_string();
        path.push(suffix);
        let _ = std::fs::remove_file(path);
    }
}

/// Microseconds, readably.
fn show(us: f64) -> String {
    if us >= 1000.0 {
        format!("{:.2} ms", us / 1000.0)
    } else {
        format!("{us:.1} µs")
    }
}
