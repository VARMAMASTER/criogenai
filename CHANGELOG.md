# Changelog

All notable changes to Criogen. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Before 1.0, a minor version
(0.x → 0.y) may contain breaking changes; patch versions do not.

All crates in this repository share one version number.

## [Unreleased]

First release, planned as 0.1.0 (Rust crates only).

### Renamed: AgentBuild is now Criogen

- The project, its packages and every identifier were renamed from AgentBuild to Criogen:
  the packages are `criogen` (npm and PyPI; import `criogen`), the Rust crates `criogen-*`,
  the platform addons `criogen-<platform>-<arch>`, environment variables `CRIOGEN_*`, the error
  base class `CriogenError`, and the database tables `criogen_*`. Nothing had been released,
  so there is no migration; a database created before the rename must be recreated.
- `agentbuild-sdk@0.1.0-alpha.1` (npm, Windows only) was published once under the old name
  and is deprecated; `criogen` starts at `0.1.0` once the all-platform build has run.

### Keys from the environment and a `.env` file (Python, TypeScript)

- `load_env()` / `loadEnv()` reads a `.env` file into the environment (names returned, never values;
  an existing variable wins unless `override`; a missing file is fine unless `required`; 1 MB cap).
  `env.status()` / `envStatus()` says which providers have a key, `key_name()` / `keyName()` names the variable.
- A model with no key now fails at once, naming the variable to set, instead of an `auth` error later.
  A custom `base_url` (a gateway or local server) still needs no key.

### Patterns, specialists and web tools in Python, TypeScript and Rust (design 0019)

- `criogen.patterns`: sequential, parallel, loop, review and critique,
  iterative refinement, coordinator, hierarchical, swarm, human review, react,
  custom, and `quick("researcher > writer")`. Built from agents and workflows
  (no engine change); every loop has a cap (ceiling 50) and a `stop_reason`.
- `criogen.specialists`: default agent recipes (researcher, summarizer,
  classifier, extractor, planner, writer, critic, fact checker, code reviewer,
  translator, support agent) with `build`, `extend`, `register`, `get`.
- `criogen.web`: `search()` (Tavily or Brave, from the environment), `fetch()`
  (public pages only, no redirects, bounded); the researcher uses them when a key is set.
- TypeScript: `patterns`, `specialists`, `web` (same shapes, camelCase). Rust:
  `criogen_agent::{patterns, specialists}` and `web` behind the cargo feature `web`.
- Private-network addresses are refused by the page reader in every spelling
  (IPv4 inside IPv6, NAT64, 6to4), in all three languages.
- Fixed: the default Gemini embedding model (`text-embedding-004`) no longer
  exists; it is now `gemini-embedding-001` (Python and TypeScript).
- Website: `/playground/` and `/demo/` (feature tour with a scripted and a real
  model, and a chat); `demos/feature-tour/`.


### criogen-core: the workflow engine

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

### criogen-providers: models

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

### Security

- TLS certificate checks: `rustls-webpki` 0.103.16, `webpki-roots` and
  `webpki-root-certs` 1.0.10 (RUSTSEC-2026-0381, -0382, -0383: distrusted
  intermediates could be accepted), in every lockfile.

### Security audit, October 2026

An audit of the whole codebase (four independent reviewers; each finding
checked against the code) and what changed. **Behaviour changes:**

- **Encrypted databases refuse plain rows** (`storage_corrupt`): a database
  written before encryption was turned on must be converted with
  `reencrypt()` before it can be read with keys.
- **Telemetry exports no content by default in Rust too**
  (`OtlpConfig::capture_content` is now `false`), and with it off no error
  text, `user_id`, `session_id` or custom metadata, only error codes.
- **Credentials are not sent over plain `http://` to another machine**
  unless `allow_insecure_http` is set (SDK models, `Endpoint`, gateway
  config); loopback and `https://` are unaffected.
