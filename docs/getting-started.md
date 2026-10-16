# Getting started

This guide builds up from a one-node workflow to an agent that calls tools,
asks a person before doing something risky, and keeps conversations in a
SQLite file. Every Rust example on this page is compiled and run by the
test suite, so it matches the current API.

AgentBuild is Rust-only today; Python and TypeScript SDKs are planned
([design 0007](design/0007-public-api.md)).

## Add the crates

Until the first release is on crates.io, depend on the repository:

```toml
[dependencies]
agentbuild-core = { git = "https://github.com/VARMAMASTER/agentbuild" }
agentbuild-providers = { git = "https://github.com/VARMAMASTER/agentbuild" }
agentbuild-agent = { git = "https://github.com/VARMAMASTER/agentbuild", features = ["sqlite"] }
agentbuild-storage = { git = "https://github.com/VARMAMASTER/agentbuild", features = ["sqlite"] }
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

| Crate | What it gives you |
|---|---|
| `agentbuild-core` | The workflow engine: nodes, edges, routers, state, suspend/resume, retries |
| `agentbuild-providers` | Messages, tools and models: OpenAI-compatible servers, Anthropic, Gemini, Ollama, vLLM, LM Studio |
| `agentbuild-agent` | Agents: the model ⇄ tools loop, approvals, structured output, hooks, threads |
| `agentbuild-storage` | Where threads live: in memory, SQLite (`sqlite` feature) or Postgres (`postgres` feature) |
| `agentbuild-telemetry` | OpenTelemetry / Langfuse export |

Rust 1.88 or newer.

## 1. A workflow

A workflow is a graph of async nodes. Each node reads the shared state and
returns the keys it changes.

```rust
use agentbuild_core::{END, State, Update, WorkflowBuilder, node_fn};

# tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
let mut builder = WorkflowBuilder::new();
builder
    .add_node("greet", node_fn(|state| async move {
        let name = state.get("name").and_then(|v| v.as_str()).unwrap_or("world");
        Ok(Update::new().set("greeting", format!("Hello, {name}!")))
    }))
    .set_entry("greet")
    .add_edge("greet", END);

let workflow = builder.build().unwrap(); // every graph problem is reported here
let output = workflow.run(State::new().with("name", "Ada")).await.unwrap();
assert_eq!(output.state.get("greeting").unwrap(), "Hello, Ada!");
# });
```

Workflows also do branching with routers, parallel nodes and joins, loops,
per-node timeouts and retries, cancellation, live events and pausing for a
person. See [the core crate](../crates/agentbuild-core/README.md) and
[the execution model](design/0001-execution-model.md).

## 2. An agent with a tool

An agent is a model plus tools, run as a loop: the model answers or asks for
tool calls, the tools run (concurrently, with validated arguments), and the
results go back to the model.

With a real model:

```rust,no_run
use agentbuild_agent::{Agent, ToolError, tool_fn};
use agentbuild_providers::{Endpoint, ProviderModel, ToolSpec};
use serde_json::json;

# async fn demo() -> Result<(), Box<dyn std::error::Error>> {
let model = ProviderModel::new(Endpoint::openai(std::env::var("OPENAI_API_KEY")?), "gpt-5-mini")?;
// Or: Endpoint::anthropic(key), Endpoint::gemini(key), Endpoint::ollama(),
//     Endpoint::vllm(url, key), Endpoint::openai_compatible(name, url, key).

let weather = tool_fn(
    ToolSpec::new(
        "get_weather",
        "Current temperature in a city, in Celsius.",
        json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
        }),
    ),
    |args, _ctx| async move { Ok::<_, ToolError>(json!({"city": args["city"], "celsius": 17})) },
);

let agent = Agent::builder(model)
    .instructions("You are a helpful travel assistant.")
    .tool(weather)
    .build()?;
let result = agent.run("How warm is it in Paris?").await?;
println!("{}", result.text().unwrap_or_default());
# Ok(()) }
```

The same agent with `FakeModel`, which plays back scripted replies. Use it
in tests: no network, no cost, exact behaviour.

```rust
use agentbuild_agent::{Agent, ToolError, tool_fn};
use agentbuild_providers::{FakeModel, FakeReply, ToolSpec};
use serde_json::json;

# tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap().block_on(async {
let model = FakeModel::new()
    .then(FakeReply::tool_call("get_weather", json!({"city": "Paris"})))
    .then(FakeReply::text("It is 17 °C in Paris."));
