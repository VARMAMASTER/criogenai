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
   Parts 2–4 done: SSE parser, stream assembler, HTTP transport,
   `openai_chat` + `ProviderModel` + presets.
4. **Done:** the whole agent list of 0004 (loop, tools, approval,
   structured output, hooks, limits, model resilience, agents as nodes and
   tools, tool features, `agent.stream()`); observability and Langfuse
   (`agentbuild-telemetry`, 0009); `anthropic_messages` (0006 §11 step 3);
   persistence steps 1–2 (0008 §11–§12: `agentbuild-storage`, threads,
   per-step saving and crash recovery, SQLite with delta + keyframe
   checkpoints and migrations).
   Demo app `demos/support-chat` built and tested by hand with real models.
   Persistence step 3 (0008 §13): memory store and agent `Memory`.
   Persistence step 4 (0008 §14): Postgres and thread locks across
   processes. Step 5 (0008 §15): retention and encryption at rest.
   **Next:** the roadmap tasks below.
5. Phase 2 (Python SDK via PyO3/maturin): first design doc covers asyncio ↔
   Tokio bridging, free-threaded Python, and `abi3` wheels. Package test:
   install the built wheel in a clean venv.
6. Still open from the old plan: the 0003 run-error catalog generated from the
   Rust enums and checked in CI; JSON Schemas for the 0002 types for the SDKs
   (needs a schema generator dependency; decide when the SDKs start).

## Roadmap tasks

Started 2026-10-15 at the user's request: ten tasks, one at a time, each
with a full cycle (spec, implementation, tests, review, local CI, commit
and push). A task is marked done only when all of that is verified.

| # | Task | Spec | Status |
|---|---|---|---|
| 1 | Retention and encryption at rest | 0008 §15 | Done |
| 2 | Vector (semantic) memory search | 0008 §16 | Done |
| 3 | Audit trail | 0010 | Done |
| 4 | `thread.stream` | 0008 §17 | Done |
| 5 | Python SDK (first slice) | 0011 | Done |
| 6 | TypeScript SDK (first slice) | 0012 | Done |
| 7 | LLM gateway (first slice) | 0006 §12 | Done |
| 8 | Error catalog and JSON Schemas | 0013 | Done |
| 9 | Workflow threads | 0008 §18 | Done |
| 10 | Benchmarks | 0014 | Done |

Second roadmap, started 2026-10-16 at the user's request ("finish all the
remaining tasks one by one"), same full cycle:

| # | Task | Spec | Status |
|---|---|---|---|
| 11 | Packaging fixes (licences, metadata, Python types, gateway release) | RELEASING.md | Done |
| 12 | Linux and macOS runs, fix what they find | — | Linux done (GCP VM); macOS blocked (GitHub billing) |
| 13 | Workflows in the Python and TypeScript SDKs | 0015 | Done |
| 14 | Hooks in the SDKs | 0017 §5 | Done |
| 15 | Semantic memory and telemetry in the SDKs | 0017 §3–4 | Done |
| 16 | `Gateway` client in the SDKs | 0007 Level 0 | Planned |
| 17 | TypeScript tool input with Standard Schema (Zod) | 0007 decision 3 | Planned |
| 18 | Gateway: `/v1/messages`, relay path, rate limits, budgets | 0006 §7 | Planned |
| 19 | Providers: OpenAI Responses, native Gemini | 0006 §11 step 7 | Planned |
| 20 | Vector indexes (pgvector, sqlite-vec) | 0008 §16 | Planned |

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

### 2026-10-22: framework comparison, tool exceptions, site and licence decisions