- **HTTP redirects are not followed** by the provider client.
- **The gateway reserves tokens at admission**, caps `max_tokens`, limits
  requests in flight (new code `concurrency_limited`), cuts off clients
  that stop reading, shows clients only the kind of upstream failures and
  no longer sends `x-criogen-model` unless `expose_model_header` is set.
- A thread keeps the value that scopes its memory (`Memory::by_state`): a
  later turn that changes it is `invalid_input`.
- A `$ref` a tool schema does not define is a validation problem (it used
  to let the value through unchecked); Pydantic `$defs` are hoisted to the
  schema root so nested arguments are checked.

**Fixed:** a panic on a non-ASCII character after a URL in a provider's
error text; keys inside JSON or other punctuation are now redacted from
errors; `extras` can no longer override `model`, `stream` or `messages`;
response size, SSE and stream buffers are bounded; an exception reaches
its own run's error, never a concurrent run's (hooks and workflow nodes);
exception text shown to the model has URLs and credentials removed;
memory recalled into the system prompt is one bounded line per fact;
Postgres thread locks use SHA-256 keys; a SQLite path is a path, not URL
options (and the file is owner-only on Unix); a recursive query on a
tampered checkpoint chain is bounded; a stored vector's size cannot
overflow.

**Added:** the audit log refuses `UPDATE`/`DELETE` (and `TRUNCATE` on
Postgres) in the database, and signed anchors (`AuditLog::anchor`,
`verify_anchor`) detect a cut tail or a recomputed chain;
`seal_snapshot` / `open_snapshot` sign a suspended run for apps that let
users hold one; `KeyLimits::max_concurrent`,
`GatewayBuilder::max_concurrent_requests`, `max_tokens`,
`expose_model_header`; `HttpConfig::max_response_bytes`;
`Endpoint::allow_insecure_http`; `HookError::cause_key`.

**Not changed, and documented** (SECURITY.md, design 0008 §15.2, 0010
§5.1): replay of an older valid thread head by someone with database write
access (needs an anchor outside the database), key and plaintext buffers
are not zeroised, encrypted memory search cost grows with the namespace,
SQLite's WAL can hold deleted content until it is checkpointed, and any
role on the same Postgres database can take advisory locks or send
interrupts (use a dedicated role).

### Gemini 3 tool use works (design 0006 §14)