let weather = tool_fn(
    ToolSpec::new("get_weather", "Current temperature", json!({
        "type": "object",
        "properties": {"city": {"type": "string"}},
        "required": ["city"]
    })),
    |args, _ctx| async move { Ok::<_, ToolError>(json!({"city": args["city"], "celsius": 17})) },
);
let agent = Agent::builder(model).tool(weather).build().unwrap();

let result = agent.run("How warm is it in Paris?").await.unwrap();
assert_eq!(result.text(), Some("It is 17 °C in Paris."));
assert_eq!((result.model_calls, result.tool_calls), (2, 1));
# });
```

Bad arguments never reach your tool: they are checked against the schema and
sent back to the model to fix. A `ToolError` becomes a result the model can
react to; `ToolError::fatal` stops the run.

## 3. Ask a person first

Gate risky tools with an `Approval`. The run pauses before **any** call of
that turn runs, and returns the requests. Continue with a decision per call:
approve, reject (with a reason the model sees), edit the arguments, or
respond in the tool's place.

```rust
use agentbuild_agent::{Agent, Approval, Decision, ToolError, tool_fn};
use agentbuild_providers::{FakeModel, FakeReply, ToolSpec};
use serde_json::json;

# tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap().block_on(async {
let model = FakeModel::new()
    .then(FakeReply::tool_call("refund", json!({"order": "A-1001", "amount": 24})))
    .then(FakeReply::text("Your refund is on its way."));
let refund = tool_fn(
    ToolSpec::new("refund", "Refund an order", json!({"type": "object"})),
    |_args, _ctx| async { Ok::<_, ToolError>("refunded") },
);
let agent = Agent::builder(model)
    .tool(refund)
    .approval(Approval::new().tool("refund"))
    .build()
    .unwrap();

let paused = agent.run("My mug arrived broken").await.unwrap();
assert!(paused.is_suspended()); // nothing was refunded yet
let request = &paused.approval_requests()[0];
assert_eq!(request.args["amount"], 24);

// Later, possibly in another process: the snapshot is plain JSON.
let decisions = [(request.call_id.clone(), Decision::approve())];
let done = agent.resume(paused.snapshot().unwrap(), decisions).await.unwrap();
assert_eq!(done.text(), Some("Your refund is on its way."));
# });
```

## 4. Conversations that survive restarts

A **thread** is one conversation. Each call saves checkpoints to the agent's
storage: before every step and after every tool call. So:

- the next message continues the conversation;
- a pending approval waits in storage until anyone calls `approve()`, from
  any process;
- if the process dies mid-run, `retry()` continues where it stopped, and
  finished tool calls are not run again.

```rust
use std::time::Duration;

use agentbuild_agent::Agent;
use agentbuild_providers::{FakeModel, FakeReply};
use agentbuild_storage::Storage;

# tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
// A file in real use: Storage::connect("sqlite://agent.db"). Tables are
// created on first use.
let storage = Storage::connect("sqlite::memory:").await.unwrap();
// Answers at once (FakeModel::new() waits 2–3 s per call, like a real model).
let model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO)
    .then(FakeReply::text("Nice to meet you, Ada."))
    .then(FakeReply::text("Your name is Ada."));
let agent = Agent::builder(model).storage(storage).build().unwrap();

let chat = agent.thread("user-42");
chat.send("Hi, I am Ada").await.unwrap();
let reply = chat.send("What is my name?").await.unwrap();
assert_eq!(reply.text(), Some("Your name is Ada."));
assert_eq!(chat.messages().await.unwrap().len(), 4);

// Every checkpoint is listed, newest first; `chat.rewind(id)` goes back.
let history = chat.history(10).await.unwrap();
assert_eq!(history.len(), 4);
# });
```

A thread waiting for approval answers `chat.approve()`, `chat.reject(reason)`
or `chat.decide(decisions)`. A thread whose last run failed or was
interrupted answers `chat.retry()`; `send` refuses until then
(`thread_interrupted`). Two `send`s on one thread queue up by default.

**Several processes or machines?** Use Postgres. Turn on the
`postgres` feature (on `agentbuild-storage` and `agentbuild-agent`) and
connect with
`Storage::connect("postgres://app:secret@db.internal/agents?sslmode=verify-full")`.
Tables are created on first use, and threads and memory share the
database. The busy-thread rules hold across processes: a second `send`
waits even when another server runs the first. Use a direct connection,
or PgBouncer in session mode.

**Find facts by meaning.** Keyword search finds "penicillin" only for
"penicillin". With an embedding model, a question such as "Which
antibiotics should I avoid?" finds it too:

```rust,ignore
let embedder = ProviderEmbedder::new(Endpoint::openai(key), "text-embedding-3-small")?;
let agent = Agent::builder(model)
    .storage(storage.clone())
    .memory(Memory::by_state("patient", patients).semantic(embedder.clone()))
    .build()?;
