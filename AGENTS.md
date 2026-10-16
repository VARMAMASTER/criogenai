# AgentBuild: notes for assistants and contributors

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

- **Implemented:** `crates/agentbuild-core`. Superstep engine with static
  edges, async routers, joins, cycles, concurrent nodes, atomic steps, stable
  error codes. CI was last green on GitHub on Linux, Windows, macOS and
  MSRV 1.88 at `3ca76cc` (2026-10-13).
- **Design approved** (user gave full decision freedom, 2026-10-11):
  `docs/design/0002`–`0008` cover messages and tools, errors, the agent loop,
  core extensions, the provider layer and gateway, the public API, and
  persistence and memory. Research is in `docs/research/`.
- **Implemented in `agentbuild-core`:** the 0001 engine plus all of 0005:
  NodeContext, `Update`/`Command`/`Suspend` outcomes, suspend/resume with
  JSON snapshots, reducers, the workflow fingerprint, live events
  (`Workflow::stream`, `event_channel`, `ctx.events()`), policies
  (`NodePolicy` timeout/retry, run deadline, `CancelToken` with grace), and
  saved task writes with `ctx.memo()` plus snapshots on failure
  (`RunError::snapshot`, `Workflow::retry`).
- **Implemented in `agentbuild-providers` (foundation):** the 0002 types
  (`Message`, `ContentBlock`, `ToolSpec`, `ModelRequest`/`ModelResponse`,
  `Capabilities`), `ModelError` with `classify_http` (0003 §4), transcript
  check/repair (0002 §6), the `Model` trait, `FakeModel`, the SSE parser,
  the stream assembler, the HTTP `Transport` (feature `http`, on by
  default), the `openai_chat` and `anthropic_messages` protocols,
  `Endpoint` presets and `ProviderModel` (OpenAI, OpenRouter, Ollama,
  LM Studio, vLLM, Anthropic, any OpenAI- or Anthropic-compatible server).
- **Implemented in `agentbuild-agent` (slice 1):** the agent loop (0004):
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
- **Implemented in `agentbuild-storage` (0008 steps 1–5):** `Checkpoint`,
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
  `agentbuild_settings` refuses opening an encrypted database without
  its keys. SQLite runs with `secure_delete`. Step 6 (§16): semantic
  memory search. `Embedder` (`ProviderEmbedder` for OpenAI-compatible
  `/embeddings`, `FakeEmbedder` for tests) in providers; `Embedding`,
  `MemoryStore::put_embedded`, `SearchQuery::similar_to` (brute-force
  cosine in Rust, every backend; vectors encrypted with the rest) in
  storage; `Memory::semantic(embedder)`, `put_embedded`, `memory_text`
  in agents (keyword fallback when the embedder fails).
- **Implemented: audit trail (0010).** `AuditLog` (`InMemoryAuditLog`,
  `SqliteAuditLog`, `PostgresAuditLog`; table `agentbuild_audit`,
  migration 5), SHA-256 hash chain with `verify` and `head`,
  `Storage::with_audit` / `record`, fail closed (`audit_failed`). Agents
  record model and tool calls (before the result is saved), runs,
  approvals (decisions before the calls run), rewinds, deletions and
  memory writes (key hashed), with `Thread::actor`. No content in
  records. Core: `NodeContext::metadata` (agents put the audit context
  under `agentbuild_audit`).
