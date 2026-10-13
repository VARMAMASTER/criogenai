//! Live check: exercises every AgentBuild feature against real model APIs
//! and prints a PASS/FAIL table per provider.
//!
//! Usage: `cargo run -- [openai] [gemini]` (default: every provider with a
//! key). Keys come from `OPENAI_API_KEY` / `GEMINI_API_KEY`, or from the
//! file in `AGENTBUILD_KEYS_FILE` (default `~/.agentbuild-keys.env`).
//! Models: `OPENAI_MODEL` (default `gpt-5-mini`), `GEMINI_MODEL` (default
//! `gemini-2.5-flash`).
//!
//! Real models answer differently each time; the checks look for facts that
//! any correct answer contains (a number, a name, a tool call), not exact
//! text.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agentbuild_agent::{
    Agent, AgentEvent, AgentResult, Approval, Decision, Hook, HookAction, HookContext, HookError,
    Limits, OnLimit, StopReason, ToolCallRequest, ToolError, ToolNext, ToolOutput, tool_fn,
};
use agentbuild_core::{
    BoxFuture, END, NodeError, RunOptions, State, Update, WorkflowBuilder, node_fn, router_fn,
};
use agentbuild_providers::{
    BlockDelta, Endpoint, Message, MessageDelta, Model, ModelErrorKind, ModelRequest,
    OutputStrategy, ProviderModel, ToolSpec,
};
use agentbuild_storage::{CheckpointStatus, SqliteCheckpointer, Storage};
use agentbuild_telemetry::{OtlpConfig, OtlpExporter};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// How to make a model for one provider.
#[derive(Clone)]
struct Provider {
    name: &'static str,
    model: String,
    endpoint: Endpoint,
}

impl Provider {
    fn llm(&self) -> ProviderModel {
        ProviderModel::new(self.endpoint.clone(), &self.model).expect("HTTP client")
    }

    /// The same provider with a bad key (for error and fallback checks).
    fn broken(&self) -> ProviderModel {
        let mut endpoint = self.endpoint.clone();
        endpoint.api_key = Some("invalid-key-for-agentbuild-live-check".into());
        ProviderModel::new(endpoint, &self.model).expect("HTTP client")
    }
}

type Outcome = Result<String, String>;

fn main() {
    // Before the runtime starts any thread (`set_var` is not thread-safe).
    load_keys_file();
    let wanted: Vec<String> = std::env::args().skip(1).collect();
    let mut providers = Vec::new();
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        providers.push(Provider {
            name: "openai",
            model: std::env::var("OPENAI_MODEL").unwrap_or("gpt-5-mini".into()),
            endpoint: Endpoint::openai(key),
        });
    }
    if let Ok(key) = std::env::var("GEMINI_API_KEY") {
        providers.push(Provider {
            name: "gemini",
            model: std::env::var("GEMINI_MODEL").unwrap_or("gemini-2.5-flash".into()),
            endpoint: Endpoint::openai_compatible(
                "gemini",
                "https://generativelanguage.googleapis.com/v1beta/openai",
                Some(key),
            ),
        });
    }
    providers.retain(|p| wanted.is_empty() || wanted.iter().any(|w| w == p.name));
    if providers.is_empty() {
        eprintln!("no provider: set OPENAI_API_KEY and/or GEMINI_API_KEY");
        std::process::exit(2);
    }
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let failed = runtime.block_on(async {
        let mut failed = 0;
        for provider in &providers {
            failed += run_all(provider).await;
        }
        failed
    });
    std::process::exit(if failed == 0 { 0 } else { 1 });
}

