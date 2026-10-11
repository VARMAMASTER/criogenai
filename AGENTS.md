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
  error codes. CI is green on Linux, Windows, macOS and MSRV 1.88.
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
  default), the `openai_chat` protocol, `Endpoint` presets and
  `ProviderModel` (OpenAI, OpenRouter, Ollama, LM Studio, vLLM, any
  OpenAI-compatible server).
- **Implemented in `agentbuild-agent` (slice 1):** the agent loop (0004):
  `model` ⇄ `tools` workflow, Rust tools (`tool_fn`), argument validation,
  concurrent calls in call order, timeouts, limits, model retries, events,
  memo-backed `Agent::retry`; slice 2: human approval (`Approval`,
  `review` node, `Agent::resume` with `Decision`s).
- **Implemented in `agentbuild-telemetry` (0009):** OTLP/HTTP JSON export
  of runs (trace = run, spans, generations, tool observations), Langfuse
  preset, content capture switch and masks, non-blocking observer
  (`RunOptions::observe`), run metadata (`RunOptions::metadata`).
- Tests: 283 across the four crates, plus the package test.
- **Next:** agent slice 3, structured output (0004 §6); then hooks (§3),
  remaining limits, model fallback and `Retry-After`, agents as nodes and
  tools; `anthropic_messages`. See `docs/progress.md`. The user will build their
  hospital product (Axon Health OS) on AgentBuild: keep the framework
  general, but approval gates, audit trails, no patient data in logs, and
  self-hosted models matter.
- **Not started:** the Anthropic protocol, gateway, Python SDK,
  TypeScript SDK, agent structured output/hooks, persistence. Do not
  describe these as existing anywhere.

## Commands

```sh
cargo build
cargo test                       # unit + integration + doctests
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
cargo test -p agentbuild-providers   # one crate
cargo run -p agentbuild-core --example basic_workflow -- "some text"
cargo run -p agentbuild-core --example approval_workflow -- yes
cargo run -p agentbuild-core --example llm_workflow -- "remote work"
cargo run -p agentbuild-core --example retry_workflow
cargo run -p agentbuild-core --example recover_workflow
cargo run -p agentbuild-agent --example approval_agent -- edit   # or approve, reject
# Langfuse, manual only (needs LANGFUSE_PUBLIC_KEY / LANGFUSE_SECRET_KEY):
cargo run -p agentbuild-telemetry --example langfuse
# Real model, manual only (needs a server; default: Ollama on localhost):
AGENTBUILD_MODEL=llama3.2 cargo run -p agentbuild-providers --example chat -- "hi"
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
bash scripts/test-package.sh     # package the crate, use it from a separate project
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
