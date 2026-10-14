# 0008: Persistence, threads and memory

Status: **Accepted**; §10 steps 1–3 built (§11, §12, §13). Builds on [0005](0005-core-runtime-extensions.md)
§5 (snapshots, saved task writes, resume) and gives developers the easy
interactive API sketched in [0007](0007-public-api.md). Research input:
[langgraph](../research/langgraph.md) §5 (checkpointing), §6 (interrupts) and §8 (store).

## Goals

1. **One parameter turns it on.** `storage="sqlite:///agent.db"` gives
   conversations, crash recovery, approval/resume, history and long-term memory.
   No extra setup code.
2. **Interactive by default.** Chatting, approving, rewinding and inspecting are
   short method calls on a `Thread` object.
3. **Same data from Python and TypeScript.** Everything stored is plain JSON
   (0005 §5.2). A thread started in Python can be continued in TypeScript.
4. **Cheap at scale.** Storage grows with *new* data, not with history length.
   LangGraph rewrites a whole changed channel each step, which is O(n²) for long
   chats ([research](../research/langgraph.md) §5.2, §10).
5. **Correct under crashes and concurrency.** Clear guarantees, documented
   limits, no silent forks.

---

## 1. Concepts

| Concept | What it is | Lifetime |
|---|---|---|
| **Thread** | One conversation or work item: a sequence of runs sharing state | Until deleted or expired |
| **Checkpoint** | Durable record of a committed step (0005 §5) | Per retention policy |
| **Short-term memory** | The thread's state, including `messages` | Lives in checkpoints |
| **Long-term memory** | Facts that outlive a thread (user preferences, learned info), in a namespaced **memory store** | Until deleted or its TTL ends |
| **Storage** | One object holding a checkpointer and a memory store, usually the same database | App lifetime |

## 2. The developer API

### 2.1 Turning it on

```python
agent = Agent(model=…, tools=[…], storage="sqlite:///agent.db")       # file, zero setup
agent = Agent(model=…, tools=[…], storage="postgres://user@host/db")  # production
agent = Agent(model=…, tools=[…])                                      # in-memory (lost on exit)
```

```ts
const agent = new Agent({ model, tools, storage: "sqlite://./agent.db" });
```

Workflows take the same parameter: `wf.compile(storage=...)`. Tables are
created automatically on first use (§6.4).

### 2.2 Threads: the interactive surface

```python
chat = agent.thread("user-42")                 # created on first use

reply = await chat.send("My order A-1001 arrived broken")
print(reply.text)

reply = await chat.send("Can you refund it?")  # remembers the conversation
if reply.needs_approval:                       # human-in-the-loop (0004 §7)
    for req in reply.approvals:
        print(req.tool, req.args, req.description)
    reply = await chat.approve()               # approve all pending
    # or: await chat.reject("Too expensive"), chat.edit(req.id, args={...}),
    #     chat.respond(req.id, "Customer already refunded")

async for event in chat.stream("Anything else?"):     # streaming send
    if event.type == "message_delta":
        print(event.text, end="")
```

| Method | Does |
|---|---|
| `send(input)` / `stream(input)` | Run one turn on the thread; returns `Reply` / streams events |
| `approve()`, `reject(msg?)`, `edit(id, args)`, `respond(id, msg)` | Answer pending approvals, then continue automatically |
| `resume(values)` | Generic resume for custom `Suspend` payloads (workflows) |
| `cancel()` | Cancel the active run on this thread |
| `state()` | Current state (messages, output, custom keys) |
| `history(limit=)` | Checkpoints, newest first: step, time, source, summary |
| `rewind(checkpoint_id)` | Make an older checkpoint the head (keeps the newer branch, §5.3) |
| `fork(checkpoint_id, new_thread=)` | Copy history up to a checkpoint into a new thread |
| `update(values)` | Edit state by hand (recorded as a `manual` checkpoint, §5.4) |
| `delete()` | Delete the thread and its checkpoints |

`Reply` fields: `text`, `output`, `status` (`completed` | `suspended`),
`needs_approval`, `approvals`, `usage`, `stop_reason`, `checkpoint_id`.

