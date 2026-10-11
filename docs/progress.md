# Progress log

Newest entries first. Each entry records what changed, decisions made and why,
and what is next. Keep "Next steps" and "Open decisions" current. They are the
handoff point for whoever continues the work.

## Next steps

1. ~~Design review~~ Approved (see log, 2026-10-11 night).
2. **Done:** 0005 §1–§3 (NodeContext, node outcomes with suspend/resume and
   JSON snapshots, reducers). **Next:** events (§6) → policies (§4) → saved task
   writes and the task memo (§5) → snapshots on failure.
3. Phase 2 (Python SDK via PyO3/maturin) can start after 0005 §1–§3. Its first
   design doc covers asyncio ↔ Tokio bridging, free-threaded Python, and
   `abi3` wheels.
4. Implement the 0002 data types and the 0003 error catalog in Rust, with
   JSON Schemas generated for the SDKs.
5. `agentbuild-providers` foundation (0006 §11 step 1), in parallel with 3–4.

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
