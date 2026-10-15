# AgentBuild

An AI agent orchestration framework with a Rust execution runtime.

Today AgentBuild is a set of **Rust crates** for building AI agents and
workflows: a workflow engine, a model layer (OpenAI-compatible servers,
Anthropic, Gemini, Ollama and other self-hosted models), agents with tools
and human approval, conversations stored in memory, SQLite or Postgres
with crash recovery, and OpenTelemetry export.

**Planned, not built yet:** Python and TypeScript SDKs on the same Rust
engine (sketched in [docs/design/0007-public-api.md](docs/design/0007-public-api.md)),
and an embeddable LLM gateway. There are no performance claims until benchmarks are in this
repository.

> **Status: early development (0.1, unreleased).** The API may change
> between 0.x releases. Nothing has been published yet. To get started,
> read [docs/getting-started.md](docs/getting-started.md).

## What works today

The `agentbuild-core` crate runs workflow graphs:

- named async nodes that read a JSON state and return partial updates
- static edges, fan-out, and fan-in
- conditional routing via async routers
- joins that wait for branches of different lengths
- cycles, bounded by a step limit
- per-key state reducers: replace, append, upsert-by-id
- nodes that pick their next step (`Command` goto, validated)
- **human-in-the-loop**: a node can suspend the run; the run returns a
  JSON snapshot that can be stored anywhere and resumed later
- **live events**: `workflow.stream(input)` reports run, step and node
  progress as it happens; nodes can emit their own events
- **reliability policies**: per-node timeouts and retries with backoff, a
  run deadline, and graceful cancellation
- **recovery without repeats**: a failed run returns a JSON snapshot;
  `workflow.retry(snapshot)` continues it after a fix, without re-running
  nodes that finished or work saved with `ctx.memo()` (e.g. a sent email)
- concurrent execution of nodes scheduled in the same step
- graph validation that reports every issue at once
- structured errors with stable codes; failed steps are rolled back

