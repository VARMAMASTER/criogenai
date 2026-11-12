# CriogenAI

AI agents and workflows on a Rust engine, used from Python or TypeScript.
Tools, human approval, conversations that survive crashes, memory, an audit
trail and one gateway for every model provider.

[Website](https://criogenai.com) ·
[Docs](https://criogenai.com/docs/) ·
[Quickstart](https://criogenai.com/docs/quickstart/) ·
[API reference](https://criogenai.com/docs/reference/api/) ·
[Benchmarks](https://criogenai.com/benchmarks/)

```sh
pip install criogenai      # Python 3.9+
npm install criogenai      # Node.js 18+, also in the browser as "criogenai/web"
```

> **Platforms in 0.1.7:** engines for Windows x64 and the browser. The Linux
> and macOS engines follow with the next build, planned for 16 November;
> until then those systems use the previous package name, `criogen` 0.1.6
> (`pip install criogen`, `import criogen`). The packages were renamed from
> `criogen` to `criogenai` in 0.1.7.

## A first agent

```python
import asyncio
from criogenai import Agent, ToolError, tool


@tool
async def get_order(order_id: str) -> dict:
    """Look up an order by id."""
    if order_id != "A-1001":
        raise ToolError(f"No order {order_id}")  # the model sees this and can recover
    return {"status": "shipped", "eta": "tomorrow"}


agent = Agent(
    "openai:gpt-5-mini",  # key from OPENAI_API_KEY
    instructions="You are a support agent. Be brief.",
    tools=[get_order],
)


async def main() -> None:
    result = await agent.run("Where is order A-1001?")
    print(result.text)


asyncio.run(main())
```

The same in TypeScript:

```ts
import { Agent, ToolError, tool } from "criogenai";

const getOrder = tool({
  name: "get_order",
  description: "Look up an order by id.",
  input: { type: "object", properties: { orderId: { type: "string" } }, required: ["orderId"] },
  run: async ({ orderId }) => {
    if (orderId !== "A-1001") throw new ToolError(`No order ${orderId}`);
    return { status: "shipped", eta: "tomorrow" };
  },
});

const agent = new Agent({
  model: "openai:gpt-5-mini",
  instructions: "You are a support agent. Be brief.",
  tools: [getOrder],
});
console.log((await agent.run("Where is order A-1001?")).text);
```

Next: the [tutorial](https://criogenai.com/docs/tutorial/support-agent/)
builds a complete support agent with approval, a saved conversation and
memory, step by step.

## What it does

- **Agents and tools.** Functions become tools; their arguments are checked
  against a schema before your code runs. Tool calls of one turn run at the
  same time, with timeouts, limits and retries.
- **Human approval.** Mark the risky tools. A run stops before them and waits
  in the database until someone approves, edits or rejects the call, from
  any process.
- **Conversations that survive crashes.** Saved after every step and every
  tool call, in SQLite or Postgres. After a crash, `retry()` continues where
  the run stopped, without running finished tools again.
- **Workflows.** Graphs of steps and agents: routes, parallel branches,
  joins, loops and pauses for a person, plus ready-made multi-agent patterns.
- **Every model provider.** OpenAI (Responses API), Anthropic, Gemini, Azure
  OpenAI, Amazon Bedrock, Google Vertex AI, OpenRouter, DeepSeek, Mistral,
  xAI, Groq and more by name; Ollama, LM Studio, vLLM or any compatible
  server for self-hosted models. Retries and fallbacks between models.
- **Memory and search.** Long-term memory per user, and document collections
  searched by meaning, usable as agent tools.
- **Compliance.** Encryption at rest, a hash-chained audit trail without
  message content, retention rules, and redaction before anything reaches a
  provider.
- **Tools from MCP servers**, **evals** that fail CI when quality drops,
  **tracing** to Langfuse, OpenTelemetry or a local viewer, and an
  **LLM gateway** with one OpenAI-compatible endpoint for every model.

What is built and what is planned, per language:
[status](https://criogenai.com/docs/status/). Performance numbers, with how
they were measured: [benchmarks](https://criogenai.com/benchmarks/) and
[docs/benchmarks.md](docs/benchmarks.md).

## Rust

The engine is Rust, and the crates are usable directly. They are not on
crates.io yet, so depend on this repository:

```toml
[dependencies]
criogenai-agent = { git = "https://github.com/VARMAMASTER/criogenai", features = ["sqlite"] }
criogenai-providers = { git = "https://github.com/VARMAMASTER/criogenai" }
criogenai-storage = { git = "https://github.com/VARMAMASTER/criogenai", features = ["sqlite"] }
```

Reference: [Rust API](https://criogenai.com/docs/reference/api/rust/).

## Status

Early development: 0.1, and the API may change between 0.x releases.
Changes are listed in the [changelog](CHANGELOG.md).

## Repository

```
crates/criogenai-core/        the workflow engine (steps, routes, pauses, retries)
crates/criogenai-providers/   messages, tools, model protocols and providers
crates/criogenai-agent/       agents: the model and tools loop, threads, MCP, evals
crates/criogenai-storage/     threads, checkpoints, SQLite and Postgres, memory, audit
crates/criogenai-telemetry/   OpenTelemetry and Langfuse export
crates/criogenai-gateway/     the LLM gateway (library and binary)
crates/criogenai-trace/       the local trace viewer
crates/criogenai-rt/          tasks and timers on servers and in the browser
crates/criogenai-js/          the bridge under the Node.js addon and the WebAssembly build
sdks/python/                  the Python package (PyO3, maturin)
sdks/typescript/              the TypeScript package (napi-rs, WebAssembly)
website/                      criogenai.com: docs, API reference, benchmarks, playground
docs/                         architecture, design decisions, research notes
```

## Building and testing

Requires Rust 1.88+, Python 3.9+ and Node.js 18+.

```sh
cargo test --workspace --all-features   # the Rust crates
bash scripts/test-python.sh             # builds the wheel, installs it in a new venv, runs pytest
bash scripts/test-typescript.sh         # builds the npm package, installs it in a new project, runs the tests
bash scripts/ci-local.sh                # the whole pipeline (CI runs locally for now)
```

Tests never call a real model: they use a simulated one. How the system works:
[docs/architecture.md](docs/architecture.md). Notes for contributors and AI
assistants: [AGENTS.md](AGENTS.md).

## Contributing and security

Issues and pull requests are welcome; see [CONTRIBUTING.md](CONTRIBUTING.md).
Report security problems privately as described in [SECURITY.md](SECURITY.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
