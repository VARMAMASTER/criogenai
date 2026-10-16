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

### Workflow threads (design 0008 §18)

- `Storage::workflow_thread(&workflow, id)`: durable, leased, audited
  threads for any workflow. `run` continues from the stored state,
  `resume` answers the nodes that paused for a human (from any process),
  `retry` continues a failed run without repeating finished nodes; plus
  `cancel`, `interrupts`, `history`, `rewind`, `delete` and `actor`.

### Error catalog and JSON Schemas (design 0013)

- `schemas/error-codes.json`: every stable error code with its layer,
  category, retryability, SDK error class and gateway HTTP status;
  `docs/errors.md` is generated from it. Tests keep the Rust sources, the
  page and both SDKs in line with it.
- `schemas/agentbuild.schema.json`: JSON Schemas of the data every
  language exchanges, checked against values the engine writes.
- Tool-argument validation now enforces `anyOf`, `oneOf`, `allOf` and
  local `$ref` (e.g. Pydantic `Optional` fields and nested models).
- SDKs: `step_limit_exceeded` and `deadline_exceeded` raise
  `LimitExceededError`, `workflow_mismatch` raises `ValidationError`.

### agentbuild-gateway: LLM gateway, first slice (design 0006 §12)

- An OpenAI-compatible HTTP gateway: `POST /v1/chat/completions`
  (streaming and not), `GET /v1/models`, `GET /health`, in front of any
  upstream AgentBuild speaks (OpenAI, Anthropic, Gemini, OpenRouter,
  Ollama, vLLM). Routes with fallback models, virtual keys per route,
  usage records without content, the upstream call stopped when the
  client leaves. A library (`Gateway::builder().router()`) and a binary
  configured by TOML whose secrets come from environment variables.

### TypeScript SDK, first slice (design 0012)

- `sdks/typescript`: the `@agentbuild/sdk` package on the Rust engine
  (napi-rs addon, Node 18+): the same slice as the Python SDK, with
  `AbortSignal` on every call and error classes whose `cause` is what a
  crashing tool threw. Checked live against OpenAI and Gemini.

### Python SDK, first slice (design 0011)

- `sdks/python`: the `agentbuild` package on the Rust engine (PyO3 abi3
  wheel for Python 3.9+): `Agent`, `@tool`, approvals, typed output,
  threads with streaming, `Storage` (SQLite, Postgres, encryption, audit,
  retention), `Memory`, errors with the Rust codes, sync wrappers,
  `FakeModel`. Checked live against OpenAI and Gemini.
- agent: `Thread::with_options`, `ToolContext::metadata`.

### Audit trail (design 0010)

- `AuditLog` with in-memory, SQLite and Postgres backends (schema version
  5): records chained by SHA-256, `verify` reports the first changed or
  missing record, `head` for anchoring elsewhere. `Storage::with_audit`
  keeps it in the same database.
- Agents record model and tool calls, runs, approval requests and
  decisions (before the decided calls run), rewinds, deletions and memory
  writes, with the actor from `Thread::actor`. Records hold no content
  (memory keys as hashes). Fail closed: `audit_failed`.
- Core: `NodeContext::metadata` exposes `RunOptions::metadata` to nodes.

### agentbuild-agent: agents

- `Thread::stream` (and `stream_approve`, `stream_reject`,
  `stream_decide`, `stream_retry`): a thread operation's events as they
  happen, then the saved `Reply` (`ThreadStream`). Dropping the stream
  cancels the run and still saves it, so `retry` continues it.
- A cancelled run stops its model call at once (it used to run until the
  cancel grace ended).

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
- Long-term memory (`Memory`): facts kept per user or any namespace
  across conversations, recalled before each model call, with `remember`,
  `forget` and `search_memory` tools; a run without its user fails
  instead of sharing memory. `AgentInput::with` sets run state values.

### agentbuild-storage: where threads live

- Checkpoints as plain JSON, compare-and-set commits, per-thread leases
  (`OnBusy`: wait, reject, interrupt), durability `step` (default) or
  `exit`.
- In-memory backend; SQLite backend (feature `sqlite`) with delta +
  keyframe checkpoints, automatic migrations and crash recovery.
- Memory store: JSON items under namespaces, keyword search ranked the
  same on every backend, time to live, deleting a whole namespace; in
  memory or in the same SQLite or Postgres database.
- Postgres backend (feature `postgres`): the SQLite format and guarantees
  on a server (PostgreSQL 13+), TLS, and thread locks across processes
  (`ThreadLocks`): `OnBusy` wait, reject and interrupt hold between
  servers, and a crashed server's threads are freed.
- Retention: `Retention` (idle threads expire, threads keep their last N
  checkpoints, abandoned branches go), applied by `Storage::prune` or a
  background `Storage::spawn_pruner`; expired memory is removed too.
  `Checkpointer` gains `threads` (list, paged) and `prune_thread`.
- Encryption at rest (feature `encryption`): `Keyring`,
  `Storage::connect_encrypted`, `Storage::reencrypt` (rotation, or
  encrypting an existing database). Checkpoint bodies, snapshots, task
  writes, memory values and memory keys are encrypted with AES-256-GCM
  (a key per row, derived with HKDF); rows cannot be moved between
  places. An encrypted database refuses to open without its keys
  (`encryption_key_missing`) or with a wrong key
  (`encryption_key_invalid`).
- Semantic memory search: `Embedding`, `MemoryStore::put_embedded`,
  `SearchQuery::similar_to` / `min_score`, ranked by cosine similarity the
  same way on every backend (brute force; vectors are encrypted with the
  rest). Schema version 4.
- SQLite overwrites deleted rows in the file (`secure_delete`).
- Schema version 3 (`agentbuild_settings`, an index for idle threads);
  applied automatically.

### agentbuild-telemetry: tracing

- OpenTelemetry export over OTLP/HTTP (runs, steps, model generations, tool
  calls) with a Langfuse preset, content capture switch and masking, and a
  non-blocking observer.

### Testing

- Simulated models in all automated tests; a package test that uses the
  packaged crates from a separate project; a manual live check of every
  feature against real models (`tests/live`), passing on OpenAI, Gemini and
  Ollama.
