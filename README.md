# AgentBuild

An AI agent orchestration framework with a Rust execution runtime and
(planned) first-class Python and TypeScript SDKs.

AgentBuild is for **building AI agents and workflows** in Python or TypeScript,
on a fast Rust engine. It includes an embeddable **LLM gateway** that is also
usable on its own. The planned developer API is sketched in
[docs/design/0007-public-api.md](docs/design/0007-public-api.md).

> **Status: early development.** The Rust execution core exists, plus the
> foundation of the provider layer (message and tool types, model errors, a
> simulated model). There are no Python or TypeScript packages yet, no real
> LLM integrations, and no persistence. Nothing has been published.

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
transcript repair. It speaks the OpenAI-compatible protocol, which covers
OpenAI, OpenRouter, Ollama, LM Studio, vLLM and most gateways, with streaming
and tool calls. A `FakeModel` simulates a model for tests. The Anthropic
protocol comes later.

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
| 1     | Rust execution core                                           | In progress |
| 2     | Python SDK (PyO3 + maturin)                                   | Planned     |
| 3     | TypeScript SDK (NAPI-RS)                                      | Planned     |
| 4     | Agents, tools, providers (loop, tools, OpenAI protocol: done) | In progress |
| 5     | Persistence (events, retries, snapshots on failure: done)     | In progress |
| 6     | Docs, templates, publishing, benchmarks                       | Planned     |

## Documentation

- [Architecture](docs/architecture.md): how the system is put together and how a run executes
- [Execution model](docs/design/0001-execution-model.md): exact runtime semantics
- [Progress log](docs/progress.md): status, decisions, next steps
- [AGENTS.md](AGENTS.md): orientation for contributors and AI assistants

## Layout

```
crates/agentbuild-core/   Rust execution engine
docs/design/              Design decisions and contracts
docs/research/            Studies of existing frameworks
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
