# Criogen: notes for assistants and contributors

Read this first when picking up work on this repository. Then read
[docs/progress.md](docs/progress.md) for current status and next steps, and
[docs/architecture.md](docs/architecture.md) for how the system works.

## What this project is

An open-source AI agent orchestration framework. A **Rust runtime** executes
workflow graphs; **Python and TypeScript SDKs** (planned) let developers define
agents, tools and workflows in their own language. User code always runs in its
own interpreter; Rust schedules and coordinates.

The goal is not to clone LangChain or LangGraph. We study their design (MIT
licensed, installed in `research/.venv`) and build our own, smaller and faster
design. **Never copy their source code**; describe ideas in our own words and
write our own implementation.

## Current state (keep this section accurate)

- **Implemented:** `crates/criogen-core`. Superstep engine with static
  edges, async routers, joins, cycles, concurrent nodes, atomic steps, stable
  error codes. CI was last green on GitHub on Linux, Windows, macOS and
  MSRV 1.88 at `3ca76cc` (2026-10-13).
- **Design approved** (user gave full decision freedom, 2026-10-11):
  `docs/design/0002`–`0008` cover messages and tools, errors, the agent loop,
  core extensions, the provider layer and gateway, the public API, and
  persistence and memory. Research is in `docs/research/`.
- **Implemented in `criogen-core`:** the 0001 engine plus all of 0005:
  NodeContext, `Update`/`Command`/`Suspend` outcomes, suspend/resume with
  JSON snapshots, reducers, the workflow fingerprint, live events
  (`Workflow::stream`, `event_channel`, `ctx.events()`), policies
  (`NodePolicy` timeout/retry, run deadline, `CancelToken` with grace), and
  saved task writes with `ctx.memo()` plus snapshots on failure
  (`RunError::snapshot`, `Workflow::retry`).
- **Implemented in `criogen-providers` (foundation):** the 0002 types
  (`Message`, `ContentBlock`, `ToolSpec`, `ModelRequest`/`ModelResponse`,
  `Capabilities`), `ModelError` with `classify_http` (0003 §4), transcript
  check/repair (0002 §6), the `Model` trait, `FakeModel`, the SSE parser,
  the stream assembler, the HTTP `Transport` (feature `http`, on by
  default), the `openai_chat` and `anthropic_messages` protocols,
  `Endpoint` presets and `ProviderModel` (OpenAI, OpenRouter, Ollama,
  LM Studio, vLLM, Anthropic, any OpenAI- or Anthropic-compatible server).
- **Implemented in `criogen-agent` (slice 1):** the agent loop (0004):
  `model` ⇄ `tools` workflow, Rust tools (`tool_fn`), argument validation,
  concurrent calls in call order, timeouts, limits, model retries, events,
  memo-backed `Agent::retry`; slice 2: human approval (`Approval`,
  `review` node, `Agent::resume` with `Decision`s); slice 3: structured
  output (`AgentBuilder::output`, native or `final_output` tool); slice 4:
  hooks (`Hook`: on_start, before_model, wrap_model, after_model,
  wrap_tool, on_end); slice 5: `OnLimit::End`, token budget, spent tool
  budget ends at the next model call; slice 6: model resilience
  (`Retry-After`, immediate retry of unsent requests, fallback models);
  slice 7: `agent.as_node()`, `agent.as_tool()`, `Limits::nesting`;
  slice 8: tool features (per-tool concurrency, safe retries, result
  truncation, state updates, app context); slice 9: `agent.stream()`.
  The 0004 agent list is complete. Threads (0008 §11): `agent.thread(id)`
  with `send`, `approve`/`reject`/`decide`, `retry`, `cancel`, `history`,
  `rewind`, `delete`; `stream` / `stream_approve` / `stream_reject` /
  `stream_decide` / `stream_retry` (`ThreadStream`, 0008 §17: a dropped
  stream cancels and still saves; model calls stop at once on cancel).