// Facts the app saves itself need a vector too:
put_embedded(storage.memory().as_ref(), &embedder, &patient, "allergy",
    json!("Severe penicillin reaction"), None).await?;
```

Facts the model saves with `remember` get their vector automatically. A
self-hosted embedding model works the same way
(`ProviderEmbedder::new(Endpoint::ollama(), "nomic-embed-text")`).

**Workflows that wait for a person.** Any workflow gets the same
durable threads. A node that returns `NodeOutput::suspend(...)` pauses the
run; it waits in storage until someone answers, from any process:

```rust,ignore
let thread = storage.workflow_thread(&expenses, "expense-1");
let turn = thread.run(State::new().with("amount", json!(120))).await?;
if turn.is_suspended() { /* show turn.interrupts() to a manager */ }
// later, elsewhere:
storage.workflow_thread(&expenses, "expense-1").actor("user:boss")
    .resume([("approve", json!("yes"))]).await?;
```

**Who did what?** Turn on the audit trail and name who acts:

```rust,ignore
let storage = Storage::connect("sqlite://agent.db").await?.with_audit();
// … build the agent with this storage …
agent.thread("visit-7f3a").actor("user:dr-rao").approve().await?;
let records = storage.audit().unwrap().query(&AuditQuery::new().thread("visit-7f3a")).await?;
assert!(storage.audit().unwrap().verify().await?.ok);
```

Runs, model and tool calls, approval requests and decisions, rewinds,
deletions and memory writes are recorded, without message text, tool
arguments or results. Records are chained by hash, so `verify` finds a
changed or removed record. If a record cannot be written the action fails
(`audit_failed`).

**Patient data, or anything else sensitive?** Turn on the `encryption`
feature and open storage with keys:

```rust,ignore
// AGENTBUILD_ENCRYPTION_KEYS="k1:<64 hex digits>"  (openssl rand -hex 32)
let keys = Keyring::from_env("AGENTBUILD_ENCRYPTION_KEYS")?;
let storage = Storage::connect_encrypted("sqlite://agent.db", keys)
    .await?
    .retention(Retention::new().thread_ttl(Duration::from_secs(30 * 86_400)));
```

Messages, approval requests, tool results and remembered facts are then
encrypted in the database. Once encrypted, the database refuses to open
without its keys, so an app started without them never writes plain text
into it. Thread ids and memory namespaces stay readable: use opaque ids.
To rotate, put a new key first (`k2:…,k1:…`) and call
`storage.reencrypt()`. Retention deletes threads idle for 30 days when
`storage.prune()` runs (from a cron job) or in the background with
`storage.spawn_pruner(every)`.

**Stream a turn to a UI.** `chat.stream(input)` gives the events of
`send` as they happen, then the saved reply:

```rust,ignore
let mut stream = chat.stream("Anything else?");
while let Some(event) = stream.next().await {
    if let AgentEvent::Text { text, .. } = event {
        print!("{text}");
    }
}
let reply = stream.finish().await?;
```

If the client goes away and the stream is dropped, the run is cancelled
(the model call stops at once) and saved as failed; `chat.retry()`
continues it. `stream_approve`, `stream_reject`, `stream_decide` and
`stream_retry` stream the other operations.

Testing tip: SQLite storage needs Tokio's real clock. Under a paused test
clock (`start_paused`), the database pool's connection timeout fires at
once; use `Storage::in_memory()` there, or real time with a fast
`FakeModel` as above.

## 5. Show progress as it happens

`agent.stream(input)` yields typed events for a UI: text as it is written,
tool calls starting and finishing, approvals needed.

```rust
use agentbuild_agent::{Agent, AgentEvent};
use agentbuild_providers::{FakeModel, FakeReply};

# tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap().block_on(async {
let agent = Agent::builder(FakeModel::new().then(FakeReply::text("Hello!")))
    .build()
    .unwrap();