async fn run_all(p: &Provider) -> usize {
    println!("\n=== {} ({}) ===", p.name, p.model);
    let checks: Vec<(&str, BoxFuture<'_, Outcome>)> = vec![
        ("chat: plain answer and usage", Box::pin(chat(p))),
        ("streaming: text arrives in pieces", Box::pin(streaming(p))),
        (
            "tools: one call, answer uses the result",
            Box::pin(one_tool(p)),
        ),
        (
            "tools: several calls in one turn",
            Box::pin(parallel_tools(p)),
        ),
        (
            "tools: error result, model tries again",
            Box::pin(tool_error_recovery(p)),
        ),
        (
            "tools: argument validation",
            Box::pin(argument_validation(p)),
        ),
        (
            "output: native JSON schema",
            Box::pin(structured(p, OutputStrategy::Native)),
        ),
        (
            "output: final_output tool",
            Box::pin(structured(p, OutputStrategy::Tool)),
        ),
        (
            "approval: approve runs the tool",
            Box::pin(approval(p, "approve")),
        ),
        (
            "approval: reject skips the tool",
            Box::pin(approval(p, "reject")),
        ),
        (
            "approval: edit changes the arguments",
            Box::pin(approval(p, "edit")),
        ),
        (
            "agent.stream: text and tool events",
            Box::pin(agent_stream(p)),
        ),
        ("threads: memory across turns", Box::pin(thread_memory(p))),
        (
            "threads: SQLite survives reopening",
            Box::pin(sqlite_reopen(p)),
        ),
        (
            "threads: crash mid-tool, retry",
            Box::pin(crash_recovery(p)),
        ),
        ("agents: agent as a tool", Box::pin(agent_as_tool(p))),
        ("hooks: before_model and wrap_tool", Box::pin(hooks(p))),
        ("limits: model-call limit ends the run", Box::pin(limits(p))),
        ("resilience: fallback model", Box::pin(fallback(p))),
        (
            "errors: bad key is classified as auth",
            Box::pin(bad_key(p)),
        ),
        ("workflow: LLM nodes with a router", Box::pin(workflow(p))),
        (
            "telemetry: spans exported over OTLP",
            Box::pin(telemetry(p)),
        ),
        ("deadline: run stops in time", Box::pin(deadline(p))),
    ];
    // LIVE_ONLY=text runs only the checks whose name contains it.
    let only = std::env::var("LIVE_ONLY").unwrap_or_default();
    let mut failed = 0;
    for (name, check) in checks {
        if !name.contains(only.as_str()) {
            continue;
        }
        let started = Instant::now();
        let outcome = match tokio::time::timeout(Duration::from_secs(180), check).await {
            Ok(outcome) => outcome,
            Err(_) => Err("timed out after 180 s".into()),
        };
        let secs = started.elapsed().as_secs_f64();
        match outcome {
            Ok(detail) => println!("PASS {name:<42} {secs:>5.1}s  {}", short(&detail)),
            Err(why) => {
                failed += 1;
                println!("FAIL {name:<42} {secs:>5.1}s  {}", short(&why));
            }
        }
    }
    println!("--- {}: {failed} failed", p.name);
    failed
}

fn short(text: &str) -> String {
    let one_line = text.replace('\n', " ");
    // LIVE_VERBOSE=1 prints details in full.
    if one_line.chars().count() > 110 && std::env::var_os("LIVE_VERBOSE").is_none() {
        format!("{}…", one_line.chars().take(110).collect::<String>())
    } else {
        one_line
    }
}

fn ensure(condition: bool, why: impl Into<String>) -> Result<(), String> {
    if condition { Ok(()) } else { Err(why.into()) }
}

fn answer(result: &AgentResult) -> String {
    result.text().unwrap_or_default().to_string()
}

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// A tool that records its calls.
fn weather(calls: &Arc<Mutex<Vec<Map<String, Value>>>>) -> impl agentbuild_agent::Tool {
    let calls = calls.clone();
    tool_fn(
        ToolSpec::new(
            "get_weather",
            "Current temperature in a city, in Celsius.",
            json!({
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }),
        ),
        move |args, _ctx| {
            let calls = calls.clone();
            async move {
                let city = args["city"].as_str().unwrap_or_default().to_lowercase();
                calls.lock().unwrap().push(args);
                let celsius = if city.contains("paris") {
                    17
                } else if city.contains("tokyo") {
                    23
                } else {
                    11
                };
                Ok::<_, ToolError>(json!({"city": city, "celsius": celsius}))
            }
        },
    )
}

// ---- checks ---------------------------------------------------------------

async fn chat(p: &Provider) -> Outcome {
    let agent = Agent::builder(p.llm()).build().map_err(err)?;
    let result = agent
        .run("What is 17 + 25? Reply with just the number.")
        .await
        .map_err(err)?;
    let text = answer(&result);
    ensure(text.contains("42"), format!("answer: {text}"))?;
    ensure(
        result.usage.input_tokens > 0 && result.usage.output_tokens > 0,
        "no token usage",
    )?;
    Ok(format!(
        "{text:?}, {} in / {} out tokens",
        result.usage.input_tokens, result.usage.output_tokens
    ))
}