- **Implemented in `criogen-storage` (0008 steps 1–5):** `Checkpoint`,
  the `Checkpointer` trait (compare-and-set commit, `put_pending`),
  `InMemoryCheckpointer`, `SqliteCheckpointer` (feature `sqlite`, sqlx
  0.8: delta + keyframe checkpoints, migrations), `ThreadWriter`,
  `Storage` with per-thread leases, `OnBusy` (wait, reject, interrupt) and
  `Durability` (`Step` default: saved per step and per tool call via core's
  `Persister` / `RunOptions::persist`; `Exit`). Crash recovery:
  `thread.retry()` on an `in_progress` head. Long-term memory (0008
  §13): `MemoryStore` (`InMemoryStore`, `SqliteMemoryStore`; keyword
  search ranked the same on every backend, TTL, `delete_namespace`),
  `Namespace`, `ScopedMemory`, `Storage::memory()`. Agents:
  `AgentBuilder::memory(Memory::by_state("user_id", users))` recalls
  facts before each model call and adds `remember` / `forget` /
  `search_memory`; `AgentInput::with(key, value)` sets run state;
  `ToolContext::memory()`. Postgres (feature `postgres`, §14):
  `PostgresCheckpointer`, `PostgresMemoryStore`, and
  `PostgresThreadLocks`. `ThreadLocks` makes `OnBusy` hold across
  processes, using session advisory locks plus `LISTEN` for interrupts.
  The tests need a server: `scripts/with-postgres.sh`, which starts a
  throwaway one from the portable binaries that `scripts/get-postgres.sh`
  downloads. Step 5 (§15): `Retention` (`thread_ttl`,
  `keep_checkpoints`, `drop_branches`) applied by `Storage::prune` or
  `spawn_pruner`; `Checkpointer::threads` / `prune_thread`. Encryption
  at rest (feature `encryption`): `Keyring`, `Storage::connect_encrypted`,
  `reencrypt`; AES-256-GCM per-row keys (HKDF), memory keys as HMAC tags,
  `criogen_settings` refuses opening an encrypted database without
  its keys. SQLite runs with `secure_delete`. Step 6 (§16): semantic
  memory search. Workflow threads (§18): `Storage::workflow_thread(&wf,
  id)` → `WorkflowThread` (`run`, `resume(values)`, `retry`, `cancel`,
  `interrupts`, `history`, `rewind`, `delete`, `actor`), `ThreadError`,
  `Turn`; same leases, writer, guards and audit records as agent
  threads. `Embedder` (`ProviderEmbedder` for OpenAI-compatible
  `/embeddings`, `FakeEmbedder` for tests) in providers; `Embedding`,
  `MemoryStore::put_embedded`, `SearchQuery::similar_to` (brute-force
  cosine in Rust, every backend; vectors encrypted with the rest) in
  storage; `Memory::semantic(embedder)`, `put_embedded`, `memory_text`
  in agents (keyword fallback when the embedder fails).
- **Implemented: audit trail (0010).** `AuditLog` (`InMemoryAuditLog`,
  `SqliteAuditLog`, `PostgresAuditLog`; table `criogen_audit`,
  migration 5), SHA-256 hash chain with `verify` and `head`,
  `Storage::with_audit` / `record`, fail closed (`audit_failed`). Agents
  record model and tool calls (before the result is saved), runs,
  approvals (decisions before the calls run), rewinds, deletions and
  memory writes (key hashed), with `Thread::actor`. No content in
  records. Core: `NodeContext::metadata` (agents put the audit context
  under `criogen_audit`).