- **Comparison with LangGraph and CrewAI (design 0016):** `benches/compare/`
  runs the same agents, workflows, loads (1/100/1000 runs at once) and
  injected faults on all three, in Python, one venv per framework, one
  process per measurement, one simulated model; results in
  `bench/results/*.json` (`agentbuild-compare/1`), shown by the website's
  benchmark lab as comparison graphs and tables. Fairness choices are
  in the file's notes (CrewAI cache off and BaseTool tools for its
  checkpoints; each framework's own async API; native tool calling).
  Found while building it: a harness bug (parallel tools overwrote each
  other's log lines; now one lock) and that one load batch is noisy on a
  laptop (now three batches, median reported, all three recorded).
- **Tool exceptions (0003 §5.1, 0004):** an exception a tool did not
  handle (or a Rust panic, now caught around the call) reaches the model
  as `tool_failed` by default; `on_tool_exception` = `return_to_model` |
  `return_generic` | `fail`. Python and TypeScript bridges keep the
  exception as a cause only under `fail`. The comparison showed AgentBuild
  and LangGraph failing this test while CrewAI passed; the user asked for
  the change.
- **Python SDK workflows (0015, task 13 Python half):** `Workflow`,
  routes, joins, reducers, goto, suspend/resume, threads, agents as
  nodes. `FakeModel(fail_first=, by_turn=)`; Rust `FakeModel::by_turn`,
  `without_request_log`.
- **Website:** rebuilt in Next.js (`website/`, static export): the
  landing page, all docs pages, and the benchmark lab with "Head to head"
  graphs. All GitHub links removed (the repository is private).
- **Decisions by the user:** the code opens at 10,000 installs; until
  then everything is free to use under the AgentBuild Free Licence
  (`sdks/*/LICENSE`, draft: have it reviewed before release); Python and
  TypeScript are published first, Rust crates when the code opens; the
  benchmark kit is published on its own (`benches/compare/package.py` →
  the site); a live install counter on the home page. Package names:
  `agentbuild-sdk` on PyPI ("agentbuild" was taken by an unrelated project
  on 2026-10-21; `import agentbuild` stays), `@agentbuild/sdk` on npm (the
  user owns the org).

### 2026-10-17: Linux (task 12)

- GitHub refused the approved CI and SDK-package runs ("recent account
  payments have failed or your spending limit needs to be increased");
  nothing ran or was charged. macOS stays untested until billing is fixed
  or the repo is public.
- Linux instead on a temporary GCP VM (`agentbuild-ci`, project
  `resumingai-app`, e2-standard-8, Ubuntu 24.04; Rust 1.99.1 and 1.88,
  Node 22, Python 3.12, portable Postgres 18.6), code copied from the
  working tree over SSH: `scripts/ci-local.sh` passed all eight steps in
  17 minutes, with nothing to fix. First Linux run since 2026-10-13.
- Found while copying: `.gitignore` hid the new `_native.pyi` stub (the
  pattern `_native.*` was meant for build outputs); narrowed.
- A wheel built directly on Ubuntu 24.04 is tagged `manylinux_2_38`
  (installs only on recent Linux); the SDK-package workflow builds in a
  `manylinux_2_28` container for wider use, as intended.
- The Windows pipeline for task 11 was stopped at `msrv` by low memory
  (`rust` and `postgres` had passed); the same tree then passed all steps
  on Linux.

### 2026-10-16: packaging fixes (task 11)

- Both SDK packages now ship `LICENSE-MIT` and `LICENSE-APACHE`, with
  project links and keywords; the wheel uses PEP 639 licence metadata
  (`License-Expression`, `License-File`).
- The Python package is typed: `py.typed` and a stub for the native
  module. mypy found one real error (two `call` adapters with different
  signatures), fixed; `scripts/test-python.sh` now runs mypy on the
  installed package and the consumer.
- `agentbuild-gateway` is in the release workflow's publish order.
- SDK binaries are built with `strip = "symbols"`: no change on Windows
  (16.6 MB; the symbols are in a separate PDB there); the stripped Linux
  addon is 16.5 MB, the Linux wheel 7.2 MB. Fat LTO
  made the Windows addon 21% smaller (13.1 MB) but the build took 13
  minutes instead of 1.5; not adopted.

### 2026-10-16: release preparation (no user decision needed)

- `Storage::close()` (crate-internal `Backend` trait, which also carries
  `reencrypt`), and `close()` in both SDKs. Its test found that after
  closing, Windows can keep the SQLite file busy for about 20 ms (another
  program, typically the virus scanner, opening the new file); the test
  retries the delete for up to a second, and also shows that without
  `close` the file stays locked.
- `.github/workflows/sdk-packages.yml`: wheels (maturin-action, abi3,
  manylinux 2_28) and npm addons on Linux x64/arm64, macOS arm64 and
  `macos-15-intel` (`macos-13` was retired in December 2025), Windows
  x64; each tested in a fresh environment on its platform; publishing
  only when asked (PyPI trusted publishing, npm token). Not run yet: it
  needs the user's go-ahead (paid minutes while the repo is private).
- RELEASING.md covers the SDKs, the gateway crate and the registry
  setup. Names checked free on 2026-10-16: all crates, PyPI
  `agentbuild`, npm `@agentbuild/sdk` (unscoped `agentbuild` is taken).

### 2026-10-16: benchmarks (design 0014, task 10)

- `crates/agentbuild-bench` (workspace member, never published) with a
  reusable instant model, `benches/compare_langgraph.py`,
  `benches/python_bridge.py`, `benches/typescript_bridge.mjs`;
  `docs/benchmarks.md` records the run (Ryzen 5 4600H, Windows 11).
  `ci-local.sh` runs the harness in `quick` mode so it keeps working.
- Headline numbers (trivial nodes, this machine): 21.4 µs per workflow
  step against LangGraph's 244 µs; 3.9 µs per parallel node against
  334 µs; the gateway adds 0.14 ms per request on localhost; a Python
  call adds 0.2 ms and a Python tool 0.3 ms.
- Found: `FakeModel` at zero delay waited for a timer tick (~1 ms on
  Windows), which first made the SDKs look 1.5 ms slower per call. Fixed
  with a timer-free yield (not `tokio::task::yield_now`, which needs a
  Tokio feature the providers crate does not use). Two telemetry tests
  had relied on the slowness (one span batch per run); they now check
  retries per batch, and passed three runs in a row.

### 2026-10-16: workflow threads (0008 §18, task 9)

- `WorkflowThread` in `agentbuild-storage` (not the agent crate, so
  workflows without agents need no agent dependency): `run`, `resume`,
  `retry`, `cancel`, `head`, `state`, `interrupts`, `history`, `rewind`,
  `delete`, `actor`, `with_options`; `ThreadError` (`Run`, `Storage`,
  `State`) with catalog codes; `Turn`.
- Same rules as agent threads: leases and `OnBusy`, the
  `thread_suspended` / `thread_interrupted` guards, per-step saving,
  failures saved with their snapshot, `run.finished` and `run.resumed`
  audit records; refused resume input writes nothing.
- Tests (4): an expense workflow paused for approval and resumed from
  another handle (exact audit trail), refused input, failure then retry
  without re-running finished nodes, rewind to the pause, cancel saved as
  failed, a pause surviving a reopened SQLite database. Package consumer
  extended.

### 2026-10-16: error catalog and JSON Schemas (design 0013, task 8)

- `schemas/error-codes.json` (53 codes) and `schemas/agentbuild.schema.json`
  (28 definitions). `docs/errors.md` generated from the catalog; 0003 §9
  now points to it.
- The source scan found a code no document listed:
  `memo_type_mismatch` (core's task memo), now in the catalog. It also
  found SDK mappings that disagreed with the catalog
  (`step_limit_exceeded`, `deadline_exceeded`, `workflow_mismatch` raised
  the base class); fixed in both SDKs.
- The validator gained `anyOf`, `oneOf`, `allOf` and local `$ref`
  (with a depth guard for self-references); for a failed `oneOf` it
  reports the variant whose `const` discriminator matches. Tool
  arguments are checked with it too, so Pydantic `Optional` and nested
  models are now enforced (a deliberate tightening, 0013 §4).
- Tests: catalog (4), schemas (5: every content block, delta, model
  error kind, decision, audit record, real agent messages and approval
  requests; wrong values rejected with paths), validator unit test, the
  gateway checks its usage records, both SDKs check every catalog code.
- A dev Python module built before task 5's cancel fix made one test
  fail locally; rebuilding fixed it (the pipeline always builds fresh).

### 2026-10-16: LLM gateway, first slice (design 0006 §12, task 7)

- New crate `agentbuild-gateway` (axum 0.8, toml 0.9): server-side OpenAI
  codec (requests → 0002, answers and deltas → `chat.completion` /
  `chat.completion.chunk`), routes with fallback (transient errors and
  `quota_exhausted`; streams only before the first delta), virtual keys
  as SHA-256 digests, usage records via `try_send` with a drop counter,
  OpenAI error bodies and status codes, the upstream task aborted when
  the client's body stream is dropped.
- The stream encoder sends at the end whatever the upstream did not
  stream (FakeModel streams text in one piece and tool calls not at
  all; real providers stream everything).
- Binary: TOML config whose secrets are environment variable names;
  refuses to serve without keys off loopback unless `--allow-open`;
  `--log-usage` prints usage records (no content) as JSON lines.
- Tests: 9 (codec and config unit tests; end to end with AgentBuild's
  own OpenAI client and raw HTTP: streamed tool calls, SSE framing and
  `[DONE]`, usage chunk, error kinds and statuses, fallback for plain and
  streamed calls, keys, dropped usage records, client disconnect).
  Package test and consumer extended. Live (manual): the official
  `openai` Python SDK 3.31 against the binary with OpenAI and Gemini
  upstreams, 10/10 (plain, streamed with usage, tool round trip,
  streamed tool calls, bad key 401, unknown route 404); the usage log
  held no key.

### 2026-10-16: TypeScript SDK, first slice (design 0012, task 6)

- `sdks/typescript`: crate `agentbuild-node` (napi-rs 3, outside the
  workspace; no `@napi-rs/cli`, cargo builds and a script copies the
  addon to `native/agentbuild.node`) and `@agentbuild/sdk` in TypeScript.
- Bridge: napi async methods on napi's Tokio runtime; tools as weak
  thread-safe functions `(request) => Promise<envelope>` that Rust
  awaits (no event-loop bookkeeping, unlike Python); `AbortSignal`
  fires a cancel token registered before the call starts; errors cross
  as JSON in the message. Streams hold `ThreadStream::cancel_token`.
- Tests: 13 `node --test` tests (simulated models);
  `scripts/test-typescript.sh` packs with npm, installs the tarball into
  a new project, runs an ES-module consumer and the tests; `ci-local.sh`
  step `typescript`. Live (manual, `examples/live.mjs`): OpenAI and
  Gemini 4/4.
- Found: on Windows a test's SQLite file stays locked while its
  `Storage` is open, and `Storage` has no `close()` (only the backends
  do). Tests clean up best effort, as the Python tests do; a
  `Storage::close` is an open item.
- Node 22's `node --test test/` reads `test/` as a file: the scripts
  pass the test files.

### 2026-10-16: Python SDK, first slice (design 0011, task 5)

- `sdks/python`: crate `agentbuild-python` (cdylib, outside the
  workspace) with PyO3 0.29 (abi3-py39; 0.30 was three days old, so the
  settled line) and the pure-Python package `agentbuild`. Configuration
  crosses as JSON; tools as adapters returning JSON envelopes.
- Bridge (0011 §4): one Tokio runtime; each awaitable call returns an
  asyncio future completed with `call_soon_threadsafe`; cancelling it
  fires the run's `CancelToken`. Async tools run on the caller's loop
  (its id travels in the run metadata, read through the new
  `ToolContext::metadata`); sync tools on `spawn_blocking`. A crashing
  tool's exception becomes the `tool_failed` error's `__cause__`.
- Agent API added for it: `Thread::with_options` (default options for a
  handle's operations, so approve/reject/retry and their streams carry
  the bridge's metadata) and `ToolContext::metadata`.
- Tests: 15 pytest tests (simulated models); `scripts/test-python.sh`
  builds a release wheel, installs it into a new venv, runs
  `examples/consumer.py` and the tests from outside the source tree;
  `ci-local.sh` step `python`. Live (manual, `examples/live.py`):
  OpenAI and Gemini 4/4 (async tool, memory across turns, approval,
  streaming); a first Gemini failure was the script's own shared state.
- Found while planning the TypeScript bridge: `stream.cancel()` could not
  reach a run while another task waited in `next()` (the stream was
  locked). Fixed with `ThreadStream::cancel_token`, held apart from the
  stream; tests in Rust and Python cancel a stream that is being read.
- The first full pipeline was stopped by the machine running out of
  memory (a compiler process failed to start); rerun with 2 build jobs:
  all seven steps passed.

### 2026-10-16: `thread.stream` (0008 §17, task 4)

- `ThreadStream` and `Thread::stream` / `stream_with` / `stream_approve`
  / `stream_reject` / `stream_decide` / `stream_retry`. The operation
  runs in a Tokio task; dropping the stream cancels (never aborts) so the
  run is committed as failed and `retry` continues it.
- Found while testing: cancelling waited for a model call in progress
  until the core's cancel grace ended (5 s, still paying for tokens).
  The model node now stops the call when the run's token fires. Done
  without `tokio::select!`: the agent crate has Tokio's `macros` only for
  tests, so a `select!` would have broken normal builds (caught by
  `cargo build` without dev-dependencies).
- Tests: streamed text and a saved reply, approvals streamed on both
  sides (audited), a refused stream ends with no events, reject and
  decide, a dropped stream saved for retry, cancel in under a second
  (was 5 s).

### 2026-10-16: audit trail (design 0010, task 3)

- New design 0010. Storage: `AuditLog` (in-memory, SQLite, Postgres;
  migration 5), records chained by SHA-256 over canonical JSON (keys
  sorted by us, so serde_json's `preserve_order` in a downstream build
  cannot change hashes), `verify` (paged), `head`. Appends serialize per
  database (SQLite write lock, Postgres advisory transaction lock).
  `Storage::with_audit` / `with_audit_log` / `record`; prune and
  reencrypt record themselves.
- Agents: model and tool calls recorded in the nodes (tool calls before
  their result is saved, so a failed record makes the step fail and a
  retry records the call); thread operations record outcomes, approval
  requests, decisions (validated first, recorded before the calls run),
  rewinds and deletions; `Thread::actor`. Memory keys only as SHA-256.
  Core gained `NodeContext::metadata` for the audit context.
- Fix found by the tests: `audit_failed` was reported as `node_failed`
  (not in the agent's pass-through list).
- Tests: the log contract on three backends, tamper detection by raw SQL
  (edit, deletion), 3 processes × 30 appends into one chain (SQLite,
  Postgres), fail closed in storage and agents, full approval story with
  two actors and no content in any record.

### 2026-10-15: semantic memory search (0008 step 6, task 2)

- Design 0008 §16. Providers: `Embedder` trait, `ProviderEmbedder`
  (OpenAI-compatible `POST /embeddings`, results put back in input
  order by `index`, classified errors, key redacted; Anthropic endpoints
  refused) and `FakeEmbedder` (word-hash vectors with synonyms, failing
  mode, call log).
- Storage: `Embedding`, `put_embedded`, `similar_to`, `min_score`;
  migration 4 adds `agentbuild_memory.embedding` (base64 f32, encrypted
  with keys and rewritten by `reencrypt`). Ranking in Rust on every
  backend; no vector index (brute force over the prefix).
- Agents: `Memory::semantic(embedder)`: recall and `search_memory` by
  meaning with keyword fallback (the `memory_recalled` event says which,
  and why); `remember` stores vectors (a failing embedder is a retryable
  tool error); `put_embedded` and `memory_text` for app-saved facts.
- Tests: the vector contract on the three backends; encrypted vectors
  unreadable and re-encrypted; `ProviderEmbedder` against the scripted
  server; agents with a fake embedder (by meaning where keywords fail,
  fallback, helper errors). Live check (manual): OpenAI
  `text-embedding-3-small` (1536 dims) and Gemini `gemini-embedding-001`
  (3072 dims) both rank the allergy first for "Which antibiotics should I
  stay away from?", and the model's answer uses it.
- The disk filled up during the work (target/ at 28 GB): deleted
  target/debug and rebuilt.

### 2026-10-15: retention and encryption at rest (0008 step 5, task 1)

- Design 0008 §15. Retention: `Retention` (`thread_ttl`,
  `keep_checkpoints`, `drop_branches`), `Storage::prune` and
  `spawn_pruner` (incremental after the first pass). Expired threads are
  leased with `Reject` first, so a thread a run holds (in any process on
  Postgres) is skipped. Shortening rewrites the oldest kept checkpoint
  as a keyframe in the same transaction (Postgres locks the thread row).
  `Checkpointer` has seven methods now (`threads`, `prune_thread`).
- Encryption: feature `encryption` (aws-lc-rs, already in the tree for
  TLS; base64, already there via sqlx). Format `enc:v1:<id>:<base64>`
  with a per-row HKDF-derived AES-256-GCM key and the row's identity as
  additional data. Memory keys become HMAC tags; keyword search decrypts
  the prefix's items and ranks in Rust. Migration 3 adds
  `agentbuild_settings`: an encrypted database refuses to open without
  keys, and a wrong key fails at connect. `reencrypt` rotates keys and
  encrypts databases written before. SQLite: `secure_delete` on.
- Fixed two races in interrupts across processes (found by the local CI
  under load): the lock task listened only after taking the lock, and an
  interrupt that came before the run registered its cancel token was
  lost. Now it listens first, interrupts name the holder's server process
  (`pg_locks`), and a lease remembers an early interrupt (with a
  generation, so it never reaches the next lease). New tests:
  `an_interrupt_before_the_run_registers_still_stops_it`,
  `an_interrupt_reaches_only_the_holder_even_before_its_run_registers`.
  The agent test that overlapped two runs by a fixed 150 ms now starts the
  second run once the first one's model call is under way.
- Windows: aws-lc-sys needs `prebuilt-nasm` when built without rustls
  (storage alone), so the dependency enables it.
- Tests: contract (thread listing, pruning with an abandoned branch and
  deltas) on all three backends; retention rules and the background
  pruner; encryption on SQLite and Postgres (nothing readable in any
  content column, wrong or missing keys, rotation, `reencrypt`, a moved
  row refused, turning it on later); deleted rows gone from the SQLite
  file (fails without `secure_delete`); an agent conversation with
  remembered facts leaves no readable trace in the file or its WAL (fails
  on plain storage), then continues after retention.

### 2026-10-15: Postgres (0008 step 4)

- `agentbuild-storage` feature `postgres`: `PostgresCheckpointer`
  (`connect`, `connect_with` + `PostgresOptions`: connections, lock
  connections, acquire timeout), `PostgresMemoryStore`, and
  `PostgresThreadLocks`. `Storage::connect("postgres://…")` sets up all
  three. The schema and migration versions are SQLite's. Migrations run
  under a transaction advisory lock. Agents get the same feature
  (forwarded).
- The encoding shared by both SQL backends moved to `src/sql.rs`
  (deltas, keyframes, snapshot without state, state cache): one
  implementation, so SQLite and Postgres store the same thing.
- `ThreadLocks` trait (new, backend-neutral) and
  `Storage::with_locks`. `Storage::lease` takes the in-process lease,
  then the cross-process lock; `busy_timeout` covers both. Interrupts
  across processes cancel the holder's run. Postgres implements it with
  session advisory locks held by one task per lock, with `lock_timeout`
  for waits and `LISTEN`/`pg_notify` for interrupts.
- Errors: Postgres URLs are never echoed (passwords); NUL and similar
  text is `invalid_value`. SQLite memory: a TTL beyond `i64` no longer
  wraps to "already expired".
- TLS: rustls on aws-lc-rs (already in the tree via reqwest).
  `cargo update -p rustls` to 0.23.46 for RUSTSEC-2026-0346 / 0347,
  which were found by `cargo deny` and were already present via reqwest.
- Testing: `scripts/get-postgres.sh` (portable binaries, checksummed;
  no install) and `scripts/with-postgres.sh` (a throwaway cluster with
  a TLS certificate from a throwaway CA). `ci-local.sh` has a new
  `postgres` step, and the package step runs with a server. The GitHub
  workflow has a manual Postgres 13/18 job. Tested locally on 13.23 and
  18.6.
- Tests:
  - storage: 10 (contract, reconnecting, stored format, concurrent
    migrations, bad URLs and NUL, TLS, 9 writers in 3 processes, locks
    across processes, interrupt, lost connection), plus the memory
    contract and prefix edges;
  - agent: 4 (turns, interrupt and reject, shared memory, a 3-process
    stress test), plus crash recovery by another process;
  - the package consumer.
- Mutation check: with the locks left out of `Storage::connect`, the
  three lock tests fail.

### 2026-10-14: long-term memory (0008 step 3)

- `agentbuild-storage`: `MemoryStore` (put / get / search / delete /
  `delete_namespace` / `prune_expired`), `Namespace` (segment paths,
  checked), `MemoryItem`, `SearchQuery`, `ScopedMemory`, `InMemoryStore`,
  `SqliteMemoryStore` (migration 2, `agentbuild_memory`; same database as
  threads via `Storage::connect`). `Storage::memory()` / `with_memory`.
  New storage kind `invalid_value` (root namespace, bad key, value over
  1 MiB).
- Keyword search ranks in Rust with one function for every backend
  (whole word 1, word start 0.5, mean over the query's words minus stop
  words; ties newest, then path and key), so results do not depend on the
  backend. SQLite only narrows rows with `instr`. FTS5 deferred until a
  benchmark needs it.
- `agentbuild-agent`: `Memory` (`by_state`, `shared`, `resolve`;
  `recall(n)` default 5, `tools(bool)`, `ttl`), `AgentBuilder::memory`,
  `ToolContext::memory()`, tools `remember` / `forget` / `search_memory`.
  Recall goes into the request as a system message `msg_memory` after the
  instructions, never into the transcript; the `memory_recalled` event
  has keys only (no values: personal data). A run whose namespace cannot
  be resolved fails with `invalid_state` before any model call; there is
  no fallback to a shared namespace (Axon: one patient's facts must never
  reach another's run).
- `AgentInput::with(key, value)`: run state values (new; needed so the
  scope can come from the run). In threads they carry over to later
  turns. The agent's own keys are refused (`invalid_input`, new code in
  0003).
- Tests: 5 storage contract/unit tests (both backends, upgrade from a
  version-1 database), 4 store unit tests, 9 agent tests (recall order and
  isolation per user, limits and switches, missing scope, the tools with a
  bad key, TTL, threads, reserved keys, custom tools, SQLite across
  restarts), the `Memory` doctest. Package consumer extended.

### 2026-10-14: CI runs locally

- Decision (user, for the next few months): GitHub Actions no longer runs
  on push or pull requests; `.github/workflows/ci.yml` is manual only
  (`workflow_dispatch`). Code is still pushed to GitHub.
- `scripts/ci-local.sh` runs the same jobs on the dev machine: `rust` (fmt,
  clippy with all and default features, tests, doctests, no-`http` tests),
  `msrv` (`cargo +1.88 check`), `deps` (`cargo deny`), `standalone` (live
  check and demo server lint), `package` (docs, package test, examples).
  `--quick` runs only `rust`; step names run single steps. It stops at the
  first failure. Rust 1.88 (minimal profile) and cargo-deny 0.20.2 are
  installed on the dev machine for it.
- Lost coverage: Linux and macOS. Before a release, start the CI workflow
  by hand (`RELEASING.md`). Last green GitHub run: `3ca76cc`.

### 2026-10-13: release preparation (0.1.0, Rust crates)

- README rewritten to say what is built and what is planned (it claimed a
  gateway and Python/TypeScript use, and called the engine "fast" without
  benchmarks). Roadmap rows updated.
- `docs/getting-started.md`: workflow, agent with a tool (real model and
  `FakeModel`), approval, SQLite threads, streaming, structured output.
  Every Rust block runs as a doctest of `agentbuild-agent` (with the
  `sqlite` feature). Writing it found two traps, now documented: SQLite
  storage under Tokio's paused clock times out at once; structured output
  with a model without JSON mode answers through `final_output`.
- `Endpoint::gemini(key)` preset (verified live).
- `CHANGELOG.md`, `CONTRIBUTING.md`, `SECURITY.md`, `RELEASING.md`.
- `deny.toml` + CI job `dependencies` (cargo-deny 0.20 with all features:
  no advisories, permissive licenses only, crates.io only).
- docs.rs builds all features; `release.yml` (manual: tests, package
  test, then optionally publish in dependency order, tag and GitHub
  release).
- More tests: randomized delta histories (8,000 changes) and SQLite
  histories with branches; 8 concurrent writers on two handles of one
  file (no fork, no lost commit; 24–64 contested races retried per run);
  20 conversations at once plus 5 simultaneous sends on one thread.
  Ollama added to the live check (deepseek-r1:1.5b: chat, streaming,
  threads, SQLite, workflow, deadline pass; tool checks skipped, the model
  has no tool support).
- Still waiting on the user: package names (npm `agentbuild` is taken),
  making the repo public, scope of the first release, name check.
- Fixed after that: reasoning models served without a reasoning parser
  (Ollama 0.6, some vLLM/llama.cpp setups) put `<think>…</think>` inside
  the answer text. `openai_chat` now splits a leading `<think>` block into
  a reasoning block, whole and streamed (0006 note; unit tests cut the
  stream at every character; checked live on Ollama).

### 2026-10-13: live check of every feature with real models; fixes it found

- New `tests/live`: a stand-alone program (not in the workspace or CI) that
  runs 23 checks per provider against real APIs: chat, streaming, one and
  several tool calls, tool errors, argument validation, structured output
  (native and tool), approval (approve, reject, edit), `agent.stream`,
  thread memory, SQLite reopen, crash mid-tool with `retry`, agent as a
  tool, hooks, limits, fallback, error classification, an LLM workflow with
  a router, OTLP telemetry, deadlines. Result: 46/46 pass (OpenAI
  `gpt-5-mini`, Gemini `gemini-2.5-flash`).
- Fixed, found by it:
  - `SqliteCheckpointer::connect` / `Storage::connect("sqlite:…")` could not
    be awaited inside `tokio::spawn` (sqlx's `raw_sql` future is not `Send`
    for every lifetime). Migrations now run statement by statement; a test
    opens and uses storage from spawned tasks.
  - Gemini errors (list-wrapped, Google `status` / `details[].reason`) were
    not parsed, and a bad Gemini key was `invalid_request`, so fallback on
    `auth` never fired. Now `auth` (0006 §3 note).
  - The note an edited tool call returns to the model made gpt-5-mini tell
    the user the email went "to the wrong address". Reworded: the reviewer
    approved the call with changes; it is the intended action.
- Earlier the same day: the SQLite crash test and `durable_agent` waited a
  fixed time before killing the run, which failed once on a slow CI disk;
  both now wait until the charge is stored.

### 2026-10-13: demo app `demos/support-chat`, tested with real models

- Rust server (axum 0.8, AgentBuild agent with `lookup_order`,
  `check_stock` and approval-gated `issue_refund`; threads in SQLite;
  OpenAI `gpt-5-mini` and Gemini `gemini-2.5-flash` via its
  OpenAI-compatible endpoint, chosen per request) and a Next.js 16.3.6 /
  React 19.3 UI (approve/reject cards, checkpoint list with rewind,
  "continue the run" after a crash). Not a workspace member; not in CI.
- Tested by hand against the real APIs: order lookup with tool calls
  (OpenAI); refund suspended for approval (Gemini); server restarted while
  waiting, then approved with OpenAI: the approval survived, the
  conversation switched model, the refund was paid once, a second approve
  got `resume_invalid`; server killed mid-run after three tool calls:
  `send` refused with `thread_interrupted`, `retry` (Gemini) answered
  without re-running any tool; in the browser (Playwright): suggestion
  click, approval card, reject with a reason (no payment; the model
  offered a replacement).
- Keys from the user are kept in `~/.agentbuild-keys.env` (outside the
  repo); the server reads them at startup and never logs them. Next.js 16
  writes `AGENTS.md`/`CLAUDE.md` into the app folder by default; disabled
  with `agentRules: false`.

### 2026-10-12: persistence step 2: per-step saving, crash recovery, SQLite (0008 §12)

- Core: new `Persister` trait and `RunOptions::persist`. The run loop saves
  a checkpoint before the first step of every run/resume/retry and after
  each committed step it continues from, and every finished node's output
  and `TaskMemo::save` entry as it happens. All awaited; a failure stops
  the run with the new run error `storage_failed` (snapshot kept, so
  nothing is lost while the database is down). `TaskMemo::save` is new
  (`put` + write-through); `once` uses it.
- Storage: `CheckpointStatus::InProgress`; `Checkpointer::put_pending`
  (fifth method) and `merge_pending`; `ThreadWriter` (the per-thread
  `Persister`); `Durability` (`Step` default for every backend, `Exit`);
  `Storage::connect(url)`; public `delta` module (`set` / `append` /
  `remove`, reducer-independent); `SqliteCheckpointer` behind feature
  `sqlite` (sqlx 0.8.6 without macros, bundled SQLite; 0.9 needs Rust 1.94,
  above our MSRV 1.88). New storage code `storage_incompatible`.
- Agent: threads save through `ThreadWriter`; `send` on an `in_progress`
  head fails with `thread_interrupted`; `retry` continues failed or
  interrupted runs; the tools node saves each finished call with
  `memo.save` and stops before the next model call if storage fails.
  Feature `sqlite` forwards to storage.
- Decisions: deltas are encoded inside the SQL backend (backends still get
  whole checkpoints; custom backends may reuse `delta`) rather than above
  the trait; explicit `retry` instead of auto-recover in `send` (strict
  now); step durability by default even in memory, so behaviour and
  history do not depend on the backend. Versions per call grow (two for a
  plain chat turn); tests updated.
- Tests: 6 core persistence tests (order of saves, crash then retry
  without repeating a memo'd side effect, failing checkpoint, failing task
  write, failing memo save, resume values in the first checkpoint); a
  shared checkpointer contract run on memory and SQLite (file and
  `:memory:`); SQLite: reopen, delta sizes, migrations and newer schemas,
  bad URLs and closed pools, two handles racing on one file; agent: crash
  mid-tool-call recovered from memory and from a reopened SQLite file
  (card charged once), database down at `send` costs no model call,
  unsaved tool results stop the run, `Exit` durability. Total 415 with
  `--all-features`. Example `durable_agent` (CI). Package consumer:
  SQLite thread survives a restart.
- Manual check with real models (keys from the user, kept outside the
  repo): the `chat` example streams from OpenAI `gpt-5-mini` and from
  Gemini `gemini-2.5-flash` through its OpenAI-compatible endpoint.

### 2026-10-12: persistence step 1: threads and in-memory storage (0008 §11)

- New crate `agentbuild-storage` (deps: core, serde, thiserror, tokio
  `sync`/`time`, uuid). `Checkpoint` is plain JSON (state, plus the
  snapshot of a suspended or failed run; status derives from it).
  `Checkpointer` has four boxed-future methods: `load`, compare-and-set
  `commit`, `history` (head's branch), `delete_thread`.
  `InMemoryCheckpointer` implements it.
- `Storage` = checkpointer + `OnBusy` (`Wait` default, `Reject`,
  `Interrupt`) + `busy_timeout` (60 s). Thread operations hold a
  process-local lease; the lease table forgets idle threads.
- `agentbuild-agent`: `AgentBuilder::storage` (default in-memory),
  `agent.thread(id)` → `Thread` with `send`/`send_with`, `approve`,
  `reject`, `decide`/`decide_with`, `retry`, `cancel`, `head`, `state`,
  `messages`, `pending_approvals`, `history`, `rewind`, `delete`. `Reply`
  carries the `AgentResult`, checkpoint id and version.
- Decisions (recorded in 0008 §11): one checkpoint per call in step 1
  (durability `exit`; memory cannot survive a crash anyway); new error
  `thread_suspended` for `send` while approvals are pending (strict now);
  per-run bookkeeping (`agent`, `hooks`) resets each turn, other keys
  carry over; a failed run is saved as a `failure` checkpoint so `retry`
  works later; a caller's cancel token keeps working (the thread uses a
  child token, via the new `RunOptions::cancellation` getter in core).
- Not in step 1: workflow threads, `thread.stream`, `update`, `fork`,
  `on_mismatch`, retention, encryption, the memory store.
- Tests: 13 thread tests (conversation memory, approval across agent
  instances, double approve, reject/edit/invalid decisions, retry without
  repeating tools, send after failure, wait/reject/timeout/interrupt,
  cancel with caller token, rewind incl. to a suspension, delete, failed
  commit, error before the run), 6 storage contract tests + 1 unit test,
  doctests; package consumer step; example `thread_agent` (run in CI).
  Total 395. Mutation check: 16 of 16 caught.
- The dev machine's C: drive filled up (0 bytes free) during the build;
  `cargo clean` freed the project's 17 GB `target/`. Builds without
  incremental caches (`CARGO_INCREMENTAL=0`) use less space.

### 2026-10-12: providers: `anthropic_messages` (0006 §11 step 3)

- `Protocol::AnthropicMessages(AnthropicOptions)`, presets
  `Endpoint::{anthropic, anthropic_compatible}`. Encode, decode and stream
  decoding; blocks map one to one and in order (signed and redacted
  reasoning, citations, `cache_control`, hosted-tool blocks round-trip).
- Checked against Anthropic's current docs before building. Changes from
  research §10.6: reasoning effort → `thinking.adaptive` +
  `output_config.effort` (newer models reject `budget_tokens`; a budget
  still maps to `thinking.enabled`); native output is
  `output_config.format` (no beta); files need no beta. Details in 0006.
- New public API: `ContentBlock::with_extra` / `extras` (e.g. prompt
  caching), `StreamItem::Extras` (fields merged into a block, used for
  streamed citations), `AnthropicOptions` builder.
- Tests: 30 in `tests/anthropic_messages.rs` (golden bodies per mapping
  row, decode, streaming, errors, a decoded answer sent back unchanged) and
  an agent test that runs two parallel tool calls over the Anthropic wire
  against a scripted server (`agentbuild-agent/tests/anthropic_wire.rs`;
  the scripted server now serves several connections). 30 mutation checks:
  29 caught after adding three tests, one equivalent (a redundant call,
  removed).
- `scripts/test-package.sh` now restores `Cargo.lock` (the package
  patches added `[[patch.unused]]` entries). Example `chat` takes
  `AGENTBUILD_PROTOCOL=anthropic`. No live call: no Anthropic key here.

### 2026-10-12: agent slice 9: `agent.stream()`; the agent list is complete

- `AgentStream` / `AgentEvent` (text pieces, tool calls, messages,
  approval needed, other events); `finish()` gives the `AgentResult`.
- 4 tests, three mutation checks (all caught); package consumer streams.
- All of design 0004's agent tasks are built. Remaining gaps recorded in
  0004: per-run app context and nested agents' events in the parent's
  stream/trace (both with the SDKs).

### 2026-10-12: agent slice 8: tool features

- Per-tool `max_concurrency`; safe retries (read-only/idempotent only,
  after retryable errors or timeouts); result truncation with the full
  content in the artifact; `ToolOutput::with_state` / `with_artifact`
  (lists joined in call order, conflicts and reserved keys rejected);
  `AgentBuilder::context` for app services.
- Fixed while building it: a reused (memo-saved) call lost its
  `return_direct` value on retry; memo entries now hold message, state
  writes and direct value.
- 11 tests; eight mutation checks, all caught.

### 2026-10-12: agent slice 7: agents as workflow nodes and as tools

- `AgentNode` (`agent.as_node()`): nested run in a node; approval pauses
  the workflow (agent snapshot in the node memo); workflow retry retries
  the agent without repeating its tool calls.
- `AgentTool` (`agent.as_tool()`): recoverable sub-agent failures are tool
  errors for the calling model, the rest fail the run; `Limits::nesting`
  (8), `ToolContext::depth()`, `Agent::retry_with`.
- 7 tests. Five mutation checks: all caught, after strengthening the retry
  test (a fresh rerun consumed the next scripted reply and looked the
  same; the test now checks the transcript continued the failed turn).
- Package consumer covers both.

### 2026-10-12: agent slice 6: model resilience (0003 §5)

- Model retries moved from the core node policy into the agent's model
  call (inside the hooks): `Retry-After` honored (≤ `max_retry_after`,
  60 s), unsent requests retried at once, fallback models (switch at once
  on rate limit/overload, after retries on other transient errors, and on
  `fallback_on` kinds), provider extras dropped for the fallback, waits past
  the deadline fail with `deadline_exceeded`. Events
  `model_attempt_failed`, `model_fallback`.
- `FakeModel::named(provider, model)` to tell fakes apart.
- 9 tests on the paused clock (exact waits); seven mutation checks, all
  caught.

### 2026-10-12: agent slice 5: remaining limits (0004 §5)

- `OnLimit::{Error, End}`, `Limits::tokens`, `StopReason::Limit`,
  `AgentResult::limit`. Budgets checked before each model call; a spent
  tool-call budget ends the run at the next model call (design table).
- 4 tests (graceful end with a valid transcript, token budget both ways,
  spent tool budget, no false positives); the existing per-run tool
  budget test now also checks the follow-up limit. Four mutation checks:
  all caught. `nesting` moves to the agents-as-tools task.

### 2026-10-12: agent slice 4: hooks (0004 §3)

- `Hook` trait with six optional methods; `HookAction` (state, replace
  transcript, replace message, end); `HookContext`; `ModelNext` /
  `ToolNext` chains (first hook outermost, callable several times);
  `HookError` → `hook_failed`; `StopReason::HookEnd`; `HOOKS` state key.
- `on_start` / `on_end` wrap each `run`, `resume`, `retry`; `on_end` always
  runs, in reverse order.
- 10 tests: exact onion order across a full run, summarizer, cached answer
  without a model call, guardrail (redact or end), recovery from context
  overflow in `wrap_model`, `wrap_tool` changing/short-circuiting/blocking,
  hook state across turns, failing hook (with `on_end`), suspended
  segments, duplicate names. Eight mutation checks: all caught (one
  re-run with a mutation that compiles).

### 2026-10-11: agent slice 3: structured output (0004 §6)

- `AgentBuilder::output(schema)` / `output_with(schema, strategy)`;
  `Limits::output_retries` (2); `StopReason::Output`; error
  `output_invalid`; `FINAL_OUTPUT_TOOL`.
- Tool strategy (default unless the model declares native JSON mode),
  non-object schemas wrapped as `{"value": …}`; other calls in the final
  turn skipped; plain-text answers get a reminder. Native strategy parses
  the answer (code fences removed).
- 9 integration tests, 3 unit tests. Eight mutation checks: seven caught;
  the eighth did not apply (rustfmt had re-wrapped the line) and its
  behaviour is asserted directly by
  `non_object_schemas_are_wrapped_for_the_tool`.

### 2026-10-11: agent slice 2: human approval (0004 §7)

- `Approval` / `ApprovalRule` (glob patterns, allowed decisions, `when`
  predicate, `describe`), `Decision` (approve, edit, reject, respond),
  `ApprovalRequest`; `review` node; `Agent::resume` validates decisions
  before resuming; `AgentResult::{is_suspended, approval_requests,
  snapshot}`.
- A gated call suspends the whole batch before anything runs; edits keep
  the original call in the transcript and tell the model what ran.
- 8 approval tests, 3 unit tests; nine mutation checks: all caught (one
  needed a new assertion that bad decisions are rejected before any run
  starts).
- Example `approval_agent` (a doctor approves, edits or rejects a
  medication order; the snapshot goes through JSON). Package consumer
  covers approval.

### 2026-10-11: observability: OpenTelemetry export and Langfuse (0009)

- User asked for seamless Langfuse support, and to study Langfuse's
  internals. Langfuse ingests standard OTLP (its own ingestion API is
  deprecated), so AgentBuild exports OTLP/HTTP JSON and Langfuse is a preset;
  the same exporter works with any OTel backend. No OpenTelemetry SDK
  dependency.
- Source study (`docs/research/langfuse.md`) corrected the first mapping:
  chat-shaped input/output, Langfuse's usage keys (cached/reasoning tokens
  excluded from input/output), span events not stored (→ event
  observations), trace fields needed on every span, ingestion-version 4
  header.
- Core: `EventObserver` + `RunOptions::observe` (several observers, plus the
  channel), `RunOptions::metadata` (in `run_started`). Agent events now carry
  what a trace needs (`model_request`, full `message_completed`, tool args
  and results).
- New crate `agentbuild-telemetry`: non-blocking observer → bounded queue →
  background task (batching, retries on 429/5xx, no retries on 4xx),
  `flush`/`shutdown`/`stats`, content capture switch, mask (panic-safe),
  truncation. `Langfuse::from_env()` (`LANGFUSE_PUBLIC_KEY`,
  `LANGFUSE_SECRET_KEY`, `LANGFUSE_BASE_URL` or `LANGFUSE_HOST`).
- Tests: 8 unit (mapping, usage, chat, content policy, base64, OTLP shape),
  10 against a fake collector (headers, retries, failures, back-pressure,
  shutdown, full agent trace, masking, failed node, suspend/resume in one
  trace), 3 core observer tests. Nine mutation checks: all caught.
- No live Langfuse check: Docker is not installed here.
  `cargo run -p agentbuild-telemetry --example langfuse` with real keys does
  it.

### 2026-10-11: agent slice 1 (`agentbuild-agent`); glossary

- `docs/glossary.md`: every term, plus naming rules for code, JSON, events,
  error codes and ids (user request).
- New crate `agentbuild-agent`: `Agent` compiles to `model` ⇄ `tools`;
  `Tool`/`tool_fn`; JSON Schema subset validation; concurrent calls bounded
  by a semaphore, results in call order; per-tool timeouts; limits;
  `return_direct`; refusals end the run; transient model errors retried by
  the core node policy; events `message_delta`, `message_completed`,
  `message_aborted`, `tool_call_started`, `tool_call_finished`,
  `transcript_repaired`, `warning`; memo-backed `Agent::retry` (the 0003 E2
  email scenario is a test).
- Core: `TaskMemo::keys()`. Providers: `FakeReply::Message` (scripted
  message; its `finish_reason` is used).
- 17 tool-calling graph tests on the paused clock (parallelism, ordering,
  limits, timeouts, retries, repair, cancellation and retry, refusal).
  Twelve mutation checks: all caught.
- Package test now covers all three crates. Packaging the agent crate needs
  its unpublished dependencies patched to the unpacked packages
  (`--config patch.crates-io...`); the consumer runs an agent loop.

### 2026-10-11: providers part 4: `openai_chat`, endpoints, `ProviderModel`

- Encode (golden tests per mapping row), decode (through the same
  `Assembler` as streaming), chunk decoder (wire-index or id keyed tool
  calls, whole calls in one chunk, refusals, reasoning fields, in-band
  errors), `OpenAiQuirks`, presets, `ProviderModel` (invoke + stream),
  `Model::stream` with a default, `ModelResponse.warnings`,
  `preview_request`.
- Missing public constructors found by the tests: `ContentBlock::{audio,
  file}`, `OutputSpec::{new, with_strategy}`.
- 21 tests (`tests/openai_chat.rs`, scripted server now shared in
  `tests/support/server.rs`). Eight mutation checks: all caught.
- `examples/chat.rs` (manual, not CI) against the local Ollama:
  `deepseek-r1:1.5b` streamed an answer with usage. Its first run found a real
  bug: Ollama's 404 "model not found" carries in-band type `api_error`, which
  was mapped to `server` (transient, would be retried). Generic in-band types
  no longer override the status; regression test uses Ollama's exact body.

### 2026-10-11: providers part 3: HTTP transport; AgentBuild's first user

- The user will build **Axon Health OS** (AI hospital operating system,
  axonhealth.in) on AgentBuild once it is done. The framework stays general;
  healthcare needs (approval gates, audit, no patient data in logs,
  self-hosted models) inform priorities. Suggested a trademark check on
  "Axon" (Axon Enterprise exists).
- `Transport` on reqwest 0.13 (rustls + aws-lc-rs, platform verifier,
  HTTP/2, system proxy), feature `http` (default). Builds on Windows without
  extra tools (~1 min clean).
- Timeouts: connect, first byte, idle, total. Dispatch: `not_sent` for
  connection failures, `sent`, `streaming`. Non-2xx → `classify_http`.
  URL-free, redacted messages.
- 7 tests against a scripted local TCP server. Found while testing: on
  Windows a refused localhost connection is retried until the connect timeout,
  so it surfaces as `timeout` (still `not_sent`); the test accepts both.
  Six mutation checks: all caught. CI also tests the crate without `http`.

### 2026-10-11: providers part 2: SSE parser and stream assembler

- `SseParser` (WHATWG): `
`, `
`, `
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