async fn streaming(p: &Provider) -> Outcome {
    let model = p.llm();
    let request = ModelRequest::new(vec![Message::user(
        "Count from 1 to 30, separated by commas.",
    )]);
    let mut pieces = 0;
    let mut text = String::new();
    let mut sink = |delta: MessageDelta| {
        if let BlockDelta::Text { text: t } = delta.delta {
            pieces += 1;
            text.push_str(&t);
        }
    };
    let response = model.stream(&request, &mut sink).await.map_err(err)?;
    ensure(pieces > 1, format!("only {pieces} piece(s)"))?;
    ensure(
        text == response.message.text(),
        "deltas differ from the final message",
    )?;
    ensure(text.contains("30"), format!("text: {text}"))?;
    Ok(format!("{pieces} pieces, {} chars", text.len()))
}

async fn one_tool(p: &Provider) -> Outcome {
    let calls = Arc::default();
    let agent = Agent::builder(p.llm())
        .tool(weather(&calls))
        .build()
        .map_err(err)?;
    let result = agent
        .run("What's the temperature in Paris right now? Use the tool.")
        .await
        .map_err(err)?;
    let text = answer(&result);
    ensure(result.tool_calls >= 1, "no tool call")?;
    ensure(text.contains("17"), format!("answer: {text}"))?;
    Ok(format!(
        "{} model / {} tool calls: {text}",
        result.model_calls, result.tool_calls
    ))
}

async fn parallel_tools(p: &Provider) -> Outcome {
    let calls: Arc<Mutex<Vec<Map<String, Value>>>> = Arc::default();
    let agent = Agent::builder(p.llm())
        .tool(weather(&calls))
        .build()
        .map_err(err)?;
    let result = agent
        .run("Get the current temperature in Paris and in Tokyo (call the tool for each city), then tell me which is warmer.")
        .await
        .map_err(err)?;
    let text = answer(&result);
    let cities: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|a| a["city"].as_str().unwrap_or_default().to_lowercase())
        .collect();
    ensure(
        cities.iter().any(|c| c.contains("paris")) && cities.iter().any(|c| c.contains("tokyo")),
        format!("cities called: {cities:?}"),
    )?;
    ensure(
        text.to_lowercase().contains("tokyo"),
        format!("answer: {text}"),
    )?;
    let turns = result.model_calls;
    Ok(format!(
        "{} calls in {turns} model turns ({cities:?})",
        cities.len()
    ))
}

async fn tool_error_recovery(p: &Provider) -> Outcome {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = attempts.clone();
    let flaky = tool_fn(
        ToolSpec::new(
            "exchange_rate",
            "EUR to USD exchange rate.",
            json!({"type": "object", "properties": {}}),
        ),
        move |_args, _ctx| {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            async move {
                if n == 0 {
                    Err(ToolError::new(
                        "service temporarily busy, please call again",
                    ))
                } else {
                    Ok(json!({"eur_usd": 1.0873}))
                }
            }
        },
    );
    let agent = Agent::builder(p.llm())
        .instructions("If a tool returns an error that says to call again, call it again.")
        .tool(flaky)
        .build()
        .map_err(err)?;
    let result = agent
        .run("What is the EUR to USD rate?")
        .await
        .map_err(err)?;
    let text = answer(&result);
    ensure(
        attempts.load(Ordering::SeqCst) >= 2,
        "the model did not call again",
    )?;
    ensure(text.contains("1.08"), format!("answer: {text}"))?;
    let first = result
        .messages
        .iter()
        .find(|m| m.tool.as_deref() == Some("exchange_rate"))
        .and_then(|m| m.status);
    Ok(format!("first result {first:?}, then: {text}"))
}

async fn argument_validation(p: &Provider) -> Outcome {
    // The schema wants an integer 1–5. The model is asked for 9, gets an
    // `invalid_arguments` result, and must correct itself.
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
    let log = seen.clone();
    let rate = tool_fn(
        ToolSpec::new(
            "rate_order",
            "Record a star rating for an order.",
            json!({
                "type": "object",
                "properties": {"stars": {"type": "integer", "minimum": 1, "maximum": 5}},
                "required": ["stars"],
                "additionalProperties": false
            }),
        ),
        move |args, _ctx| {
            log.lock().unwrap().push(args["stars"].clone());
            async { Ok::<_, ToolError>("rating saved") }
        },
    );
    let agent = Agent::builder(p.llm())
        .instructions("Call tools exactly as the user asks first. If a tool rejects the arguments, fix them and try again.")
        .tool(rate)
        .build()
        .map_err(err)?;
    let result = agent
        .run("Call rate_order with stars = 9. If that is refused, use the highest allowed value instead.")
        .await
        .map_err(err)?;
    let rejected = result.messages.iter().any(|m| {
        m.error
            .as_ref()
            .is_some_and(|e| format!("{e:?}").contains("invalid_arguments"))
    });
    let stored = seen.lock().unwrap().clone();
    ensure(
        stored
            .iter()
            .all(|v| v.as_i64().is_some_and(|n| (1..=5).contains(&n))),
        format!("tool ran with invalid stars: {stored:?}"),
    )?;
    ensure(stored == [json!(5)], format!("stored ratings: {stored:?}"))?;
    Ok(format!(
        "invalid call rejected: {rejected}; tool ran with {stored:?}"
    ))
}