The same methods exist in TypeScript (`chat.send`, `chat.approve`, …), with
`AbortSignal` support on `send` and `stream`.

**Pending approvals survive restarts.** A process can die after `send` returns
`needs_approval`. Another process calls `agent.thread("user-42").approve()`
and continues from the stored snapshot.

### 2.3 Long-term memory

```python
from agentbuild import Memory

agent = Agent(
    model=…, tools=[…], storage="postgres://…",
    memory=Memory(
        namespace=lambda ctx: ("users", ctx.context.user_id),   # per-user memory
        recall=5,                     # inject up to 5 relevant memories before each model call
        tools=True,                   # give the model `remember` / `forget` tools
        embed="openai:text-embedding-…",   # optional: semantic search; else keyword
    ),
)
```

Direct access from app code, tools and hooks:

```python
mem = agent.storage.memory
await mem.put(("users", "42"), "diet", {"value": "vegetarian"}, ttl="90d")
item = await mem.get(("users", "42"), "diet")
hits = await mem.search(("users", "42"), query="food preferences", limit=5)
await mem.delete(("users", "42"), "diet")
```

Inside a tool: `ctx.memory` (same API, already scoped to the agent's namespace).

---

## 3. How it fits the runtime

```mermaid
sequenceDiagram
    participant App
    participant Thread
    participant Run as Run loop (Rust)
    participant CP as Checkpointer
    participant DB as Database

    App->>Thread: send("hi")
    Thread->>CP: load head(thread)
    CP->>DB: SELECT head + deltas since keyframe
    CP-->>Thread: snapshot (state, frontier, version v7)
    Thread->>Run: run(input appended to messages)
    loop each superstep
        Run->>Run: execute nodes
        Run->>CP: task memo / finished-task writes (immediately)
        Run->>CP: commit step (delta, expected version v_n)
        CP->>DB: INSERT checkpoint v_n+1 WHERE head = v_n
    end
    Run-->>Thread: outcome (completed / suspended)
    Thread-->>App: Reply
```

## 4. Storage format

### 4.1 Delta checkpoints with keyframes

Each committed step stores **only what changed**, as reducer operations
(0005 §3): "append these 2 messages", "set `category`". Reducers are
deterministic and run in Rust, so replaying deltas rebuilds the state exactly.

- A **keyframe** (full state) is written every N checkpoints (default 25), and
  whenever the delta chain since the last keyframe exceeds the state's size.
- Loading a thread = the latest keyframe + the deltas after it. That's a
  bounded amount of work.
- A 500-turn chat stores each message **once**, plus occasional keyframes. Not
  500 copies of a growing list.

### 4.2 Tables (SQL backends)

```
threads(thread_id PK, created_at, updated_at, head_checkpoint_id, head_version,
        metadata JSON, expires_at)
checkpoints(checkpoint_id PK (UUIDv7), thread_id, parent_id, version, step,
        source ENUM(input, step, suspend, failure, manual, fork),
        kind ENUM(delta, keyframe), body JSON (delta ops or full snapshot),
        frontier JSON, join_progress JSON, suspended JSON, workflow_fingerprint,
        created_at)
pending_writes(checkpoint_id, task_id, key, value JSON, created_at)   -- task memo + finished-task output
memory(namespace TEXT, key TEXT, value JSON, created_at, updated_at, expires_at,
        embedding VECTOR NULL, PRIMARY KEY(namespace, key))
schema_migrations(version, applied_at)
```

Namespaces are stored as a joined path (`users/42`) so prefix search is an
index range scan. JSON is the only value format: **no pickle**, no
language-specific class names (LangGraph's msgpack embeds Python class paths,
[research](../research/langgraph.md) §5.6).

### 4.3 Backends

| Backend | URL | Use | Vector search |
|---|---|---|---|
| In-memory | (default) | Tests, scripts | brute force |
| SQLite | `sqlite:///path.db` | Local apps, single process, edge | `sqlite-vec` (optional feature) |
| Postgres | `postgres://…` | Production, multi-process | `pgvector` (optional feature) |
| Custom | `Storage(checkpointer=…, memory=…)` | Anything else | implementer's choice |

All built-in backends are implemented **once in Rust** (async, `sqlx`), so
Python and TypeScript share the same code, behaviour and on-disk format. Custom
backends implement two small interfaces (§7), in Rust or in the SDK language.
Redis/Valkey and DynamoDB are later candidates, driven by demand.

---

## 5. Guarantees and semantics

### 5.1 Durability modes

| Mode | Behaviour | Default for |
|---|---|---|
| `step` | Each step's checkpoint is written **before** the next step starts | **default** |
| `async` | Checkpoint writes overlap the next step; a crash may lose the last step (the run resumes from the previous one) | high-throughput workflows |
| `exit` | Only the final, suspended or failed state is written | short runs where only results matter |

Task memo writes (finished tool calls, 0005 §1) are always written immediately
in `step` mode. That is what lets a resumed run skip tools that already ran.

### 5.2 Concurrency on a thread

Each thread has a **head version**. A commit succeeds only if the head is still
the version the run started from (compare-and-set). A second run on a busy
thread therefore cannot silently fork it:

| `on_busy` | Second `send` while a run is active |
|---|---|
| `"wait"` (default) | Waits for the active run to finish, then runs (queued, FIFO). Waits at most `busy_timeout` (default 60 s), then `ThreadBusyError` |
| `"reject"` | Fails fast with `ThreadBusyError` (`thread_busy`) |
| `"interrupt"` | Cancels the active run (outcome `cancelled`), then runs |

Across processes, the same compare-and-set keeps it correct. `wait` uses an
advisory lock on Postgres, an in-process queue plus head-version polling with
backoff on SQLite, and a plain queue in memory. Queuing is the default because
chat UIs expect a second message to wait its turn, not to fail.

### 5.3 History, rewind and fork

- Checkpoints form a tree via `parent_id`. `rewind(id)` moves the thread head
  to an older checkpoint with source `fork`. The abandoned branch stays in
  `history(all_branches=True)` until pruned.
- Resuming from an old checkpoint **re-runs** the next steps; it does not replay
  recorded results. Use it to retry with a different input or after editing
  state. (LangGraph behaves the same, [research](../research/langgraph.md) §5.5.)

### 5.4 Manual edits

`thread.update(values)` applies `values` through the normal reducers and writes
a `manual` checkpoint. The frontier is left unchanged. A pending approval stays
pending, and the next `send` or `resume` sees the edited state. This
replaces LangGraph's `as_node` mechanism, which re-runs a node's writers and has
special `INPUT`/`END`/`__copy__` cases ([research](../research/langgraph.md) §5.5).

### 5.5 Crash recovery

On restart, a thread whose head checkpoint has a non-empty frontier and no
outcome was interrupted mid-run (status `in_progress`). `thread.retry()`
re-runs the interrupted step: finished tasks reuse their saved output,
unfinished ones run again with their memo. `send` on such a thread fails
with `thread_interrupted` instead of starting a new turn on top of an
unfinished one (strict now; an automatic `recover` on `send` can be added
later without breaking anyone, rule 4).

**Honest limit.** If the process dies *after* a tool's side effect but
*before* its memo is written, the tool runs again on recovery. For tools where
that matters, `ctx.idempotency_key` gives a deterministic key
(hash of run, step, task and `call_id`) to pass to external APIs that support
idempotency keys (payments, email providers), or to check in your own database.

---

## 6. Operations

### 6.1 Retention

`storage.retention(keep_checkpoints=50, keep_branches=False, thread_ttl="30d")`
prunes old checkpoints, never deleting the latest keyframe chain. Expired
threads and memory items are removed by a background sweep, or by calling
`storage.prune()` (for serverless deployments).

### 6.2 Privacy

- `thread.delete()` and `memory.delete_namespace(ns)` remove all rows for the
  thread or namespace, including pending writes and embeddings.
- **Run context is never stored.** `context=` holds runtime dependencies (DB
  clients, credentials) and is not part of state.
- Optional encryption at rest:
  `Storage(…, encrypt=Key.from_env("AGENTBUILD_KEY"))` encrypts `body`,
  `value` and pending writes (AES-GCM, key id stored per row for rotation).

### 6.3 Schema evolution

- The **workflow fingerprint** (0005 §5.2) is stored per checkpoint. Loading a
  thread whose fingerprint differs from the current agent or workflow follows
  `on_mismatch`:
  - `"error"` (default): `WorkflowMismatchError`;
  - `"migrate"`: run user-registered migration functions for state keys, then
    continue;
  - `"continue"`: accept the risk.
- Agents get a stable fingerprint: changing tools or instructions does **not**
  change it; changing the graph shape does.

### 6.4 Database migrations

Built-in backends version their tables (`schema_migrations`). The default is to
migrate automatically on first use. `storage.migrate(dry_run=True)` and the CLI
`agentbuild storage migrate` exist for teams that run migrations in CI.

---

## 7. Extension interfaces

```rust
trait Checkpointer: Send + Sync {
    async fn load(&self, thread: &ThreadId, at: Option<CheckpointId>) -> Result<Option<Snapshot>>;
    async fn commit(&self, thread: &ThreadId, expected: Version, cp: CheckpointRecord) -> Result<Version>; // CAS
    async fn put_pending(&self, cp: &CheckpointId, task: &TaskId, key: &str, value: Value) -> Result<()>;
    async fn history(&self, thread: &ThreadId, q: HistoryQuery) -> Result<Vec<CheckpointMeta>>;
    async fn delete_thread(&self, thread: &ThreadId) -> Result<()>;
}

trait MemoryStore: Send + Sync {
    async fn put(&self, ns: &Namespace, key: &str, value: Value, ttl: Option<Duration>) -> Result<()>;
    async fn get(&self, ns: &Namespace, key: &str) -> Result<Option<Item>>;
    async fn search(&self, ns_prefix: &Namespace, q: SearchQuery) -> Result<Vec<Item>>;
    async fn delete(&self, ns: &Namespace, key: &str) -> Result<()>;
}
```

Five checkpointer methods and four store methods. LangGraph's saver has
about 12 methods with sync and async twins
([research](../research/langgraph.md) §5.2). Backends receive whole
checkpoints. A custom backend may store them as they are; the built-in SQL
backends encode deltas and keyframes internally with the public
`agentbuild_storage::delta` helpers (as built: §12). Retention and
encryption are planned above the interface. Python and TS can implement the
same interfaces (async methods); the bridge adapts them.

## 8. Errors (added to 0003 §9)

| Code | Category | When |
|---|---|---|
| `thread_busy` | policy | Another run holds the thread and `on_busy="reject"`, or `busy_timeout` elapsed with `on_busy="wait"` |
| `storage_unavailable` | transient | Database connection or timeout failure; retried per policy, then fails |
| `storage_conflict` | developer | Compare-and-set lost for a reason other than `thread_busy` (e.g. external edit) |
| `checkpoint_not_found` | developer | `rewind`/`fork` to an unknown id |
| `workflow_mismatch` | developer | §6.3 |
| `storage_corrupt` | internal | A delta chain cannot be replayed, or a row cannot be decoded |
| `storage_incompatible` | developer | The database schema is newer than this version supports |
| `thread_suspended` | developer | `send` on a thread that waits for approval: answer it first (`approve`, `reject`, `decide`) |
| `thread_interrupted` | developer | `send` on a thread whose last run did not finish: continue it with `retry` first (§5.5) |
| `storage_failed` | transient | (core run error) the run's progress could not be saved during the run; the cause's storage code is in the message and `NodeError` kind |
| `invalid_value` | developer | A memory item the store does not accept: root namespace, bad key, value over 1 MiB (P11, §13) |

A storage failure **during** a run fails the step (nothing committed) and the
run reports the error with its in-memory snapshot attached, so no work is lost
even if the database was down.

## 9. Edge cases

| # | Case | Behaviour |
|---|---|---|
| P1 | Two browser tabs call `send` on the same thread | Second waits its turn (default) and then runs on the updated state; `on_busy="reject"` fails it instead |
| P2 | Process killed while waiting for approval | Nothing lost: the suspension was committed; any process can `approve()` later |
| P3 | Process killed mid-tool-call | `recover()` re-runs the step; finished calls are skipped; the in-flight call runs again (§5.5 idempotency key) |
| P4 | Approve twice (double click) | The first `approve()` consumes the suspension; the second gets `ResumeError` ("nothing pending") |
| P5 | Database down at `send` | `storage_unavailable` before any model call (no cost incurred) |
| P6 | Database fails at step commit | Step not committed; `RunError` carries the in-memory snapshot to retry later |
| P7 | Thread created in Python, continued in TS | Works: JSON format, same fingerprint rules |
| P8 | 10,000-message thread | Load = keyframe + ≤ 25 deltas; per-step writes stay proportional to new messages |
| P9 | Agent deployed with a new tool while threads are suspended | Fingerprint unchanged (tools excluded); pending calls are re-validated against current tools on resume (0004 A9) |
| P10 | Memory search with no embedding model configured | Keyword search (SQLite FTS5 / Postgres full-text); `warning` once |
| P11 | `memory` item too large (> 1 MB default) | `invalid_value`; long content belongs in files or blobs, not memory |
| P12 | Clock skew between processes | Ordering uses UUIDv7 plus versions, never wall-clock comparisons |

## 10. Implementation order

1. In-memory checkpointer + `Thread` API (`send`, `approve`, `history`) on top
   of 0005 snapshots.
2. SQLite backend (delta + keyframe format, compare-and-set, migrations).
3. Memory store (keyword search) + agent `Memory` hook and tools.
4. Postgres backend, `on_busy="wait"` across processes.
5. Vector search (`sqlite-vec`, `pgvector`), retention, encryption.

## 11. Step 1 as built

Crate `agentbuild-storage` (depends on `agentbuild-core` only) holds the
storage contract and the in-memory backend; `agentbuild-agent` adds the
`Thread` API on top. SQL backends will be features of the storage crate.

**Checkpoints.** A `Checkpoint` is plain JSON: `id` (UUIDv7), `thread_id`,
`parent_id`, `version` (1, 2, 3… per thread), `source`, `run_id`, `step`,
`state`, and `snapshot` when the run is resumable (suspended or failed).
Its status follows from the snapshot: `suspended` if it has suspended nodes,
`failed` if it carries an error, otherwise `completed`.

**Checkpointer.** Four methods, all returning boxed futures so the trait
works behind `Arc<dyn Checkpointer>`:

| Method | Contract |
|---|---|
| `load(thread, at)` | The head (`at = None`) or one checkpoint of that thread; `None` if missing |
| `commit(checkpoint, expected)` | Compare-and-set: succeeds only if the head version is `expected` (0 for a new thread) and `checkpoint.version == expected + 1`; the checkpoint becomes the head. Otherwise `storage_conflict` |
| `history(thread, limit)` | The head's branch, newest first, as `CheckpointMeta` (no state) |
| `delete_thread(thread)` | Removes the thread and all its checkpoints; `true` if it existed |

`put_pending` (§7) arrived with per-step commits in step 2 (§12).

**Durability in step 1 is `exit`** (§5.1): one checkpoint per call, written
when the run completes, suspends or fails. The in-memory backend cannot
survive a crash anyway; per-step commits and crash recovery come with
SQLite.

**Storage and busy threads.** `Storage` bundles a checkpointer with the
`on_busy` policy (`Wait` default, `Reject`, `Interrupt`) and `busy_timeout`
(60 s). Each thread operation holds a process-local lease for the thread;
`Interrupt` cancels the active run first. The lease table drops a thread's
entry when no one holds or waits for it. Compare-and-set still catches
writers in other processes.

**Agent threads.** Every agent has a `Storage` (default: a fresh in-memory
one; `AgentBuilder::storage` shares one). `agent.thread(id)` returns a
`Thread`:

| Method | Does |
|---|---|
| `send(input)`, `send_with(input, options)` | Starts from the head state, appends the input messages, runs, commits. Per-run bookkeeping (`agent`, `hooks` keys) is reset; other keys carry over. Fails with `thread_suspended` while approvals are pending |
| `approve()`, `reject(msg)` | One decision for every pending request |
| `decide(decisions)`, `decide_with` | One `Decision` per `call_id` (edit, respond, mixed) |
| `retry()` | Continues a failed head from its snapshot (finished tool calls are not run again) |
| `cancel()` | Cancels this thread's active run in this process |
| `state()`, `messages()`, `head()`, `pending_approvals()` | Read the head |
| `history(limit)` | Checkpoints of the current branch, newest first |
| `rewind(checkpoint_id)` | New head (source `fork`) copying an older checkpoint |
| `delete()` | Deletes the thread |

`send` and `decide` return a `Reply`: the `AgentResult` plus the new
checkpoint's id and version. A failed run is committed as a `failure`
checkpoint (so `retry` works later) and its `AgentError` is returned. An
error before the run starts (hook failure, invalid decisions) commits
nothing. If the commit itself fails, the storage error is returned with the
run's error attached when there was one.

`send_with` keeps a caller's cancel token: the thread runs on a child of it,
so both `thread.cancel()` and the caller's token stop the run.

Not in step 1: workflow threads (`resume(values)`), `stream`, `update`,
`fork`, `on_mismatch`, retention, encryption, the memory store.

## 12. Step 2 as built

Durability `step` (§5.1), crash recovery (§5.5) and the SQLite backend
(§4). Not yet: durability `async`, history across branches, `fork`,
`update`, `on_mismatch`, retention, encryption (later steps).

**Core: `Persister`.** `agentbuild-core` gained one trait so the engine can
save a run while it executes without knowing about storage.
`RunOptions::persist(Arc<dyn Persister>)` turns it on:

| Method | Called |
|---|---|
| `save_checkpoint(snapshot)` | Before the first step of every `run`, `resume` and `retry` (resume values and saved work in `pending_writes`), and after each committed step the run continues from. Not for the final outcome: the caller saves that |
| `save_task(step, node, write)` | When a node finishes (its output), and for every `TaskMemo::save` entry, before the run goes on |

Every call is awaited. An error stops the run with the new run error
`storage_failed`; its snapshot keeps everything done so far, so nothing is
lost while the database is down (P5, P6). `TaskMemo::save(key, value)` is
`put` plus the write-through (and `once` uses it); a failed save returns a
`NodeError` of kind `storage_failed` so the node stops doing side effects.
The agent's tools node saves each finished call this way. If storage fails
it finishes the step's calls, then fails the run before the next model call,
with every result in the snapshot.

**Checkpoint status `in_progress`.** A checkpoint whose snapshot has no
suspended nodes and no error is a point a run continues from. After a
crash, the head stays `in_progress`.

**`Checkpointer::put_pending(thread, checkpoint, node, write)`.** The fifth
method (§7). It merges a task write into that checkpoint's
`pending_writes[node]` (`merge_pending`: memo entries added, `output` and
`resume` replaced). Every later `load` returns the merged snapshot. Only
checkpoints with a snapshot accept writes (`checkpoint_not_found`
otherwise).

**`ThreadWriter`** (in `agentbuild-storage`) is the `Persister` for one
thread. It holds the head it last wrote: the first checkpoint of a call is
source `input`, later ones `step`, each with `expected` = the previous
version (compare-and-set). Task writes go to its latest checkpoint.
`commit` then saves the outcome (`step`, `suspend` or `failure`). Custom
code (workflow threads, SDK bridges) uses the same writer.

**`Durability`** on `Storage`: `Step` (default, for every backend so
behaviour does not depend on the backend) or `Exit` (one checkpoint per
call, as in step 1). A simple chat turn writes two checkpoints (`input`,
final). A turn with one tool call writes four: `input`, after the model,
after the tools, final.

**Threads.** `send` fails with `thread_interrupted` on an `in_progress`
head; `retry` continues a `failed` or `in_progress` head. Finished tool
calls are not repeated, including calls finished just before a crash. A
call interrupted mid-flight runs again (§5.5's honest limit).

**Deltas** (`agentbuild_storage::delta`, public so custom backends can
reuse it). `diff(old, new)` gives `set`, `append` (a list that only grew:
just the new elements) and `remove` operations, in key order; `apply`
replays them (`storage_corrupt` if an append hits a non-list). It works on
plain state, so it needs no reducers and stays correct for any reducer.

**SQLite** (feature `sqlite` of `agentbuild-storage`, forwarded by
`agentbuild-agent`; `sqlx` 0.8 without macros, SQLite bundled).
`SqliteCheckpointer::connect(url)` / `open(path)`, or
`Storage::connect("sqlite://file.db")` (`"memory:"` and `sqlite::memory:`
also work). WAL journal, 5 s busy timeout, up to 8 connections (one for
in-memory).

- Tables, prefixed `agentbuild_` so they can live in an app's database:
  `threads(thread_id, head_id, head_version, created_at_ms, updated_at_ms)`,
  `checkpoints(id, thread_id, parent_id, version, source, status, run_id,
  step, kind, depth, body, snapshot, created_at_ms)`,
  `pending_writes(seq, checkpoint_id, node, write, created_at_ms)`,
  `schema_migrations(version, applied_at_ms)`.
- **Encoding.** `kind` is `keyframe` (body = full state) or `delta` (body =
  operations against the parent). A delta is written when the parent is
  fewer than `keyframe_interval` (default 25) deltas from its keyframe
  *and* the delta is smaller than the state. `depth` counts deltas since
  the keyframe. Loading walks the parent chain back to the keyframe in
  one recursive query and replays. The snapshot is stored without its
  state when it equals the checkpoint's (it always does for threads).
  The writer caches the last state per thread (up to 256 threads), so a
  commit does not read its parent back.
- **Compare-and-set.** One `BEGIN IMMEDIATE` transaction: a conditional
  `UPDATE threads … WHERE head_version = expected` (or an insert for a new
  thread) must change exactly one row, then the checkpoint row is
  inserted. Two handles on one file cannot fork a thread (tested); leases
  stay per process.
- **Migrations** run automatically on connect, in one transaction, and are
  recorded. A database at a newer schema version is refused with
  `storage_incompatible`.
- **Errors.** Connection, I/O, busy and closed-pool errors are
  `storage_unavailable`; undecodable rows are `storage_corrupt`.

`history`, `delete_thread` and `put_pending` are single queries (recursive
CTE along `parent_id`; three deletes in one transaction; `INSERT … SELECT …
WHERE EXISTS`). Measured in the tests: a 40-turn chat with a keyframe every
10 checkpoints stores each message about once per delta plus four
keyframes, under a third of what full copies would need. That is a test
assertion, not a benchmark.

## 13. Step 3 as built

Long-term memory (§2.3): a memory store in `agentbuild-storage`, and
`Memory` for agents in `agentbuild-agent`. Not yet: vector search, a
background sweep of expired items, encryption (step 5).

**Namespaces.** `Namespace` is a list of segments (`["users", "42"]`,
written `users/42`). Segments are non-empty, without `/` or control
characters (`invalid_value` otherwise). The root (no segments) holds no
items; it is the prefix of every namespace. `contains` is a segment-wise
prefix test, so `users/ada` does not contain `users/adam`.

**`MemoryStore`.** Six methods (§7's four plus two for privacy and
retention):

| Method | Contract |
|---|---|
| `put(ns, key, value, ttl)` | Insert or replace. `created_at` is kept on replace (reset when the old item had expired). `ttl = None` keeps it until deleted. Refuses the root, keys outside 1–256 bytes and values over 1 MiB of JSON (`invalid_value`, P11) |
| `get(ns, key)` | The item, unless missing or expired |
| `search(prefix, query)` | Live items in `prefix` and below, ranked (below); `limit` default 10 |
| `delete(ns, key)` | `true` if a live item was removed |
| `delete_namespace(prefix)` | Everything in `prefix` and below (§6.2: everything about one user or patient); returns the count; refuses the root |
| `prune_expired()` | Removes expired items (expired items are already invisible) |

`ScopedMemory` is a store plus one namespace (what tools see). `Storage`
holds a memory store next to the checkpointer (`storage.memory()`;
in-memory by default, `with_memory` to replace); `Storage::connect` with a
SQLite URL puts both in the same database.

**Keyword search, the same on every backend.** An item's words are its key
and the strings and numbers in its value (not field names), lower-cased,
split on anything that is not a letter or digit (any script). A query's
words are the same minus common English stop words ("what", "is", "my",
…), at most 32. Each query word scores 1 if it is one of the item's
words, 0.5 if it starts one (query words of three letters or more), and
the item's score is the mean. Only items scoring above 0 are returned,
best first, then newest, then namespace path and key in byte order. A
query without words (none given, or only stop words) returns the newest
items. Every backend ranks in Rust with the same function, so results do
not depend on the backend; SQLite only narrows the rows first (below).
P10's FTS5 / full-text indexes are deferred until a benchmark shows the
need: per-user memory is usually small.

**SQLite** (`SqliteMemoryStore`, from `SqliteCheckpointer::memory_store`
or `SqliteMemoryStore::connect`). Migration 2 adds
`agentbuild_memory(namespace, key, value, search_text, created_at_ms,
updated_at_ms, expires_at_ms, PRIMARY KEY (namespace, key))` and an index
on `expires_at_ms`. `search_text` stores the words as ` w1 w2 … `; a
search selects the live rows in the prefix that contain ` <word>` for any
query word (`instr`), then ranks them. Without words it orders and limits
in SQL. A version-1 database is upgraded on connect.

**Agents: `Memory`.** `AgentBuilder::memory(Memory)` uses the agent
storage's memory store. Each run works in one namespace:

| Scope | Namespace |
|---|---|
| `Memory::by_state(key, prefix)` | `prefix/<state[key]>` (a string or number) |
| `Memory::shared(ns)` | `ns` for every run |
| `Memory::resolve(rule)` | `rule(&state)` |

A run whose scope cannot be resolved (missing key, a value that is not a
string or number, an invalid segment, or the rule's `Err`) fails with
`invalid_state` before the model is called. It never falls back to a
shared namespace, so one person's facts cannot reach another's run.

The scope comes from the run's state, so `AgentInput` gained values:
`AgentInput::from("hi").with("user_id", "42")` sets state keys for the
run. In a thread they are written into the thread's state and carry over
to later turns. The agent's own keys (`messages`, `agent`, `hooks`) are
refused with the new code `invalid_input`.

- **Recall.** Before each model call the model node searches the run's
  namespace with the text of the latest user message, takes up to
  `recall` items (default 5), fills the remaining places with the newest
  items, and adds them to the request as a system message (id
  `msg_memory`) after the instructions. It is not stored in the
  transcript. It tells the model the facts may be out of date and are
  information, not instructions. A `memory_recalled` event carries the
  namespace and the keys, never the values (they may be personal data).
  `recall(0)` turns it off.
- **Tools** (default on; `tools(false)` turns them off): `remember(key,
  value)` (idempotent; the value is stored as a JSON string, with the
  optional `Memory::ttl`), `forget(key)` (idempotent, destructive) and
  `search_memory(query, limit ≤ 20)` (read-only; returns
  `[{key, value}]`). Their names are reserved like any tool name: an app
  tool with the same name fails the build. Bad keys or values become tool
  errors the model can fix; an unreachable store is a retryable tool
  error. Approval rules apply to them like to any tool.
- **`ToolContext::memory()`** gives any tool the run's `ScopedMemory`
  (`None` without `Memory`).

The fingerprint does not change: the memory tools are tools, and recall
is part of the model node.