- Fixed: with a Gemini 3 model, the second model call of a tool-using
  agent failed with HTTP 400 ("Function call is missing a
  thought_signature"). The OpenAI-format codec now keeps a tool call's
  `extra_content` (whole and streamed) and sends it back to the same
  endpoint; history from another model gets Google's placeholder
  signature on the `gemini` preset (`OpenAiQuirks::thought_signatures`).
  Checked live: every tool scenario of `tests/live` passes on
  `gemini-3.8-flash`, and the first one fails without the fix.

### A tool's exception no longer stops the run

- **Changed default:** an exception a tool did not handle (Python,
  JavaScript) or a panic in a Rust tool now reaches the model as a
  `tool_failed` result with the exception's type and message, and the run
  goes on (it used to fail the run). Choose per agent with
  `Limits::on_tool_exception` / `on_tool_exception=` / `onToolException`:
  `return_to_model` (default), `return_generic` (the model only learns
  that the tool failed; the text stays in the result's `artifact`) or
  `fail` (the old behaviour; the exception is the error's cause).
  `ToolError::crashed` marks such a failure; `ToolError::fatal` still
  fails the run. Designs 0003 §5.1, 0004, 0011, 0012.

### Memory by meaning and tracing in the SDKs (design 0017 §3–4)

- `Embedder` (OpenAI, Gemini, Ollama, any OpenAI-compatible server, and a
  fake one for tests) and `Memory.semantic(embedder)` in Python and
  TypeScript.
- `Telemetry.langfuse()` / `Telemetry.otlp(url)` with `flush`,
  `shutdown` and `stats`; `Agent(telemetry=…)` / `{ telemetry }`. Message
  content stays out of traces unless `capture_content` / `captureContent`
  is set (the SDK default; Rust's `OtlpConfig` default is unchanged).
- Rust: `AgentBuilder::observe` / `observe_shared`: observers (e.g. a
  telemetry exporter) on the agent, applied by `Agent::run_options` to
  every run and thread.

### Hooks in the SDKs (design 0017 §5)

- Python `Agent(hooks=[...])` and TypeScript `new Agent({ hooks })`: an
  object with any of `on_start`, `before_model`, `after_model`,
  `before_tool`, `after_tool`, `on_end` (camelCase in TypeScript), sync or
  async. Change or skip a tool call (`ToolResult`), raise `ToolError`,
  replace a tool result, the transcript or a reply, keep state, end a run.
- An exception in a start or model hook fails the run with `hook_failed`
  (the exception is its cause); in a tool hook it is the tool's crash.
- Rust: `HookContext::metadata` and `ToolNext::context` (hooks see the
  run's metadata).

### TypeScript tool input from Zod and Standard Schema (design 0017 §6)

- `tool({ input })` takes a Zod 4 schema or any Standard Schema v1
  (ArkType, Valibot with an explicit `jsonSchema`). The engine checks the
  JSON Schema the library makes; the library's issues (refinements) go
  back to the model as `invalid_arguments`; `run` gets the library's
  output, typed.
- Rust: `ToolError::invalid_arguments` lets any tool reject its arguments
  the way the schema check does.

### The gateway: Anthropic clients, rate limits, budgets (design 0006 §13)

- `POST /v1/messages`: the Anthropic Messages API (streaming with
  Anthropic's events, tools and tool results, images, `system`), in
  front of any upstream; keys may come as `x-api-key`; errors in
  Anthropic's shape.
- Per-key `requests_per_minute`, `tokens_per_minute` and `budget`
  (tokens per UTC day, month or in total), in memory: 429 with
  `Retry-After`, codes `key_rate_limited` and `budget_exceeded`;
  `GatewayBuilder::key_limits`, `KeyLimits`, `Gateway::spent`.

### Calling a model directly (design 0018)

- Python and TypeScript `Gateway`: `chat` (text or messages, instructions,
  temperature, max tokens, JSON output checked against a schema),
  `stream` (text chunks; a `restart` chunk when a retry starts over) and
  `embed`, with an agent's retries and fallback models; `reply.model`
  names the model that answered. Cancellation by task cancel or
  `AbortSignal`. `FakeModel(name=…)` tells fake models apart.
- Rust: `ResilientModel`, the agent's retry and fallback chain as a
  `Model` of its own.
### Sub-agents, retries and fallbacks in the SDKs (design 0017 §1–2)

- `agent.as_tool(name, description)` / `agent.asTool(...)`: an agent as a
  tool of another agent.
- `fallbacks`, `fallback_on` / `fallbackOn`, `model_retry` / `modelRetry`,
  `tool_retry` / `toolRetry` (`Retry`), `max_retry_after` /
  `maxRetryAfterSecs` on `Agent`.
- TypeScript `FakeModel` gains `failFirst` and `byTurn` (as in Python).

### Workflows in the TypeScript SDK (design 0015)

- `new Workflow()` with `.node` / `.addNode` (functions or agents),
  `.start`, `.edge`, `.route`, `.join`, `.goto`, reducers, `suspend` /
  `goto`, `compile({ storage })`, `run` / `resume` with `maxSteps`,
  `deadlineMs` and an `AbortSignal`, and durable `thread(id)`s. A failing
  node's thrown value is the error's `cause`.

### Workflows in the Python SDK (design 0015)

- `Workflow` with `@wf.node` / `add_node` (functions or agents), `start`,
  `edge`, `route`, `join`, `goto`, reducers, `suspend` / `resume`,
  `compile(storage=…)` (every graph problem at once), `run` / `run_sync`
  and durable `thread(id)`s (`run`, `resume`, `retry`, `interrupts`,
  `history`, `rewind`, `delete`). The engine runs it in Rust.
- `FakeModel(fail_first=n)` (the first calls fail with a transient rate
  limit) and `FakeModel(by_turn=True)` (replies chosen by the
  conversation's turn, so one model serves many runs at once); in Rust,
  `FakeModel::by_turn` and `FakeModel::without_request_log`.

### Comparison with LangGraph and CrewAI (design 0016)

- `benches/compare/`: the same agents, workflows, loads and injected
  faults run on Criogen, LangGraph and CrewAI in Python, one venv per
  framework and one process per measurement, with one simulated model;
  results as a JSON file (`criogen-compare/1`) that the website's
  benchmark lab shows as comparison graphs.

### Release preparation

- The Python and npm packages include the licence files and project
  links; the Python package is typed (`py.typed`, stubs; checked with
  mypy in the package test); SDK binaries are stripped.
- `Storage::close()` closes the database's connections (SQLite releases
  the file); `Storage.close()` in the Python and TypeScript SDKs.
- A manual workflow builds the Python wheels and the npm native addons
  for Linux (x64, arm64), macOS (arm64, x64) and Windows (x64), tests each
  installed on its own platform, and can publish them. The TypeScript SDK
  loads a prebuilt `criogen-<platform>-<arch>` package when no
  locally built addon is present.

### Benchmarks (design 0014)

- `criogen-bench` and `benches/`: the framework's own costs (engine
  steps, parallel nodes, agent loop, tool calls, thread saving, gateway,
  SDK bridges) with instant simulated models, and the same workflow
  shapes in LangGraph. Results in `docs/benchmarks.md`.
- `FakeModel` with zero delay no longer waits for a timer tick (about
  1 ms on Windows).

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
- `schemas/criogen.schema.json`: JSON Schemas of the data every
  language exchanges, checked against values the engine writes.
- Tool-argument validation now enforces `anyOf`, `oneOf`, `allOf` and
  local `$ref` (e.g. Pydantic `Optional` fields and nested models).
- SDKs: `step_limit_exceeded` and `deadline_exceeded` raise
  `LimitExceededError`, `workflow_mismatch` raises `ValidationError`.

### criogen-gateway: LLM gateway, first slice (design 0006 §12)

- An OpenAI-compatible HTTP gateway: `POST /v1/chat/completions`
  (streaming and not), `GET /v1/models`, `GET /health`, in front of any
  upstream Criogen speaks (OpenAI, Anthropic, Gemini, OpenRouter,
  Ollama, vLLM). Routes with fallback models, virtual keys per route,
  usage records without content, the upstream call stopped when the
  client leaves. A library (`Gateway::builder().router()`) and a binary
  configured by TOML whose secrets come from environment variables.

### TypeScript SDK, first slice (design 0012)

- `sdks/typescript`: the `criogen` package on the Rust engine
  (napi-rs addon, Node 18+): the same slice as the Python SDK, with
  `AbortSignal` on every call and error classes whose `cause` is what a
  crashing tool threw. Checked live against OpenAI and Gemini.

### Python SDK, first slice (design 0011)

- `sdks/python`: the `criogen` package on the Rust engine (PyO3 abi3
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

### criogen-agent: agents

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

### criogen-storage: where threads live

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
- Schema version 3 (`criogen_settings`, an index for idle threads);
  applied automatically.

### criogen-telemetry: tracing

- OpenTelemetry export over OTLP/HTTP (runs, steps, model generations, tool
  calls) with a Langfuse preset, content capture switch and masking, and a
  non-blocking observer.

### Testing

- Simulated models in all automated tests; a package test that uses the
  packaged crates from a separate project; a manual live check of every
  feature against real models (`tests/live`), passing on OpenAI, Gemini and
  Ollama.