async fn structured(p: &Provider, strategy: OutputStrategy) -> Outcome {
    let schema = json!({
        "type": "object",
        "properties": {
            "name": {"type": "string"},
            "year": {"type": "integer"},
            "languages": {"type": "array", "items": {"type": "string"}}
        },
        "required": ["name", "year", "languages"],
        "additionalProperties": false
    });
    let agent = Agent::builder(p.llm())
        .output_with(schema, strategy)
        .build()
        .map_err(err)?;
    let result = agent
        .run("Describe the Rust programming language: its name, the year 1.0 was released, and two languages that influenced it.")
        .await
        .map_err(err)?;
    let output = result.output.clone().ok_or("no output")?;
    ensure(output["year"] == json!(2015), format!("output: {output}"))?;
    ensure(
        output["languages"]
            .as_array()
            .is_some_and(|l| !l.is_empty()),
        format!("output: {output}"),
    )?;
    Ok(output.to_string())
}

async fn approval(p: &Provider, decision: &str) -> Outcome {
    let sent: Arc<Mutex<Vec<Map<String, Value>>>> = Arc::default();
    let log = sent.clone();
    let email = tool_fn(
        ToolSpec::new(
            "send_email",
            "Send an email.",
            json!({
                "type": "object",
                "properties": {"to": {"type": "string"}, "subject": {"type": "string"}},
                "required": ["to", "subject"]
            }),
        ),
        move |args, _ctx| {
            log.lock().unwrap().push(args);
            async { Ok::<_, ToolError>("sent") }
        },
    );
    let agent = Agent::builder(p.llm())
        .tool(email)
        .approval(Approval::new().tool("send_email"))
        .build()
        .map_err(err)?;
    let paused = agent
        .run("Send an email now to bob@example.com with the subject 'Lunch on Friday'. Don't ask me anything; just call the tool.")
        .await
        .map_err(err)?;
    let requests = paused.approval_requests();
    ensure(
        paused.is_suspended(),
        format!("not suspended: {}", answer(&paused)),
    )?;
    ensure(
        sent.lock().unwrap().is_empty(),
        "the tool ran before approval",
    )?;
    let request = requests.first().ok_or("no request")?;
    let choice = match decision {
        "approve" => Decision::approve(),
        "reject" => Decision::reject("Do not email Bob today."),
        _ => {
            let mut args = request.args.clone();
            args.insert("to".into(), json!("alice@example.com"));
            Decision::edit(args)
        }
    };
    let decisions = requests.iter().map(|r| {
        let d = if r.call_id == request.call_id {
            choice.clone()
        } else {
            Decision::reject("one email only")
        };
        (r.call_id.clone(), d)
    });
    let done = agent
        .resume(paused.snapshot().ok_or("no snapshot")?, decisions)
        .await
        .map_err(err)?;
    let sent = sent.lock().unwrap().clone();
    match decision {
        "approve" => ensure(
            sent.len() == 1 && sent[0]["to"] == json!("bob@example.com"),
            format!("sent: {sent:?}"),
        )?,
        "reject" => ensure(sent.is_empty(), format!("sent: {sent:?}"))?,
        _ => {
            ensure(
                sent.len() == 1 && sent[0]["to"] == json!("alice@example.com"),
                format!("sent: {sent:?}"),
            )?;
            let result = done
                .messages
                .iter()
                .find(|m| m.tool.as_deref() == Some("send_email"))
                .map(|m| m.text())
                .unwrap_or_default();
            ensure(
                result.contains("approved this call with changes")
                    && result.contains("alice@example.com"),
                format!("tool result: {result}"),
            )?;
            let reply = answer(&done).to_lowercase();
            ensure(
                reply.contains("alice") && !reply.contains("wrong"),
                format!("the model misread the edit: {reply}"),
            )?;
        }
    }
    Ok(format!("{} email(s); then: {}", sent.len(), answer(&done)))
}