The `agentbuild-providers` crate holds the language-neutral data contracts
for agents (design 0002): messages with ordered content blocks, tool
definitions, the model request and response, classified model errors (rate
limits, overload, context overflow, ... from any provider's HTTP error), and
transcript repair. It speaks two wire protocols, with streaming and tool
calls: OpenAI-compatible (OpenAI, OpenRouter, Ollama, LM Studio, vLLM and
most gateways) and Anthropic Messages (Claude, natively: reasoning
signatures, prompt caching, citations and hosted tools survive a round
trip). A `FakeModel` simulates a model for tests.

Terms and naming conventions: [docs/glossary.md](docs/glossary.md).

The `agentbuild-agent` crate runs agents: a model with tools, as a
`model` ⇄ `tools` loop on the workflow engine. Tool arguments are validated
against their schemas, calls run concurrently with limits and timeouts, and a
failed run can be retried without repeating tool calls that already ran.
Tool calls can require a person's approval: the run pauses before anything
runs, and continues with approve, edit, reject or respond. An agent can be
required to end with JSON matching a schema (structured output), with
invalid answers sent back to the model to fix. Hooks run code around every
model call and tool call (summaries, guardrails, caching, retries) without
changing the graph. Model calls retry with the provider's requested wait
and can fall back to other models. An agent can be one node of a larger
workflow, or a tool of another agent, and `agent.stream()` gives a UI the
answer and tool calls as they happen.

Agents keep conversations in **threads** (`agentbuild-storage`):
`agent.thread("user-42").send("...")` remembers earlier messages, a pending
approval waits in storage until any agent instance calls `approve()`, a
failed run can be retried, and history can be listed and rewound. Commits
are compare-and-set, and a second message on a busy thread waits its turn.
Storage is in memory by default or a SQLite file (feature `sqlite`:
`Storage::connect("sqlite://agent.db")`). Runs are saved after every step
and every tool call, so a run killed mid-way continues with
`thread.retry()` without repeating finished tool calls. SQLite stores only
what changed per step (plus a full copy every 25), so a long chat stores
each message about once. For services that run several processes or
machines there is **Postgres** (feature `postgres`:
`Storage::connect("postgres://…")`). It uses the same format, adds TLS,
and keeps the busy-thread rules across processes: a second message waits
its turn even when another server is running the first.

Agents also get **long-term memory**: facts kept per user (or per patient,
or any namespace) across conversations, in the same storage.
`Memory::by_state("user_id", users)` recalls matching facts before each
model call and gives the model `remember` / `forget` / `search_memory`
tools. Search is by keywords for now; semantic search is planned.

The `agentbuild-telemetry` crate sends every run to
[Langfuse](https://langfuse.com) or any OpenTelemetry backend: runs are
traces, model calls are generations with token usage, tool calls are tool
observations. It never slows or breaks a run, and content capture can be
turned off or masked for sensitive data.

The execution semantics are specified in
[docs/design/0001-execution-model.md](docs/design/0001-execution-model.md).

## Quick start

Requires Rust 1.88+ ([rustup](https://rustup.rs)).

```sh
cargo test
cargo run -p agentbuild-core --example basic_workflow -- "Hello world"
cargo run -p agentbuild-core --example approval_workflow -- yes
cargo run -p agentbuild-core --example llm_workflow -- "remote work"   # simulated LLM calls, 2–3 s each
cargo run -p agentbuild-core --example retry_workflow                  # retries and timeouts
cargo run -p agentbuild-core --example recover_workflow                # retry a failed run, no repeats
cargo run -p agentbuild-agent --example approval_agent -- edit         # a doctor edits an order before it runs
cargo run -p agentbuild-agent --example thread_agent                   # a conversation and an approval kept in a thread
cargo run -p agentbuild-agent --features sqlite --example durable_agent # a run killed mid-step, continued from SQLite
```

A complete demo app, [demos/support-chat](demos/support-chat): a support agent
on a small Rust server (threads in SQLite, refund approvals, crash recovery,
OpenAI or Gemini per message) with a Next.js chat UI. It calls real models,
so it is run by hand:

```sh
cd demos/support-chat/server && cargo run      # keys from ~/.agentbuild-keys.env
cd demos/support-chat/web && npm install && npm run dev
bash scripts/test-package.sh                                           # test the packaged crates as dependencies
cargo run -p agentbuild-providers --example chat -- "hi"               # real model via Ollama (or AGENTBUILD_BASE_URL)
```

The example is `Input → Transform → Conditional Branch → Output`:

```rust
use agentbuild_core::{END, NodeError, State, Update, WorkflowBuilder, node_fn, router_fn};

let mut builder = WorkflowBuilder::new();
builder
    .add_node("normalize", node_fn(|s| async move { /* ... */ Ok(Update::new().set("word_count", 2)) }))
    .add_router(
        "normalize",
        router_fn(|s| async move {
            let long = s.get("word_count").and_then(|v| v.as_u64()).unwrap_or(0) > 5;
            Ok::<_, NodeError>(if long { "long" } else { "short" })
        }),
        [("long", "summarize"), ("short", "echo")],
    )
    .add_node("summarize", node_fn(|_| async { Ok(Update::new().set("output", "...")) }))
    .add_node("echo", node_fn(|_| async { Ok(Update::new().set("output", "...")) }))
    .set_entry("normalize")
    .add_edge("summarize", END)
    .add_edge("echo", END);

let workflow = builder.build()?;          // validation errors are reported here
let output = workflow.run(State::new().with("text", "Hello world")).await?;
```

## Roadmap

| Phase | Scope                                                         | Status      |
|-------|---------------------------------------------------------------|-------------|
| 1     | Rust execution core                                           | Done (0.1)  |
| 2     | Python SDK (PyO3 + maturin)                                   | Planned     |
| 3     | TypeScript SDK (NAPI-RS)                                      | Planned     |
| 4     | Agents, tools, model providers (LLM gateway: planned)         | Done (0.1)  |
| 5     | Persistence (threads, SQLite, Postgres, crash recovery, long-term memory with keyword and semantic search, retention, encryption at rest; vector indexes: planned) | Done (0.1)  |
| 6     | Docs, publishing, benchmarks                                  | In progress |

## Documentation

- [Getting started](docs/getting-started.md): a first workflow, agent, approval and conversation
- [Changelog](CHANGELOG.md), [Contributing](CONTRIBUTING.md), [Security](SECURITY.md)
- [Architecture](docs/architecture.md): how the system is put together and how a run executes
- [Execution model](docs/design/0001-execution-model.md): exact runtime semantics
- [Progress log](docs/progress.md): status, decisions, next steps
- [AGENTS.md](AGENTS.md): orientation for contributors and AI assistants

## Layout

```
crates/agentbuild-core/        Rust execution engine
crates/agentbuild-providers/   Model types, protocols and HTTP providers
crates/agentbuild-agent/       Agents: the model ⇄ tools loop, threads
crates/agentbuild-storage/     Threads, checkpoints, storage backends
crates/agentbuild-telemetry/   OpenTelemetry and Langfuse export
docs/design/                   Design decisions and contracts
docs/research/                 Studies of existing frameworks
demos/support-chat/            Demo app: Rust server + Next.js UI (real models, manual)
tests/package/                 Uses the packaged crates from a separate project (CI)
tests/live/                    Every feature against real models (manual)
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