- **Implemented: Python SDK, first slice (0011).** `sdks/python`
  (crate `agentbuild-python`, not a workspace member; PyO3 0.29 abi3,
  maturin): `Agent`, `@tool` (schemas from type hints; sync tools on a
  worker thread, async tools on the caller's loop), approvals, typed
  output, threads (send/approve/reject/decide/retry/history/rewind/
  delete/stream), `Storage` (SQLite, Postgres, encryption, audit,
  retention), `Memory`, errors by code, `run_sync`/`send_sync`,
  `FakeModel`. Bridge: one Tokio runtime, asyncio futures completed with
  `call_soon_threadsafe`, Python cancellation cancels the run. Tests:
  `sdks/python/tests` (pytest); `scripts/test-python.sh` installs the
  wheel into a new venv; `ci-local.sh` step `python`. Agent API added
  for it: `Thread::with_options`, `ToolContext::metadata`. Not yet:
  Python workflows, hooks, the gateway.
- **Implemented: TypeScript SDK, first slice (0012).** `sdks/typescript`
  (crate `agentbuild-node`, not a workspace member; napi-rs 3, N-API 6,
  Node 18+; package `@agentbuild/sdk`, TypeScript in `lib/` compiled to
  `dist/`): the same slice as Python. `Agent`, `tool({...})` with JSON
  Schema input, approvals, typed output, threads (incl. `stream*`),
  `AbortSignal` on every call, `Storage`, `Memory`, error classes by
  code (`cause` = what a crashing tool threw). Tools are weak
  thread-safe functions returning Promises. Tests: `node --test`
  (`sdks/typescript/test`); `scripts/test-typescript.sh` installs the
  npm tarball into a new project; `ci-local.sh` step `typescript`.
- **Implemented in `agentbuild-telemetry` (0009):** OTLP/HTTP JSON export
  of runs (trace = run, spans, generations, tool observations), Langfuse
  preset, content capture switch and masks, non-blocking observer
  (`RunOptions::observe`), run metadata (`RunOptions::metadata`).
- Tests: 520 across the five crates (with `--all-features`; includes the
  getting-started guide's examples), plus the package test. Release prep:
  `CHANGELOG.md`, `CONTRIBUTING.md`, `SECURITY.md`, `RELEASING.md`,
  `deny.toml` (CI job `dependencies`), `.github/workflows/release.yml`
  (manual; dry run unless "publish"). Crates still have `publish = false`
  until the user confirms the names.
- **Demo app:** `demos/support-chat` (Rust server on AgentBuild + Next.js
  UI; real OpenAI/Gemini models, manual only, not in CI or the workspace).
  Keys live in `~/.agentbuild-keys.env`, never in the repo.
- **Next:** the 10-task roadmap in `docs/progress.md` ("Roadmap
  tasks"), one full cycle each:
  gateway, error catalog
  and JSON Schemas, workflow threads, benchmarks. The user will build their
  hospital product (Axon Health OS) on AgentBuild: keep the framework
  general, but approval gates, audit trails, no patient data in logs, and
  self-hosted models matter.
- **Not started:** gateway, SDK workflows and hooks (Python, TypeScript), Standard Schema tool input, vector indexes
  (`sqlite-vec`, `pgvector`). Do not describe these as existing anywhere.

## Commands

```sh
cargo build
cargo test                       # unit + integration + doctests
cargo test --workspace --all-features   # also the SQLite backend
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
cargo test -p agentbuild-providers   # one crate
cargo run -p agentbuild-core --example basic_workflow -- "some text"
cargo run -p agentbuild-core --example approval_workflow -- yes
cargo run -p agentbuild-core --example llm_workflow -- "remote work"
cargo run -p agentbuild-core --example retry_workflow
cargo run -p agentbuild-core --example recover_workflow
cargo run -p agentbuild-agent --example approval_agent -- edit   # or approve, reject
cargo run -p agentbuild-agent --example thread_agent
cargo run -p agentbuild-agent --features sqlite --example durable_agent
# Langfuse, manual only (needs LANGFUSE_PUBLIC_KEY / LANGFUSE_SECRET_KEY):
cargo run -p agentbuild-telemetry --example langfuse
# Real model, manual only (needs a server; default: Ollama on localhost):
AGENTBUILD_MODEL=llama3.2 cargo run -p agentbuild-providers --example chat -- "hi"
# Claude (needs ANTHROPIC_API_KEY):
AGENTBUILD_PROTOCOL=anthropic AGENTBUILD_MODEL=claude-haiku-5-5 cargo run -p agentbuild-providers --example chat -- "hi"
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
bash scripts/test-package.sh     # package the crate, use it from a separate project
cargo deny --all-features check  # advisories, licenses, sources (deny.toml)
bash scripts/ci-local.sh         # the whole CI pipeline, locally (--quick: rust step only)
bash scripts/test-python.sh      # Python SDK: wheel into a new venv, consumer + pytest
bash scripts/test-typescript.sh  # TypeScript SDK: npm tarball into a new project, consumer + node --test
bash scripts/get-postgres.sh     # portable PostgreSQL into ~/.agentbuild/postgres (once)
bash scripts/with-postgres.sh cargo test --workspace --all-features   # with a throwaway server
# Every feature against real models, manual only (keys in ~/.agentbuild-keys.env):
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
   deps: tokio (features `rt`, `sync`, `time`; no `macros`), serde, serde_json,
   thiserror, uuid (v7 run ids). Dev only: tokio `test-util` for the paused
   clock. `agentbuild-providers` follows the budget in research
   `litellm-providers.md` §10.11 (today: serde, serde_json, thiserror, uuid,
   tokio `time`, reqwest behind the `http` feature).
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
   `crates/agentbuild-core/tests/`. Concurrency claims need tests that would
   deadlock or time out if the claim were false (see
   `fan_out_runs_nodes_concurrently`).
   **Simulate LLM calls; never call a real model in tests.** Use `FakeLlm`
   (`crates/agentbuild-core/tests/support/fake_llm.rs`): it replies after
   2–3 s (fixed per prompt) and can fail on a marker. Tests run it on Tokio's
   paused clock (`#[tokio::test(start_paused = true)]`) so timing is exact and
   the suite stays fast; examples run it in real time. See
   `tests/llm_simulation.rs`. Code that speaks the model contract
   (`agentbuild-providers`, later the agent loop) uses
   `agentbuild_providers::FakeModel` instead: the same 2–3 s timing, plus
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

## Git and GitHub

- **CI runs locally (decided 2026-10-14, for the next few months).** GitHub
  Actions does not run on push; `ci.yml` is manual only. Before every push,
  run `bash scripts/ci-local.sh` and push only when it passes. It needs the
  1.88 toolchain and `cargo-deny` (both installed on the dev machine; it
  skips and says so if they are missing). Local runs cover Windows only;
  start the CI workflow by hand before a release for Linux and macOS.

- Remote: `https://github.com/VARMAMASTER/agentbuild` (private). The `main` branch is protected by
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
| How does a run execute? | `docs/architecture.md` §4, `crates/agentbuild-core/src/runtime.rs` |
| What are the exact semantics? | `docs/design/0001-execution-model.md` |
| What's done, what's next, what's undecided? | `docs/progress.md` |
| How does LangGraph/LangChain do X? | `docs/research/*.md` |
| How do agents, tools and errors work (design)? | `docs/design/0002`–`0005`, `docs/architecture.md` §7 |
| Validation rules | `crates/agentbuild-core/src/graph.rs` (`build`), `src/error.rs` (`ValidationIssue`) |