async fn agent_stream(p: &Provider) -> Outcome {
    let calls = Arc::default();
    let agent = Agent::builder(p.llm())
        .tool(weather(&calls))
        .build()
        .map_err(err)?;
    let mut stream =
        agent.stream("What's the temperature in Tokyo? Use the tool, then answer in one sentence.");
    let (mut texts, mut started, mut finished, mut messages) = (0, 0, 0, 0);
    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::Text { .. } => texts += 1,
            AgentEvent::ToolCallStarted { .. } => started += 1,
            AgentEvent::ToolCallFinished { .. } => finished += 1,
            AgentEvent::Message(_) => messages += 1,
            _ => {}
        }
    }
    let result = stream.finish().await.map_err(err)?;
    ensure(texts > 0, "no text events")?;
    ensure(
        started >= 1 && started == finished,
        format!("tool events {started}/{finished}"),
    )?;
    ensure(
        answer(&result).contains("23"),
        format!("answer: {}", answer(&result)),
    )?;
    Ok(format!(
        "{texts} text, {started} tool start/finish, {messages} message events"
    ))
}

async fn thread_memory(p: &Provider) -> Outcome {
    let agent = Agent::builder(p.llm()).build().map_err(err)?;
    let chat = agent.thread("memory");
    chat.send("Hi! My name is Ada and my favourite colour is teal.")
        .await
        .map_err(err)?;
    let reply = chat
        .send("What is my name and my favourite colour?")
        .await
        .map_err(err)?;
    let text = reply.text().unwrap_or_default().to_lowercase();
    ensure(
        text.contains("ada") && text.contains("teal"),
        format!("answer: {text}"),
    )?;
    let history = chat.history(20).await.map_err(err)?;
    Ok(format!(
        "{} messages, {} checkpoints",
        reply.result.messages.len(),
        history.len()
    ))
}

fn temp_db(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("agentbuild-live-{name}-{}.db", std::process::id()))
}

fn remove_db(path: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut file = path.as_os_str().to_owned();
        file.push(suffix);
        let _ = std::fs::remove_file(file);
    }
}

async fn sqlite_reopen(p: &Provider) -> Outcome {
    let path = temp_db(&format!("reopen-{}", p.name));
    let result = async {
        {
            let storage = Storage::new(SqliteCheckpointer::open(&path).await.map_err(err)?);
            let agent = Agent::builder(p.llm())
                .storage(storage)
                .build()
                .map_err(err)?;
            agent
                .thread("t")
                .send("Remember this code word: PELICAN-42.")
                .await
                .map_err(err)?;
        }
        let storage = Storage::new(SqliteCheckpointer::open(&path).await.map_err(err)?);
        let agent = Agent::builder(p.llm())
            .storage(storage)
            .build()
            .map_err(err)?;
        let reply = agent
            .thread("t")
            .send("What was the code word?")
            .await
            .map_err(err)?;
        let text = reply.text().unwrap_or_default().to_string();
        ensure(text.contains("PELICAN"), format!("answer: {text}"))?;
        Ok(format!("after reopening: {text}"))
    }
    .await;
    remove_db(&path);
    result
}