let mut stream = agent.stream("Say hello");
let mut text = String::new();
while let Some(event) = stream.next().await {
    if let AgentEvent::Text { text: piece, .. } = event {
        text.push_str(&piece);
    }
}
let result = stream.finish().await.unwrap();
assert_eq!(text, "Hello!");
assert_eq!(result.text(), Some("Hello!"));
# });
```

## 6. Structured output

Make the run end with JSON matching a schema. Invalid answers go back to the
model with the validation errors. A model with a JSON mode (OpenAI, Gemini)
answers in it; others call a reserved `final_output` tool. AgentBuild picks
from the model's capabilities, or you choose with `output_with`.

```rust
use agentbuild_agent::Agent;
use agentbuild_providers::{FakeModel, FakeReply};
use serde_json::json;

# tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap().block_on(async {
// FakeModel has no JSON mode, so it answers through the `final_output` tool.
let model = FakeModel::new().then(FakeReply::tool_call(
    "final_output",
    json!({"city": "Paris", "celsius": 17}),
));
let agent = Agent::builder(model)
    .output(json!({
        "type": "object",
        "properties": {"city": {"type": "string"}, "celsius": {"type": "number"}},
        "required": ["city", "celsius"]
    }))
    .build()
    .unwrap();
let result = agent.run("Weather in Paris as JSON").await.unwrap();
assert_eq!(result.output, Some(json!({"city": "Paris", "celsius": 17})));
# });
```

## 7. Remember users across conversations

Threads remember one conversation. **Long-term memory** keeps facts about a
user (or a patient, a customer, any namespace) across all of their
conversations, in the same storage. Each run works in one namespace,
chosen from a value you pass with the input; a run without it fails rather
than seeing someone else's facts.

```rust
use std::sync::Arc;
use std::time::Duration;

use agentbuild_agent::{Agent, AgentInput, Memory, REMEMBER_TOOL};
use agentbuild_providers::{FakeModel, FakeReply};
use agentbuild_storage::{Namespace, Storage};
use serde_json::json;

# tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let storage = Storage::connect("sqlite::memory:").await.unwrap();
let model = Arc::new(
    FakeModel::with_delay(Duration::ZERO, Duration::ZERO)
        // The model saves a fact with the `remember` tool...
        .then(FakeReply::tool_call(REMEMBER_TOOL, json!({"key": "diet", "value": "vegetarian"})))
        .then(FakeReply::text("Noted!"))
        .then(FakeReply::text("How about a lentil curry?")),
);
let agent = Agent::builder_shared(model.clone())
    .storage(storage.clone())
    // Memory per user: users/<user_id>.
    .memory(Memory::by_state("user_id", Namespace::parse("users").unwrap()))
    .build()
    .unwrap();

agent.thread("monday").send(AgentInput::from("I'm vegetarian").with("user_id", "42")).await.unwrap();
// ...and a later conversation recalls it before the model is called.
agent.thread("friday").send(AgentInput::from("Dinner ideas for my diet?").with("user_id", "42")).await.unwrap();
assert!(model.requests()[2].messages[0].text().contains("- diet: vegetarian"));

// Apps read and write memory directly too, e.g. to delete everything about
// a user.
let user = Namespace::parse("users/42").unwrap();
assert_eq!(storage.memory().delete_namespace(&user).await.unwrap(), 1);
# });
```

Recall adds the best-matching facts (then the newest) to the model request
only; they are not stored in the transcript. Search is by keywords for
now. `Memory::shared(namespace)` gives every run the same memory, and
`.recall(0)` or `.tools(false)` turn either half off.

## Where next

- **More agent features:** hooks around every model and tool call, limits
  and token budgets, fallback models, agents as tools of other agents: the
  [agent crate](../crates/agentbuild-agent/README.md).
- **Tracing:** send every run to Langfuse or any OpenTelemetry backend:
  [agentbuild-telemetry](../crates/agentbuild-telemetry/README.md).
- **Examples you can run:** `cargo run -p agentbuild-agent --example
  approval_agent -- edit`, `thread_agent`, and `durable_agent` (a run killed
  mid-step and continued from SQLite, with `--features sqlite`).
- **A complete app:** [demos/support-chat](../demos/support-chat), a Rust
  server and a Next.js chat UI with approvals and crash recovery.
- **How it works:** [architecture](architecture.md), [glossary](glossary.md),
  and the design documents in [docs/design](design/).
