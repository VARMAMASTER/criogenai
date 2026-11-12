# criogenai-agent

Agents for [CriogenAI](https://github.com/VARMAMASTER/criogen): a model
with tools, run as a loop on the CriogenAI workflow engine.

- the agent compiles into an ordinary workflow (`model` ⇄ `tools`), so
  streaming, deadlines, cancellation and snapshots work as for any workflow
- tool arguments are validated against the tool's JSON Schema; unknown
  tools, bad arguments, tool errors and timeouts go back to the model
- tool calls run concurrently (bounded) and their results keep call order
- every finished call is saved at once: retrying a failed run never repeats
  a call that already ran (for example, sending an email)
- limits on model calls, tool calls and tokens; transient model errors are
  retried, with fallback models
- human approval of tool calls (approve, edit, reject, respond), structured
  output, hooks around model and tool calls, agents as nodes and as tools,
  `agent.stream()` for UIs
- audit trail: with `Storage::with_audit`, runs, model and tool calls,
  approvals (who decided, before the call runs), rewinds, deletions and
  memory writes are recorded; `agent.thread(id).actor("user:dr-rao")`
  names who acts
- threads: `agent.thread("user-42").send(...)` keeps the conversation in
  storage (in memory, SQLite with the `sqlite` feature, or Postgres with
  `postgres`; encrypted at rest with `encryption`); pending
  approvals and failed runs wait there for `approve()` or `retry()` from
  any agent instance; `chat.stream(input)` streams a turn's events and
  still saves it, even when the client disconnects
- long-term memory: `Memory::by_state("user_id", users)` keeps facts per
  user across conversations (found by meaning with `.semantic(embedder)`),
  recalls them before each model call, and
  gives the model `remember` / `forget` / `search_memory`

> **Early development.** Nothing is published yet.

## Example

```rust
use criogenai_agent::{Agent, ToolError, tool_fn};
use criogenai_providers::{FakeModel, FakeReply, ToolSpec};
use serde_json::json;

#[tokio::main(flavor = "current_thread", start_paused = true)]
async fn main() {
    // A simulated model; use `ProviderModel` for a real one.
    let model = FakeModel::new()
        .then(FakeReply::tool_call("weather", json!({"city": "Paris"})))
        .then(FakeReply::text("It is 21 °C in Paris."));
    let weather = tool_fn(
        ToolSpec::new("weather", "Current temperature", json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
        })),
        |args, _ctx| async move {
            Ok::<_, ToolError>(json!({"city": args["city"], "celsius": 21}))
        },
    );

    let agent = Agent::builder(model).tool(weather).build().unwrap();
    let result = agent.run("Weather in Paris?").await.unwrap();
    assert_eq!(result.text(), Some("It is 21 °C in Paris."));
}
```

## Threads

```rust
use criogenai_agent::Agent;
use criogenai_providers::{FakeModel, FakeReply};

# tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap().block_on(async {
let model = FakeModel::new()
    .then(FakeReply::text("Hello Ada"))
    .then(FakeReply::text("Your name is Ada"));
let agent = Agent::builder(model).build().unwrap(); // in-memory storage by default

let chat = agent.thread("user-42");
chat.send("I am Ada").await.unwrap();
let reply = chat.send("What is my name?").await.unwrap();
assert_eq!(reply.text(), Some("Your name is Ada"));
// Each call saved a checkpoint before its first step and one at the end.
assert_eq!(chat.history(10).await.unwrap().len(), 4);
# });
```

For storage that survives restarts, enable the `sqlite` feature and pass
`Storage::connect("sqlite://agent.db").await?` to `AgentBuilder::storage`.
Runs are saved after every step and every tool call, so a run interrupted
by a crash continues with `thread.retry()` without repeating finished tool
calls.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