async fn crash_recovery(p: &Provider) -> Outcome {
    let path = temp_db(&format!("crash-{}", p.name));
    let charges = Arc::new(AtomicUsize::new(0));
    let shop = |model: ProviderModel, storage: Storage, hang: bool| {
        let counter = charges.clone();
        let charge = tool_fn(
            ToolSpec::new(
                "charge_card",
                "Charge the customer's card.",
                json!({"type": "object", "properties": {"amount": {"type": "number"}}, "required": ["amount"]}),
            ),
            move |_args, _ctx| {
                let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                async move { Ok::<_, ToolError>(format!("charged, receipt R-{n}")) }
            },
        );
        let ship = tool_fn(
            ToolSpec::new(
                "ship_order",
                "Ship the order.",
                json!({"type": "object", "properties": {}}),
            ),
            move |_args, _ctx| async move {
                if hang {
                    std::future::pending::<()>().await;
                }
                Ok::<_, ToolError>("shipped, tracking T-9")
            },
        );
        Agent::builder(model)
            .instructions(
                "To complete an order, call charge_card and ship_order together in the same turn.",
            )
            .tool(charge)
            .tool(ship)
            .storage(storage)
            .build()
            .expect("valid agent")
    };
    let result = async {
        let storage = Storage::new(SqliteCheckpointer::open(&path).await.map_err(err)?);
        let first = shop(p.llm(), storage.clone(), true);
        let run = tokio::spawn(async move {
            first.thread("order").send("Complete my $40 order.").await
        });
        // Wait until the charge is stored, then "kill the process".
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            if run.is_finished() {
                return Err("the run ended before it could be interrupted (the model did not call both tools at once?)".to_string());
            }
            let head = storage.checkpointer().load("order", None).await.map_err(err)?;
            let saved = head
                .and_then(|h| h.snapshot)
                .and_then(|s| s.pending_writes.get("tools").map(|w| w.memo.len()))
                .unwrap_or(0);
            if saved >= 1 && charges.load(Ordering::SeqCst) == 1 {
                break;
            }
            ensure(Instant::now() < deadline, "the charge was never saved")?;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        run.abort();
        let _ = run.await;
        drop(storage);

        let storage = Storage::new(SqliteCheckpointer::open(&path).await.map_err(err)?);
        let second = shop(p.llm(), storage, false);
        let chat = second.thread("order");
        let head = chat.head().await.map_err(err)?.ok_or("no thread")?;
        ensure(head.status() == CheckpointStatus::InProgress, format!("head: {:?}", head.status()))?;
        let refused = chat.send("hello?").await.map(|_| ()).unwrap_err();
        ensure(refused.code == "thread_interrupted", refused.code.clone())?;
        let done = chat.retry().await.map_err(err)?;
        let charged = charges.load(Ordering::SeqCst);
        ensure(charged == 1, format!("charged {charged} times"))?;
        Ok(format!("charged once; after retry: {}", done.text().unwrap_or_default()))
    }
    .await;
    remove_db(&path);
    result
}

async fn agent_as_tool(p: &Provider) -> Outcome {
    let calls = Arc::default();
    let forecaster = Agent::builder(p.llm())
        .instructions("You answer weather questions using get_weather. Reply in one sentence.")
        .tool(weather(&calls))
        .build()
        .map_err(err)?;
    let planner = Agent::builder(p.llm())
        .instructions("You plan trips. For any weather question, ask the weather_expert tool.")
        .tool(forecaster.as_tool("weather_expert", "Answers questions about current weather."))
        .build()
        .map_err(err)?;
    let result = planner
        .run("I'm going to Paris today. How warm is it there? Should I bring a jacket?")
        .await
        .map_err(err)?;
    let used = result
        .messages
        .iter()
        .any(|m| m.tool.as_deref() == Some("weather_expert"));
    ensure(used, "weather_expert was not called")?;
    ensure(
        !calls.lock().unwrap().is_empty(),
        "the inner agent did not use its tool",
    )?;
    ensure(
        answer(&result).contains("17"),
        format!("answer: {}", answer(&result)),
    )?;
    Ok(short(&answer(&result)))
}

struct Counting {
    model_calls: AtomicUsize,
    tool_calls: AtomicUsize,
}

impl Hook for Counting {
    fn name(&self) -> &str {
        "counting"
    }

    fn before_model<'a>(
        &'a self,
        _ctx: &'a HookContext,
    ) -> BoxFuture<'a, Result<HookAction, HookError>> {
        self.model_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(HookAction::proceed()) })
    }

    fn wrap_tool<'a>(
        &'a self,
        call: ToolCallRequest,
        next: ToolNext<'a>,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        self.tool_calls.fetch_add(1, Ordering::SeqCst);
        next.call(call)
    }
}

