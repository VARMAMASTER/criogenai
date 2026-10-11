# Progress log

Newest entries first. Each entry records what changed, decisions made and why,
and what is next. Keep "Next steps" and "Open decisions" current. They are the
handoff point for whoever continues the work.

## Next steps

1. ~~Design review~~ Approved (see log, 2026-10-11 night).
2. **Done:** all of 0005 (NodeContext, node outcomes with suspend/resume and
   JSON snapshots, reducers, policies, saved task writes and the task memo,
   snapshots on failure, events). Package testing in CI.
3. **Done:** `agentbuild-providers` foundation, part 1: 0002 types, model
   error classification, transcript repair, `Model` trait, `FakeModel`.
   Part 2 done: SSE parser and stream assembler. **Next:** HTTP transport
   with timeouts and dispatch tracking (adds reqwest + rustls; see research
   §10.11), then `openai_chat` (0006 §11 step 2) with golden translation
   tests and recorded streams.
4. The agent loop (0004) on top of core + providers, tested with `FakeModel`.
5. Phase 2 (Python SDK via PyO3/maturin): first design doc covers asyncio ↔
   Tokio bridging, free-threaded Python, and `abi3` wheels. Package test:
   install the built wheel in a clean venv.
6. Still open from the old plan: the 0003 run-error catalog generated from the
   Rust enums and checked in CI; JSON Schemas for the 0002 types for the SDKs
   (needs a schema generator dependency; decide when the SDKs start).

## Open decisions

