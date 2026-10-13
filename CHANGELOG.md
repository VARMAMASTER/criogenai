# Changelog

All notable changes to AgentBuild. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Before 1.0, a minor version
(0.x → 0.y) may contain breaking changes; patch versions do not.

All crates in this repository share one version number.

## [Unreleased]

First release, planned as 0.1.0 (Rust crates only).

### agentbuild-core: the workflow engine

- Workflows as graphs of async nodes over a JSON state, run in supersteps:
  static edges, fan-out and fan-in, async routers, joins, cycles bounded by
  a step limit, concurrent nodes in a step, atomic steps (a failed step
  commits nothing).
- Per-key reducers (`replace`, `append`, `upsert_by_id`); nodes that choose
  their next step (`Command` goto, validated against `may_goto`).
- Human-in-the-loop: a node can suspend the run; the run returns a JSON
  snapshot that can be resumed later, from any process.
- Live events (`Workflow::stream`, observers), node timeouts and retries
  with backoff, run deadlines, cancellation with a grace period.
- Recovery without repeats: failed runs return a snapshot; finished nodes
  and work saved with `ctx.memo()` are not repeated on retry.
- Durable runs: `RunOptions::persist` saves a checkpoint before every step
  and every finished task as it happens (`Persister`); `TaskMemo::save`.
- Graph validation that reports every issue at once; stable error codes.

### agentbuild-providers: models

- Language-neutral messages with ordered content blocks, tool definitions,
  model requests and responses, usage, transcript checks and repair.
- Protocols: OpenAI-compatible chat (OpenAI, Gemini, OpenRouter, Ollama,
  LM Studio, vLLM, most gateways) and Anthropic Messages (reasoning
  signatures, prompt caching, citations and hosted tools round-trip).
- Streaming over SSE with incremental assembly; HTTP transport with
  connect, first-byte and total timeouts and dispatch tracking.
- Classified model errors from any provider (rate limit, overload, context
  overflow, auth, quota, …), `Retry-After` support, API keys redacted from
  messages. Endpoint presets including `Endpoint::gemini`.
- Reasoning printed inline as `<think>…</think>` (reasoning models on
  servers without a reasoning parser, e.g. Ollama 0.6) becomes a reasoning
  block, also when streamed (`OpenAiQuirks::think_tags`, on by default).
- `FakeModel` for tests: scripted replies, realistic delays, errors.

### agentbuild-agent: agents

- The model ⇄ tools loop: argument validation against JSON Schema,
  concurrent tool calls with limits and timeouts, tool errors the model can
  react to, retries for safe tools, result truncation, state updates.
- Human approval of tool calls (approve, edit, reject, respond).
- Structured output (native JSON mode or a `final_output` tool).
- Hooks around the loop, limits and token budgets, model retries and
  fallback models, agents as workflow nodes and as tools, `agent.stream()`.
- Threads (`agent.thread(id)`): conversations kept in storage, with
  approvals that wait in storage, `retry` after failures or crashes,
  history and rewind.

### agentbuild-storage: where threads live

- Checkpoints as plain JSON, compare-and-set commits, per-thread leases
  (`OnBusy`: wait, reject, interrupt), durability `step` (default) or
  `exit`.
- In-memory backend; SQLite backend (feature `sqlite`) with delta +
  keyframe checkpoints, automatic migrations and crash recovery.

### agentbuild-telemetry: tracing

- OpenTelemetry export over OTLP/HTTP (runs, steps, model generations, tool
  calls) with a Langfuse preset, content capture switch and masking, and a
  non-blocking observer.

### Testing

- Simulated models in all automated tests; a package test that uses the
  packaged crates from a separate project; a manual live check of every
  feature against real models (`tests/live`), passing on OpenAI, Gemini and
  Ollama.