async fn hooks(p: &Provider) -> Outcome {
    let calls = Arc::default();
    let hook = Arc::new(Counting {
        model_calls: AtomicUsize::new(0),
        tool_calls: AtomicUsize::new(0),
    });
    struct Shared(Arc<Counting>);
    impl Hook for Shared {
        fn name(&self) -> &str {
            self.0.name()
        }
        fn before_model<'a>(
            &'a self,
            ctx: &'a HookContext,
        ) -> BoxFuture<'a, Result<HookAction, HookError>> {
            self.0.before_model(ctx)
        }
        fn wrap_tool<'a>(
            &'a self,
            call: ToolCallRequest,
            next: ToolNext<'a>,
        ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
            self.0.wrap_tool(call, next)
        }
    }
    let agent = Agent::builder(p.llm())
        .tool(weather(&calls))
        .hook(Shared(hook.clone()))
        .build()
        .map_err(err)?;
    let result = agent
        .run("What's the temperature in Paris? Use the tool.")
        .await
        .map_err(err)?;
    let (m, t) = (
        hook.model_calls.load(Ordering::SeqCst),
        hook.tool_calls.load(Ordering::SeqCst),
    );
    ensure(
        m as u32 == result.model_calls,
        format!("hook saw {m} model calls, run made {}", result.model_calls),
    )?;
    ensure(
        t as u32 == result.tool_calls && t >= 1,
        format!("hook saw {t} tool calls"),
    )?;
    Ok(format!("hook saw {m} model and {t} tool calls"))
}

async fn limits(p: &Provider) -> Outcome {
    let calls = Arc::default();
    let agent = Agent::builder(p.llm())
        .tool(weather(&calls))
        .limits(Limits::default().model_calls(1).on_limit(OnLimit::End))
        .build()
        .map_err(err)?;
    let result = agent
        .run("What's the temperature in Paris? Use the tool.")
        .await
        .map_err(err)?;
    ensure(
        result.model_calls == 1,
        format!("{} model calls", result.model_calls),
    )?;
    ensure(
        result.stop_reason == Some(StopReason::Limit),
        format!("stop reason {:?}", result.stop_reason),
    )?;
    let strict = Agent::builder(p.llm())
        .tool(weather(&calls))
        .limits(Limits::default().model_calls(1))
        .build()
        .map_err(err)?;
    let error = strict
        .run("What's the temperature in Paris? Use the tool.")
        .await
        .map(|_| ())
        .unwrap_err();
    ensure(error.code == "limit_exceeded", error.code.clone())?;
    Ok(format!(
        "End: stop {:?}; Error: {}",
        result.stop_reason, error.code
    ))
}

async fn fallback(p: &Provider) -> Outcome {
    let agent = Agent::builder(p.broken())
        .fallback(p.llm())
        .fallback_on([ModelErrorKind::Auth])
        .build()
        .map_err(err)?;
    let result = agent.run("Reply with the word 'ok'.").await.map_err(err)?;
    let text = answer(&result).to_lowercase();
    ensure(text.contains("ok"), format!("answer: {text}"))?;
    Ok(format!("primary failed (auth), fallback answered {text:?}"))
}

async fn bad_key(p: &Provider) -> Outcome {
    let agent = Agent::builder(p.broken()).build().map_err(err)?;
    let error = agent.run("hi").await.map(|_| ()).unwrap_err();
    ensure(error.code == "model_failed", error.code.clone())?;
    let kind = error
        .details
        .as_ref()
        .and_then(|d| d.pointer("/model_error/kind"))
        .cloned()
        .unwrap_or(Value::Null);
    ensure(
        kind == json!("auth"),
        format!("kind {kind}: {}", error.message),
    )?;
    ensure(
        !error.message.contains("invalid-key-for-agentbuild"),
        "the key leaked into the error message",
    )?;
    Ok(format!("model_failed / {kind}, key redacted"))
}