| # | Decision | Status |
|---|---|---|
| D1 | State reducers | **Approved:** `replace` / `append` / `upsert_by_id` (0005 §3) |
| D2 | Where model providers live | **Decided (user, 2026-10-11):** our own light Rust provider layer (`agentbuild-providers`), learned from LiteLLM's MIT provider core. OpenAI-compatible protocol (gateways, local models) plus native Anthropic first; LiteLLM's model catalog used as data with attribution. External gateways stay usable via the OpenAI-compatible adapter. llmgateway is AGPL: study only. Designed in 0006 |
| D3 | Node-chosen next step / dynamic fan-out | **Approved:** `Command` with declared `may_goto` (0005 §2); `Send` deferred |
| D4 | Human-in-the-loop, checkpoints, resume | **Approved:** `Suspend` outcome plus JSON snapshots plus per-task saved writes (0004 §7, 0005 §5) |
| D5 | Event and streaming schema | **Approved:** seq-numbered envelope with origin (0005 §6) |
| D6 | Agent layer and middleware | **Approved:** fixed graph, six in-process hooks (0004) |
| D7 | License | **Decided:** MIT OR Apache-2.0 (user wants the gateway used in other people's projects, including commercial ones). Files added |
| D8 | Package names | **Open.** `publish = false` until verified |

## Log

### 2026-10-11: providers part 2: SSE parser and stream assembler

- `SseParser` (WHATWG): `
`, ``, `
` (also split across chunks), BOM
  (also split), comments, multi-line data, `id` persistence, `retry`,
  unterminated tail dropped. 2,000 pseudo-random chunk cuttings give
  identical events.
- `Assembler`: index-ordered blocks; text/reasoning concatenation (signature
  appended to the same block); tool-call args parsed once at block end;
  invalid JSON, non-object args, missing name → `invalid_tool_call`; `length`
  with an open call → `truncated`; missing/duplicate call ids repaired with
  warnings; no finish reason → `server` error with `dispatch: streaming` and
  `emitted_deltas`.
- Ten mutation checks: 9 caught, 1 equivalent (comment detection; an empty
  field name is ignored anyway). The CRLF mutant was first missed because an
  edit script wrote real CR/LF into a Rust literal, which Rust normalizes;
  the test now uses escapes and catches it.

### 2026-10-11: `agentbuild-providers` foundation, part 1

- New crate (workspace member, `publish = false`). Deps: serde, serde_json,
  thiserror, uuid, tokio `time`.
- 0002 types: `Message` (flat, tool-message fields on the same object;
  `validate()` checks the role/block table), `ContentBlock` (9 types,
  `#[non_exhaustive]` variants with constructors; `tool_call.args` typed as a
  JSON object), `MediaSource`, `ToolStatus`, `ToolResultError`, `Usage`,
  `FinishReason`, `MessageMetadata` (keeps unknown fields). `ToolSpec` +
  `validate_tools` (names, duplicates, reserved `final_output`, object schema,
  zero limits; every issue reported). `ModelRequest`, `ToolChoice` (`"auto"`
  … or `{"name"}`), `Settings`, `Reasoning`, `ModelResponse`,
  `Capabilities` (tri-state).
- `ModelError` / `ModelErrorKind` (14 kinds incl. `quota_exhausted`,
  `invalid_response`), `Dispatch`, `classify_http`: in-band code → status →
  text refinement for 400/413/422 only. Body shapes of OpenAI, Anthropic,
  Ollama, vLLM, llama.cpp. `Retry-After` as ms, seconds or HTTP date (own
  date parser, no dependency). `sanitize_message`: secrets, key-like tokens,
  bearer tokens, URLs, 2 KB bound.
- `transcript::check` / `repair` (0002 §6), deterministic repair ids,
  idempotent.
- `Model` trait (borrowed request; object-safe) and `FakeModel` (FakeLlm's
  timing plus scripted tool calls and errors).
- Clippy `result_large_err` allowed crate-wide: the `Ok` side is larger than
  `ModelError`, so boxing would not shrink the `Result`.
- Tests: 39 in the crate (29 unit, 8 integration, 2 doctests incl. the README).
  Ten mutation checks: 9 caught; the other was equivalent (unreachable arm).
  A missed check first found that no test proved delays vary by prompt; a
  test was added and it is now caught.
- Package test covers both crates. The consumer runs a workflow node that
  calls `FakeModel` and stores provider `Message`s under an `upsert_by_id`
  key: the two crates' JSON contracts fit together.

### 2026-10-11: 0005 §5 saved task writes, task memo, snapshots on failure; package tests

- Each finished node's output is saved in the step's progress as it
  arrives. When the step fails, `RunError.snapshot` (new, `Option<Box<Snapshot>>`)
  holds it in `pending_writes` (keyed by node name). `Workflow::retry` /
  `retry_with` re-run the step: saved outputs are reused (`node_finished` with
  new field `reused: true`, no `node_started`).
- `ctx.memo()` → `TaskMemo` (`get`, `put`, `once`): shared across attempts,
  saved in failure snapshots and in suspension snapshots (so a suspended node
  that drafted something does not draft again on resume). Starts empty in
  each new step.
- Blame rule: outputs that caused `update_conflict`, `invalid_value`,
  `missing_goto` or a goto `unknown_route` are not saved. Router failures keep
  outputs; routers re-run.
- Output returned during the cancel grace period is never saved (a node may
  return a partial result because it was cancelled). Memo entries are.
- Failure snapshots don't repeat `suspended`: resume values already supplied
  are kept as `pending_writes[node].resume` (a `null` value survives JSON).
- Snapshot gains `pending_writes` and `error {code, message}`, both serde
  defaults, so older snapshots load. `NodeOutput` is now serde (externally
  tagged). `RunError` is now `#[non_exhaustive]`.
- Cost: step outputs are cloned once when merged (kept for the snapshot).
  The step already clones the state; no benchmark yet.
- Tests: 135 passing (17 in `tests/memo.rs`). Eight mutation checks
  (reuse, memo sharing, blame rule, suspension memo, progress reset, saved
  resume value, null resume, saved outputs): all caught.
- Example `recover_workflow`: a run fails after sending an email; after the
  fix, `retry` finishes in 0.0 s with no repeated model calls.
- **Package tests** (user request): `scripts/test-package.sh` packages the
  crate, checks its contents, and runs `tests/package/rust-consumer` against
  the unpacked package. It found two real problems: the package had no README
  or license files (crate README, `LICENSE-*` copies and metadata added; the
  README example runs as a doctest), and `Workflow`/`WorkflowBuilder` did not
  implement `Debug`, so `build().unwrap_err()` did not compile in user code
  (fixed; `missing_debug_implementations` lint enabled). New CI job `package`
  (Linux and Windows) runs the docs build with `-D warnings`, the package
  test and every example.

### 2026-10-11: 0005 §4 policies implemented

- `NodePolicy { timeout_ms, retry }` per node via `builder.policy(..)`;
  `RetryPolicy` (max_attempts 3, 500 ms × 2 up to 20 s, full jitter). Both are
  serde data with defaults for missing fields.
- Retries only for `NodeError::retryable()` errors (new `retryable` field;
  `NodeError` is now `#[non_exhaustive]`). Jitter is deterministic from the
  task id and attempt, with no RNG dependency. Timeouts and panics are never
  retried.
- Run options: `deadline` (per `run`/`resume` call), `cancel_token`,
  `cancel_grace` (5 s). `RunStream::cancel()`.
- New: `CancelToken` (own ~60-line implementation on `tokio::sync::Notify`,
  with child tokens, instead of adding `tokio-util`); `ctx.cancel_token()`,
  `ctx.remaining_time()`.
- Engine: each node runs as a `NodeTask` (retry loop inside its own task).
  The step waits on `guard(..)`, a two-way race against cancellation and the
  deadline, written with `poll_fn` to avoid tokio's `macros` feature.
- New codes: `node_timeout`, `deadline_exceeded`, `cancelled`. New event:
  `attempt_failed`. `node_cancelled` reasons: `run_cancelled`,
  `deadline_exceeded`, `sibling_failed`. New validation:
  `DuplicatePolicy`, `InvalidPolicy`.
- 0003 catalog statuses brought up to date (several were stale "proposed").
- `FakeLlm::flaky_first(n)` simulates retryable provider errors.
- Tests: 112 passing (19 in `tests/policies.rs`, 9 unit tests). Five
  mutation checks: 4 caught. The fifth, a redundant token cancel on timeout,
  was removed because the step token already covers it.
- Example `retry_workflow`: a flaky model recovers on the 3rd attempt
  (9.2 s); a 1 s timeout on a 2–3 s call fails at 1.0 s.

### 2026-10-11: 0005 §6 events implemented

- `Event` envelope: `seq`, `run_id`, `step`, `node`, `task_id`, `origin`,
  `type` and `data`, plus `ts_ms`. Types: `run_started`, `step_started`,
  `node_started`, `node_finished`, `node_failed`, `node_cancelled` (new),
  `step_committed`, `suspended`, `run_finished`, `custom`.
- Subscribing: `event_channel(cap)` plus `RunOptions::events`, or
  `Workflow::stream(input)`, which returns a `RunStream` (`next()`,
  `finish()`; dropping it cancels the run). Nodes emit with
  `ctx.events().emit(name, payload)`.
- Delivery (`Hub`, `src/event.rs`): `emit` is synchronous; a full channel
  sends events to an in-order backlog, which the run drains at step boundaries
  (backpressure only there). A dropped receiver disables delivery and the run
  continues. Nothing is built without a subscriber.
- `seq` continues across resume via `Snapshot.event_seq` (`#[serde(default)]`,
  so older snapshots still load).
- Deviations from the 0005 sketch, recorded in 0005 §6:
  - `ts_ms` instead of an ISO string (no date-time dependency);
  - `parent_run_id`, `namespace` and `call_id` deferred until subgraphs and
    tools exist;
  - one subscriber per run.
- Core now uses tokio's `sync` feature (mpsc). No new crates.
- `RunStatus::Suspended.snapshot` is now `Box<Snapshot>` (clippy
  `large_enum_variant` after `event_seq` was added). `RunOutput::snapshot()`
  is unchanged.
- Tests: 84 passing (12 new integration tests in `tests/events.rs`, 4 new unit
  tests, 1 doctest). Two mutation checks confirmed the backpressure and
  real-time delivery tests fail when the behaviour is removed. The first
  version of the backpressure test did not catch its mutation and was
  strengthened.
- `llm_workflow` example now prints live events. Measured: 6.6 s for 5
  simulated calls against 10.8 s one after another.

### 2026-10-11: simulated LLM for tests and examples

- Requested by the user: test workflows that call an LLM with a dummy
  function that replies after 2–3 seconds.
- `tests/support/fake_llm.rs`: `FakeLlm` with a deterministic per-prompt delay
  (2–3 s by default), a reply derived from the prompt, a `failing_on(marker)`
  option, and recorded prompts. It is shared by tests and examples via `#[path]`.
  It is not part of the public API.
- `tests/llm_simulation.rs` (6 tests, on Tokio's paused clock, so they finish
  instantly):
  - one call takes 2–3 s;
  - sequential calls add up;
  - parallel calls take as long as the slowest;
  - a failing call aborts the step without waiting for a 30 s sibling and
    commits nothing;
  - the caller can bound a run with a timeout;
  - resume does not repeat finished calls.
- `examples/llm_workflow.rs`: plan → 3 parallel calls → join → write, in real
  time. Measured on the dev machine: 6.6 s against 10.8 s one after another.
- Dev-dependency: tokio `test-util` (paused clock).

### 2026-10-11 (late night): 0005 §1–§3 implemented

- `Node::run` now takes a `NodeContext` (`run_id` UUIDv7, `step`, `node`,
  `task_id`, `attempt`, `resume`) and returns a `NodeOutput`
  (`Update` / `Command{goto}` / `Suspend{payload}`). `node_fn` is unchanged;
  `node_with_context` was added.
- `may_goto` declarations are validated at build time (unknown targets,
  `GotoWithRouter`) and at runtime (`unknown_route`, `missing_goto`).
- Suspend/resume: `RunOutput.status` is `Completed | Suspended{interrupts,
  snapshot}`. `Workflow::resume(snapshot, values)` validates first (snapshot
  stays usable), and only suspended nodes see `ctx.resume()`. A suspended node
  does not count toward joins. The step limit counts across resumes.
- Reducers: `Replace` / `Append` / `UpsertById`, plus `$replace` and `$remove`.
  Merges run on a copy (atomic).
- Workflow fingerprint: FNV-1a of the structure, not of implementations.
- New error codes: `invalid_value`, `missing_goto`, `resume_invalid`,
  `workflow_mismatch`.
- New dependency: `uuid` (v7).
- Tests: 61 passing (18 new integration, 10 new reducer unit). New example:
  `approval_workflow`.

### 2026-10-11 (night): Design approved; open questions resolved

The user gave full freedom to choose ("you have full freedom to recommend and
choose the best thing"). The design set 0002–0008 is treated as **approved for
implementation**, with these resolutions:

- `storage=` kept.
- `on_busy` default is `wait` (60 s `busy_timeout`).
- SDK naming: `node` / `start` / `route` / `edge` / `join`.
- Decorator plus builder.
- TS state via Standard Schema or plain types.
- Python sync wrappers.

Implementation starts with 0005 §1–§3 (NodeContext, node outcomes, reducers).

### 2026-10-11 (evening): Product vision and provider decision

- **Vision (user):** AgentBuild is two products on one Rust core: a **fast
  agent engine** and a **fast LLM gateway**, both open source. The provider
  library serves the in-process agent engine first; a standalone
  OpenAI-compatible gateway binary builds on the same library later.
- **Provider choice:** learn from **LiteLLM** (MIT). Its Rust rewrite
  (`litellm-rust/`, 67 crates, ~144k lines) is not published and couples the
  provider core to auth, secrets, a Python bridge and gateway runtime, so we
  rebuild only the light core (~45k lines studied). **llmgateway** (theopenco)
  is AGPL-3.0: architecture-level study only, never ported; it remains usable as
  an external service.
- **Primary purpose (user):** people build their **agents and workflows** with
  AgentBuild. The developer-facing API is sketched in
  `docs/design/0007-public-api.md` (Python and TS, levels 0–4).
- **Provider layer designed (0006):** LiteLLM's Rust rewrite turned out to be a
  partial port running underneath Python (text-only chat, retries/fallback
  only in Python, `todo!()` stream stubs), so learning from it, not linking
  it, was right. Decisions: our 0002 types are canonical (no OpenAI-shaped
  intermediate format); protocol ≠ provider; adapters classify, the core
  retries; `dispatch` tracking (not_sent/sent); 60 s stream idle timeout; new
  error kinds `quota_exhausted` and `invalid_response`; a neutral `reasoning`
  setting; tri-state catalog capabilities from LiteLLM data with attribution;
  a gateway with a relay fast path and off-critical-path logging.
- **Persistence and memory (user):** interactive features must be very easy,
  and databases must connect for checkpoints and memory, as in LangGraph.
  Designed in `docs/design/0008-persistence-and-memory.md`: a single `storage=`
  URL (SQLite/Postgres) and a `Thread` object (`send`, `stream`, `approve`,
  `reject`, `edit`, `history`, `rewind`, `fork`, `update`, `delete`). Delta
  plus keyframe checkpoints avoid LangGraph's O(n²) growth. Compare-and-set on
  the thread head prevents silent forks. JSON-only storage is readable from
  both SDKs. A namespaced long-term memory store has keyword or vector search.
  Backends are written once in Rust (sqlx).
- **Embeddable gateway (user):** people should use the gateway *inside their
  projects*. So it is a library first (Rust, Python, TS), mountable in the
  user's own server, and also a standalone binary. No required infrastructure,
  no global state, Cargo features per provider (architecture §1). License set
  to MIT OR Apache-2.0.
- **"Faster" is a claim to prove:** benchmarks against LangGraph/LangChain
  (agent loop; baseline 2.4–8.5 ms per step measured) and LiteLLM proxy
  (gateway overhead) before any public claim.

### 2026-10-11 (later): Research complete; agent, tool and error design

**Research** (`docs/research/`, all from installed source, cited `path:line`):
- `langgraph.md`: Pregel engine schedules channels; reducers via
  annotations; per-task saved writes; resume re-runs node code; unknown
  `goto` targets are silently dropped; checkpoints embed Python class names.
- `langchain-core.md`: new language-neutral `langchain_protocol` event
  types; standard content blocks; `+`-based chunk merging is quadratic;
  a streaming callback changes `invoke` behaviour; LangSmith hard dependency;
  ~135 µs overhead per Runnable call.
- `langchain-agents.md`: middleware compiled into graph nodes; no iteration
  cap (recursion limit 9 999); inconsistent error defaults; HITL is the most
  mature part; 2.4–8.5 ms framework overhead per agent step (measured).
- `build-analysis.md`: **REA used** (user request). Installed REA 6.4.0 with a
  portable Node 24 in `research/tools/` (system Node untouched). REA on the 5
  published wheels: all pure Python (`py3-none-any`, hatchling), 355 files,
  hashes verified, zero native code, no further probes. The stack's native
  code is in dependencies (pydantic-core, orjson, ormsgpack, uuid-utils), all
  Rust via maturin. REA classified orjson's `.pyd` as native x86-64; deep
  decompilation would need Ghidra/IDA/Hopper (not installed, not needed).

**Designed** (all *Proposed*):
- 0002 messages and tools: tool calls are content blocks (single source of
  truth); tool-result statuses; transcript-validity invariant with repair.
- 0003 error model: categories by "who can fix it"; errors never become
  fake model output; suspension/cancellation are outcomes, not exceptions;
  stable code catalog; 18 edge cases.
- 0004 agent loop: fixed `model ⇄ tools (+review)` graph; six hooks with
  explicit ordering; routing tables; default limits; structured output by
  declared capability; HITL with review before any call executes.
- 0005 core extensions: NodeContext, `Update | Command | Suspend`, reducers,
  policies, JSON snapshots with per-task saved writes (prevents duplicate side
  effects on resume), event envelope.

### 2026-10-11: Phase 1 core, research setup, repository

**Built**
- Rust workspace and `agentbuild-core`. Superstep execution engine: static
  edges, async routers, joins (wait-all), cycles with a step limit,
  concurrent nodes per step, atomic steps, conflict detection, panic capture,
  stable error codes. 33 tests; runnable example
  (`Input → Transform → Conditional Branch → Output`).
- Docs: README, `docs/design/0001-execution-model.md`, `docs/architecture.md`,
  `AGENTS.md`.
- CI (fmt, clippy `-D warnings`, tests on Linux/Windows/macOS, MSRV 1.88). The
  first run passed on all jobs.
- Private GitHub repo `VARMAMASTER/agentbuild`; initial commit pushed.

**Decided**
- **Superstep (bulk-synchronous) execution** over a pure DAG (can't express
  agent loops) or free-running actors (timing-dependent merges, hard to
  checkpoint). See 0001.
- **JSON-object state.** The one representation Rust, Python and JS share.
  Typed state can be layered on in the SDKs.
- **Same-key writes in one step are an error** for now. Reducers (D1) may
  replace this.
- **Routers are async** because JS callbacks can't be invoked synchronously from
  Tokio threads.
- **Strict validation** (no router and static edges on one node; join targets
  reachable only through their join). It can be relaxed later without
  breaking users.
- **Study by reading source, not reverse engineering.** LangChain and LangGraph
  are pure Python (~125k lines, no compiled code). The REA tool targets
  binaries and needs Node ≥22.19, so the user chose direct source reading.

**Environment**
- Installed Rust 1.99 via rustup (MSVC toolchain; VS 2019 Build Tools present).
- `research/.venv` with langchain 1.4.4, langchain-core 1.6.9, langgraph
  1.2.14.