- **Implemented: Python SDK, first slice (0011).** `sdks/python`
  (crate `criogen-python`, not a workspace member; PyO3 0.29 abi3,
  maturin): `Agent`, `@tool` (schemas from type hints; sync tools on a
  worker thread, async tools on the caller's loop), approvals, typed
  output, threads (send/approve/reject/decide/retry/history/rewind/
  delete/stream), `Storage` (SQLite, Postgres, encryption, audit,
  retention), `Memory`, errors by code, `run_sync`/`send_sync`,
  `FakeModel`. Bridge: one Tokio runtime, asyncio futures completed with
  `call_soon_threadsafe`, Python cancellation cancels the run. Tests:
  `sdks/python/tests` (pytest); `scripts/test-python.sh` installs the
  wheel into a new venv; `ci-local.sh` step `python`. Agent API added
  for it: `Thread::with_options`, `ToolContext::metadata`. Workflows
  (0015): `Workflow`, nodes (functions or agents), routes, joins, goto,
  reducers, suspend/resume, threads (`sdks/python/tests/test_workflow.py`).
  Package name **`criogen`** on PyPI (import `criogen`; published from 0.1.2).
  Since then (0017):
  sub-agents, retries and fallbacks, semantic memory, telemetry and
  hooks in both SDKs; Zod / Standard Schema tool input in TypeScript;
  `Gateway` (0018: chat, stream, embed without an agent, over Rust's
  `ResilientModel`).
- **Implemented: TypeScript SDK, first slice (0012).** `sdks/typescript`
  (crate `criogen-node`, not a workspace member; napi-rs 3, N-API 6,
  Node 18+; package `criogen`, TypeScript in `lib/` compiled to
  `dist/`): the same slice as Python, and workflows (`Workflow`, 0015;
  `test/workflow.test.js`). `Agent`, `tool({...})` with JSON
  Schema input, approvals, typed output, threads (incl. `stream*`),
  `AbortSignal` on every call, `Storage`, `Memory`, error classes by
  code (`cause` = what a crashing tool threw). Tools are weak
  thread-safe functions returning Promises. Tests: `node --test`
  (`sdks/typescript/test`); `scripts/test-typescript.sh` installs the
  npm tarball into a new project; `ci-local.sh` step `typescript`.
- **Implemented: LLM gateway, first slice (0006 §12).**
  `crates/criogen-gateway` (axum 0.8, toml): `Gateway::builder()`
  (routes with fallbacks, virtual keys kept as SHA-256 digests, usage
  records on a `try_send` channel) → axum `Router`; `Config` (TOML;
  secrets only through `*_env` variable names); binary
  `criogen-gateway --config … [--listen] [--allow-open] [--log-usage]`
  (refuses an open gateway off loopback). `POST /v1/chat/completions`
  (stream and not; OpenAI wire format in and out, any upstream protocol),
  `GET /v1/models`, `GET /health`; the upstream call is aborted when the
  client goes away. Checked live with the official `openai` Python SDK
  against OpenAI and Gemini upstreams. Second slice (0006 §13):
  `POST /v1/messages` (Anthropic wire format, streaming), per-key rate
  limits and token budgets in memory (`KeyLimits`, `Gateway::spent`);
  checked by hand with the official `anthropic` Python SDK.
  Not yet: relay fast path, shared counters, budgets in money.
- **Implemented: error catalog and JSON Schemas (0013).**
  `schemas/error-codes.json` (every stable code: layers, category,
  retryable, SDK family, gateway HTTP status) and
  `schemas/criogen.schema.json` (Message, ContentBlock, ToolSpec,
  ModelRequest/Response, MessageDelta, ModelError, ApprovalRequest,
  Decision, AuditRecord, UsageRecord, ErrorObject). Tests:
  `criogen-agent/tests/catalog.rs` (scans the sources for raised
  codes; `docs/errors.md` is generated: `CRIOGEN_UPDATE_DOCS=1 cargo
  test -p criogen-agent --test catalog`), `tests/schemas.rs` (real
  values validate), SDK tests check their error classes against the
  catalog. The validator now enforces `anyOf`/`oneOf`/`allOf`/local
  `$ref` (tool arguments too).
- **Benchmarks (0014):** `crates/criogen-bench` (`cargo run -p
  criogen-bench --release`), `benches/` (LangGraph comparison, Python
  and TypeScript bridges); results in `docs/benchmarks.md` (one machine,
  2026-10-16). Quote numbers only from that page, with what was measured.
- **Framework comparison (0016):** `benches/compare/` (setup.py: one venv
  per framework in `target/compare/venvs`; `run.py`: scenarios, load,
  reliability, footprint → `bench/results/<date>T<time>-<machine>.json`;
  `package.py`: the public kit zip for the website). Criogen,
  LangGraph and CrewAI in Python with one simulated model. The website's
  benchmark lab (`website/app/benchmarks`) shows the file in
  `website/public/bench/results.json`.
- **Tool exceptions:** `Limits::on_tool_exception` (`OnToolException`:
  `ReturnToModel` default, `ReturnGeneric`, `Fail`); `ToolError::crashed`;
  Rust tool panics are caught and follow it (0003 §5.1).
- **Implemented in `criogen-telemetry` (0009):** OTLP/HTTP JSON export
  of runs (trace = run, spans, generations, tool observations), Langfuse
  preset, content capture switch and masks, non-blocking observer
  (`RunOptions::observe`), run metadata (`RunOptions::metadata`).
- Tests: 545 across the seven crates (with `--all-features`; includes the
  getting-started guide's examples), plus the package test. Release prep:
  `CHANGELOG.md`, `CONTRIBUTING.md`, `SECURITY.md`, `RELEASING.md`,
  `deny.toml` (CI job `dependencies`), `.github/workflows/release.yml`
  (manual; dry run unless "publish"). Crates still have `publish = false`
  until the user confirms the names.
- **Demo app:** `demos/support-chat` (Rust server on Criogen + Next.js
  UI; real OpenAI/Gemini models, manual only, not in CI or the workspace).
  Keys live in `~/.criogen-keys.env`, never in the repo.
- **Next:** the 10-task roadmap in `docs/progress.md` ("Roadmap
  tasks") is done, and so are the release blockers that need no
  decision: `Storage::close` (and `close()` in both SDKs),
  `.github/workflows/sdk-packages.yml` (wheels and npm addons for Linux
  x64/arm64, macOS arm64/x64, Windows x64, tested per platform; manual,
  never run yet), the TS loader for `criogen-<platform>-<arch>`.
  Decided (2026-10-22): the code stays private until 10,000 installs
  (then MIT or Apache-2.0); until then the Python and npm packages are
  free to use under the Criogen Free Licence (`sdks/*/LICENSE`, a
  draft to review); Python and TypeScript first, Rust crates when the
  code opens; no GitHub links on the website; a live install counter on
  the home page. Waiting for the user: running the GitHub workflows (paid
  minutes while private), registry accounts (see RELEASING.md), a legal
  review of the licence. The user will build their
  hospital product (Axon Health OS) on Criogen: keep the framework
  general, but approval gates, audit trails, no patient data in logs, and
  self-hosted models matter.
- **Website:** `website/` (Next.js 16, React, static export for Vercel):
  landing page, docs (MDX in `content/docs`, code samples compiled from
  `snippets/`), benchmark lab, install counter. `npm run build` /
  `npm run preview` (port 4400); checked by `ci-local.sh` step `website`.
  Pattern playground (`/playground/`): 19 agent patterns
  (`website/lib/patterns/`: reasoning, multi-agent, IBM's five agent types),
  one page each with a diagram, a run and the Criogen code
  (`snippets/*/patterns.*`, one region per pattern). The run is the
  TypeScript sample itself on the engine in the browser (design 0021):
  `scripts/build-engine.mjs` bundles the samples into
  `public/engine/playground.js` with `website/engine/runtime.js` as
  "criogen" (the real engine plus a report of tools, nodes and storage);
  `lib/patterns/live.ts` sets the samples' `MODEL` (snippets/typescript/
  model.ts) to a `FunctionModel`: scripted per agent (`casts.ts`) without a
  key, the visitor's provider through the engine with one (React state
  only, at most 16 calls a run, tools return made-up data).
  `scripts/test-playground.mjs` runs all 19 and a key (mocked providers).
  The Criogen-vs-LangChain comparison moved to `/benchmarks/#live`.
- **Implemented: the engine in the browser (0021).** `crates/criogen-rt`
  (Tokio on servers, the JavaScript event loop on `wasm32-unknown-unknown`:
  spawn, JoinSet, timers, Instant); the providers' transport moves bytes
  with reqwest or `fetch`; `crates/criogen-js` is the TypeScript bridge
  under both the Node addon and `sdks/typescript/web` (wasm-bindgen).
  TypeScript `criogen/web` (`lib/browser`, `await ready()`), in-memory
  storage, `unsupported` for what needs a server. `FunctionModel` (models
  written in JavaScript, both engines) and gateway `tools`. Build:
  `node sdks/typescript/scripts/build-web.mjs`; tests:
  `bash scripts/test-web.sh` (criogen-rt on WebAssembly, the TypeScript
  suite with `CRIOGEN_ENGINE=wasm`, Chromium/Firefox/WebKit, the package in
  a new app), ci-local step `web`. Not on npm yet (waits for a release).
- **Security audit (2026-10-22):** four reviewers, findings verified and
  fixed (CHANGELOG "Security audit"); `scripts/check-supply-chain.sh`
  guards the build and release. New rules below (10–12).
- **Implemented (Python, TypeScript, Rust): patterns, specialists, web tools (0019).**
  `criogen.patterns` (12 designs from Google Cloud's agentic-pattern guide
  as functions, plus `quick("a > b")`), `criogen.specialists` (extendable
  default agent recipes and a registry), `criogen.web` (Tavily/Brave search,
  safe page fetch). Tests: `sdks/python/tests/test_patterns.py`, `test_web.py`;
  `sdks/typescript/test/patterns.test.js`, `web.test.js`;
  `crates/criogen-agent/tests/patterns.rs`, `web.rs` (feature `web`). Website: `/playground/`,
  `/demo/` (`demos/feature-tour/`: `tour.py` records scripted and `--live`
  tours into `website/public/demo/`; `chat_server.py` is the chat backend).
- **Implemented: named providers (0006 §4.1).** `criogen_providers::PRESETS` / `preset(name)`:
  one table (name, protocol, base URL, key variable, quirks) for openai, anthropic, gemini,
  openrouter, deepseek, mistral, xai, groq, cerebras, together, fireworks, qwen, moonshot, zai,
  ollama, lmstudio. Both SDK bridges and the gateway build named providers only from it; the SDKs'
  key tables are tested against it (`_native.PRESETS`, addon `presets()`). `AnthropicCompatible` /
  `anthropicCompatible` in the SDKs. Tests: `crates/criogen-providers/tests/presets.rs`,
  `sdks/python/tests/test_providers.py`, `sdks/typescript/test/providers.test.js` (local servers
  only). Research and what is still missing (Responses API, Azure, Bedrock, Vertex):
  `docs/research/providers-2026.md`.
- **Implemented: Kaggle's Model Proxy (0020).** `criogen_providers::KaggleModel` mints the
  hourly proxy token from a Kaggle API token and renews it; `kaggle:` model strings in both SDKs
  and `provider = "kaggle"` in the gateway. Tests: `crates/criogen-providers/tests/kaggle.rs`,
  `sdks/python/tests/test_kaggle.py`, `sdks/typescript/test/kaggle.test.js` (local servers only).
- **Released (2026-11-01): npm `criogen@0.1.0`** (`latest`) with platform packages
  `criogen-linux-x64`, `-linux-arm64`, `-darwin-arm64`, `-darwin-x64` and `criogen-windows-x64`
  (npm's spam filter refused `criogen-win32-x64`; the loader maps `win32-x64` to it). Built and
  tested per platform by `sdk-packages.yml` (dry run), published from this machine. 0.1.1: serverless
  Linux, lazy loading, `ready()`, run facts, `redact`. 0.1.2 (2026-11-03): metadata only on npm (engine
  packages stay 0.1.1); **PyPI `criogen==0.1.2`** with wheels for the same five platforms, built by
  `sdk-packages.yml` and uploaded with the pinned `maturin upload` (`PYPI_TOKEN` in `~/.criogen-keys.env`).
  0.1.3 (2026-11-04): named providers (deepseek, mistral, xai, groq, cerebras, together,
  fireworks, qwen, moonshot, zai, lmstudio) and `AnthropicCompatible`; all six npm packages and
  PyPI wheels for Linux x64/ARM64, macOS ARM64 and Windows x64. The macOS Intel wheel is missing
  (GitHub never gave the `macos-15-intel` job a runner), so pip on Intel Macs installs 0.1.2.
  Check a release with `scripts/smoke-registry.sh <version>`. Site: https://criogen.vercel.app.
- **Not started:** vector indexes
  (`sqlite-vec`, `pgvector`). Do not describe these as existing anywhere.

## Commands

```sh
cargo build
cargo test                       # unit + integration + doctests
cargo test --workspace --all-features   # also the SQLite backend
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
cargo test -p criogen-providers   # one crate
cargo run -p criogen-core --example basic_workflow -- "some text"
cargo run -p criogen-core --example approval_workflow -- yes
cargo run -p criogen-core --example llm_workflow -- "remote work"
cargo run -p criogen-core --example retry_workflow
cargo run -p criogen-core --example recover_workflow
cargo run -p criogen-agent --example approval_agent -- edit   # or approve, reject
cargo run -p criogen-agent --example thread_agent
cargo run -p criogen-agent --features sqlite --example durable_agent
# Langfuse, manual only (needs LANGFUSE_PUBLIC_KEY / LANGFUSE_SECRET_KEY):
cargo run -p criogen-telemetry --example langfuse
# Real model, manual only (needs a server; default: Ollama on localhost):
CRIOGEN_MODEL=llama3.2 cargo run -p criogen-providers --example chat -- "hi"
# Claude (needs ANTHROPIC_API_KEY):
CRIOGEN_PROTOCOL=anthropic CRIOGEN_MODEL=claude-haiku-5-5 cargo run -p criogen-providers --example chat -- "hi"
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
bash scripts/test-package.sh     # package the crate, use it from a separate project
cargo deny --all-features check  # advisories, licenses, sources (deny.toml)
bash scripts/ci-local.sh         # the whole CI pipeline, locally (--quick: rust step only)
bash scripts/test-python.sh      # Python SDK: wheel into a new venv, consumer + pytest
bash scripts/test-typescript.sh  # TypeScript SDK: npm tarball into a new project, consumer + node --test
node sdks/typescript/scripts/build-web.mjs   # the engine as WebAssembly: criogen/web (needs the wasm32 target, wasm-bindgen-cli)
bash scripts/test-web.sh         # the engine in the browser: WebAssembly tests, the suite on it, 3 browsers, the package
cargo run -p criogen-bench --release   # benchmarks (docs/benchmarks.md)
cargo run -p criogen-gateway -- --config crates/criogen-gateway/examples/gateway.toml   # the LLM gateway
bash scripts/get-postgres.sh     # portable PostgreSQL into ~/.criogen/postgres (once)
bash scripts/with-postgres.sh cargo test --workspace --all-features   # with a throwaway server
# Every feature against real models, manual only (keys in ~/.criogen-keys.env):
cd tests/live && cargo run       # or: cargo run -- gemini; LIVE_ONLY=approval cargo run
```

On the original Windows dev machine, Rust lives in `~/.cargo/bin`. A shell that
predates the install may not have it on `PATH`; call `~/.cargo/bin/cargo`
directly.

Research environment (Python 3.13):
`research/.venv/Scripts/python` (Windows) has langchain 1.4.4, langchain-core
1.6.9 and langgraph 1.2.14. The source is at
`research/.venv/Lib/site-packages/{langchain,langchain_core,langgraph}`.

REA (reverse-engineering CLI, used for `docs/research/build-analysis.md`):
installed locally with a portable Node 24 because the system Node is too old.

```sh
export PATH="$PWD/research/tools/node-v24.21.0-win-x64:$PATH"
research/tools/rea/node_modules/.bin/rea --help
```

`research/tools/`, `research/wheels/` and `research/rea-evidence/` are
git-ignored. REA is not registered as an MCP server in any agent config.

## Rules

1. **Contracts before features.** Behaviour changes to the engine need an update
   to `docs/design/` first, or in the same change. Number new docs
   sequentially (`0002-…`).
2. **Keep the core small.** Add a dependency only with a stated reason. Current
   deps: tokio (`sync`), criogen-rt (tasks and timers: Tokio `rt` + `time`
   on servers, the JavaScript event loop in the browser, design 0021),
   serde, serde_json, thiserror, uuid (v7 run ids). Dev only: tokio
   `test-util` for the paused clock. `criogen-providers` follows the budget
   in research `litellm-providers.md` §10.11 (today: serde, serde_json,
   thiserror, uuid, criogen-rt, reqwest behind the `http` feature on
   servers, wasm-bindgen/js-sys/web-sys for `fetch` in the browser).
   Engine crates call `criogen_rt`, never `tokio::spawn` or `tokio::time`
   (those panic in the browser).
   Transport tests use a scripted local TCP server
   (`tests/transport.rs`), never the network.
3. **Engine semantics live in Rust only.** SDKs must not reimplement
   scheduling, merging or validation.
4. **Strict now, relax later.** Validation rules start strict; relaxing them is
   backwards compatible, tightening them is not.
5. **Explicit failures.** Every failure has a `RunErrorKind` with a stable
   snake_case `code()`. A failed step commits nothing.
6. **Honest docs.** README status tables distinguish implemented from planned.
   No performance claims without benchmarks in the repo.
7. **Tests with every behaviour change.** Integration tests live in
   `crates/criogen-core/tests/`. Concurrency claims need tests that would
   deadlock or time out if the claim were false (see
   `fan_out_runs_nodes_concurrently`).
   **Simulate LLM calls; never call a real model in tests.** Use `FakeLlm`
   (`crates/criogen-core/tests/support/fake_llm.rs`): it replies after
   2–3 s (fixed per prompt) and can fail on a marker. Tests run it on Tokio's
   paused clock (`#[tokio::test(start_paused = true)]`) so timing is exact and
   the suite stays fast; examples run it in real time. See
   `tests/llm_simulation.rs`. Code that speaks the model contract
   (`criogen-providers`, later the agent loop) uses
   `criogen_providers::FakeModel` instead: the same 2–3 s timing, plus
   scripted tool calls (`FakeReply`) and classified `ModelError`s.
8. **Public API hygiene.** Public enums and structs that may grow are
   `#[non_exhaustive]`. Public items have docs and implement `Debug`
   (`missing_docs` and `missing_debug_implementations` are warnings; CI
   treats warnings as errors).
9. **Test the packages, not just the code.** `scripts/test-package.sh` runs
   `cargo package` (fails if the package misses a file), checks the README and
   license files are inside, then builds `tests/package/rust-consumer`, a
   separate project that depends on the unpacked package and uses only the
   public API. Extend the consumer when you add public API. CI runs it on
   Linux and Windows. When the Python and TypeScript SDKs exist, the same rule
   applies: install the built wheel / npm tarball into a clean environment and
   run a consumer script against it.

10. **Secrets and content never leave by default.** A key is never in
    `Debug`, an error, a log, a URL or a redirect; telemetry and the gateway
    export no content, error text or identity unless asked; credentials are
    not sent over plain `http` to another machine. A test fails if one does.
11. **Everything that comes from outside is bounded.** Response bodies,
    streams, request bodies, `max_tokens`, requests in flight, recursion
    depth: each has a limit with a test. Input from a provider, a model, a
    tool or a database row is data, never trusted to be well-formed (no
    slicing at byte offsets, no unchecked arithmetic).
12. **The database is not trusted either.** Content rows in an encrypted
    database are authenticated or refused; the audit trail has anchors; an
    exception, a hook error or a node error carries its own key, never one
    derived from a name or message that two runs can share.

## Git and GitHub

- **CI runs locally (decided 2026-10-14, for the next few months).** GitHub
  Actions does not run on push; `ci.yml` is manual only. Before every push,
  run `bash scripts/ci-local.sh` and push only when it passes. It needs the
  1.88 toolchain and `cargo-deny` (both installed on the dev machine; it
  skips and says so if they are missing). Local runs cover Windows only;
  start the CI workflow by hand before a release for Linux and macOS.

- Remote: `https://github.com/VARMAMASTER/criogen` (private). The `main` branch is protected by
  convention: commit only when the user asks.
- Use Conventional Commits (`feat(core): …`, `docs: …`, `fix: …`); mark breaking
  changes with `!`.
- `.gitattributes` forces LF line endings. The dev machine has
  `core.autocrlf=true`; the warnings it prints are harmless.
- Never commit `target/`, `research/` contents (venv, tools, repos, wheels,
  REA evidence), credentials, or build artifacts.
- License: MIT OR Apache-2.0. Never port code from AGPL sources (llmgateway).
  LiteLLM (MIT) data or code needs a `THIRD_PARTY_NOTICES` entry when used.

## Known pitfalls

- `pip` on the dev machine has an extra index (`pypi.ngc.nvidia.com`) that does
  not resolve. Installs still succeed from PyPI after retry warnings.
- System Node.js on the dev machine is 22.14. REA needs ≥22.19, so it runs on
  the portable Node 24 in `research/tools/` (see Commands). Don't upgrade the
  system Node without asking.
- Edition 2024 with let-chains means MSRV is 1.88. Don't lower it without
  removing let-chains.
- `RunError` is intentionally large (it carries the state).
  `clippy::result_large_err` is allowed on those impls with a comment; don't
  box it without a design reason.

## Where to look

| Question | File |
|---|---|
| What does a term mean? Naming rules? | `docs/glossary.md` |
| What does an error code mean? | `docs/errors.md` (from `schemas/error-codes.json`) |
| How does a run execute? | `docs/architecture.md` §4, `crates/criogen-core/src/runtime.rs` |
| What are the exact semantics? | `docs/design/0001-execution-model.md` |
| What's done, what's next, what's undecided? | `docs/progress.md` |
| How does LangGraph/LangChain do X? | `docs/research/*.md` |
| How do agents, tools and errors work (design)? | `docs/design/0002`–`0005`, `docs/architecture.md` §7 |
| Validation rules | `crates/criogen-core/src/graph.rs` (`build`), `src/error.rs` (`ValidationIssue`) |