async fn workflow(p: &Provider) -> Outcome {
    let model: Arc<dyn Model> = Arc::new(p.llm());
    let ask = |prompt_key: &'static str, out: &'static str, model: Arc<dyn Model>| {
        node_fn(move |state: Arc<State>| {
            let model = model.clone();
            async move {
                let prompt = state
                    .get(prompt_key)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let response = model
                    .invoke(&ModelRequest::new(vec![Message::user(prompt)]))
                    .await
                    .map_err(|e| NodeError::new(e.to_string()))?;
                Ok(Update::new().set(out, response.message.text()))
            }
        })
    };
    let mut builder = WorkflowBuilder::new();
    builder
        .add_node(
            "classify",
            node_fn({
                let model = model.clone();
                move |state: Arc<State>| {
                    let model = model.clone();
                    async move {
                        let ticket = state.get("ticket").and_then(Value::as_str).unwrap_or_default();
                        let prompt = format!("Classify this support ticket as exactly one word, BILLING or TECHNICAL: {ticket}");
                        let response = model
                            .invoke(&ModelRequest::new(vec![Message::user(prompt)]))
                            .await
                            .map_err(|e| NodeError::new(e.to_string()))?;
                        let label = if response.message.text().to_uppercase().contains("BILLING") {
                            "billing"
                        } else {
                            "technical"
                        };
                        Ok(Update::new()
                            .set("category", label)
                            .set("reply_prompt", format!("Write a one-sentence {label} support reply to: {ticket}")))
                    }
                }
            }),
        )
        .add_node("billing", ask("reply_prompt", "reply", model.clone()))
        .add_node("technical", ask("reply_prompt", "reply", model.clone()))
        .set_entry("classify")
        .add_router(
            "classify",
            router_fn(|state: Arc<State>| async move {
                Ok(state.get("category").and_then(Value::as_str).unwrap_or("technical").to_string())
            }),
            [("billing", "billing"), ("technical", "technical")],
        )
        .add_edge("billing", END)
        .add_edge("technical", END);
    let workflow = builder.build().map_err(|e| e.to_string())?;
    let output = workflow
        .run(State::new().with(
            "ticket",
            "I was charged twice for my subscription this month.",
        ))
        .await
        .map_err(err)?;
    let category = output.state.get("category").cloned().unwrap_or_default();
    ensure(category == json!("billing"), format!("category {category}"))?;
    let reply = output
        .state
        .get("reply")
        .and_then(Value::as_str)
        .unwrap_or_default();
    ensure(!reply.is_empty(), "no reply")?;
    let path: Vec<String> = output.steps.iter().flat_map(|s| s.nodes.clone()).collect();
    Ok(format!("{path:?}: {reply}"))
}

/// A minimal OTLP/HTTP receiver: answers 200 and counts requests.
async fn otlp_receiver() -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let url = format!("http://{}/v1/traces", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 1 << 20];
                let mut read = 0;
                // Read headers, then the body by Content-Length.
                loop {
                    let Ok(n) = socket.read(&mut buffer[read..]).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    read += n;
                    let text = String::from_utf8_lossy(&buffer[..read]).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text
                            .lines()
                            .find_map(|l| {
                                l.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if read >= end + 4 + length {
                            break;
                        }
                    }
                }
                seen.fetch_add(1, Ordering::SeqCst);
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{}")
                    .await;
            });
        }
    });
    (url, count)
}

async fn telemetry(p: &Provider) -> Outcome {
    let (url, requests) = otlp_receiver().await;
    let exporter =
        OtlpExporter::start(OtlpConfig::new(url).service_name("live-check")).map_err(err)?;
    let calls = Arc::default();
    let agent = Agent::builder(p.llm())
        .tool(weather(&calls))
        .build()
        .map_err(err)?;
    agent
        .run_with(
            "What's the temperature in Paris? Use the tool.",
            agent.run_options().observe(exporter.observer()),
        )
        .await
        .map_err(err)?;
    exporter.flush().await;
    let stats = exporter.stats();
    ensure(
        stats.exported >= 3,
        format!("exported {} spans", stats.exported),
    )?;
    ensure(
        requests.load(Ordering::SeqCst) >= 1,
        "no request reached the receiver",
    )?;
    Ok(format!(
        "{} spans in {} request(s), {} failed",
        stats.exported,
        requests.load(Ordering::SeqCst),
        stats.failed
    ))
}

async fn deadline(p: &Provider) -> Outcome {
    let agent = Agent::builder(p.llm()).build().map_err(err)?;
    let started = Instant::now();
    let error = agent
        .run_with(
            "Write a 2000-word essay about the history of coffee.",
            RunOptions::default().deadline(Duration::from_millis(800)),
        )
        .await
        .map(|_| ())
        .unwrap_err();
    let took = started.elapsed();
    ensure(
        error.code == "deadline_exceeded",
        format!("{}: {}", error.code, error.message),
    )?;
    ensure(took < Duration::from_secs(8), format!("took {took:?}"))?;
    Ok(format!("stopped after {:.1} s", took.as_secs_f64()))
}

/// Reads `KEY=value` lines into the environment, for keys not already set.
fn load_keys_file() {
    let path = std::env::var("AGENTBUILD_KEYS_FILE").ok().or_else(|| {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()?;
        Some(format!("{home}/.agentbuild-keys.env"))
    });
    let Some(text) = path.and_then(|p| std::fs::read_to_string(p).ok()) else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let (key, value) = (key.trim(), value.trim().trim_matches('"'));
            if !key.is_empty() && !value.is_empty() && std::env::var_os(key).is_none() {
                // SAFETY: called from `main` before the runtime starts.
                unsafe { std::env::set_var(key, value) };
            }
        }
    }
}
