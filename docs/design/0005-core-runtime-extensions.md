# 0005: Core runtime extensions

Status: **implemented** (§1–§6: NodeContext with the task memo; node
outcomes with suspend/resume; reducers; policies; saved task writes and
snapshots on failure; events). Dynamic fan-out (§2) stays deferred. This document changes
[0001](0001-execution-model.md); every change is listed in §9.
It provides what [0002](0002-messages-and-tools.md),
[0003](0003-error-model.md) and [0004](0004-agent-loop.md) need. Research
input: [langgraph](../research/langgraph.md) (engine, channels, checkpoints)
and [langchain-core](../research/langchain-core.md) (event protocol).

The core stays small. Each extension below exists because an agent feature
cannot be built correctly without it.

| Extension | Needed for |
|---|---|
| §1 Node context | cancellation, deadlines, events, resume values |
| §2 Node outcomes (`Update` / `Command` / `Suspend`) | human-in-the-loop, node-chosen next steps |
| §3 Reducers | message history without O(history) copies across the language boundary |
| §4 Policies (timeout, retry, deadline) | identical behaviour in Python and TypeScript |
| §5 Snapshots and per-task saved writes | resume, crash recovery, no duplicate side effects |
| §6 Events | streaming and observability |

---

## 1. Node context

`Node::run` gains an explicit context argument. Nothing is injected by
parameter name. LangGraph passes internals through an untyped config dict and
contextvars ([research](../research/langgraph.md)).

```rust
trait Node: Send + Sync + 'static {
    fn run(&self, ctx: NodeContext, state: Arc<State>) -> BoxFuture<'static, Result<NodeOutput, NodeError>>;
}

struct NodeContext {
    run_id: RunId,            // UUIDv7 (time-ordered)
    step: usize,
    task_id: TaskId,          // deterministic: hash(run_id, step, node, index)
    attempt: u32,             // 1-based; > 1 when a retry policy re-runs the node
    resume: Option<Value>,    // set only when this node is resumed after Suspend
    cancel: CancellationToken,
    deadline: Option<Instant>,
    events: EventSink,        // emit(custom event); never blocks the node (§6.3)
    memo: TaskMemo,           // task-scoped saved results, survive retry/resume (§5.1)
}
```

`TaskMemo` has `get(key)` and `put(key, value)`. A `put` is saved immediately
in the run's pending writes for this task, not at step commit. When the same
task executes again (retry, or resume of a failed or suspended step), `get`
returns the saved values. The tools node uses it to record each finished tool
call by `call_id`, so a re-executed tools step skips calls that already
completed. User nodes can use it for their own idempotency.

The SDKs expose the same fields as `ctx` (0002 §3.1).

## 2. Node outcomes

```rust
enum NodeOutput {
    Update(Update),                                       // today's behaviour
    Command { update: Update, goto: Vec<String> },        // node picks next nodes
    Suspend { update: Update, payload: Value },           // pause the run
}
```

- **`Command`.** `goto` targets must be declared when the node is added
  (`add_node(name, node).may_goto([...])`) and are validated at build time. An
  undeclared target at runtime is an error (`unknown_route`), **not** a silent
  drop, which is LangGraph's behaviour. A node with `may_goto` may not also
  have a router (strict rule, relaxable later).
- **`Suspend`.** The step commits normally, including `update` and the other
  nodes in the step. The run ends with outcome `suspended`, the payload, and a
  snapshot (§5). On resume, **only** the suspended node is invoked again,
  with `ctx.resume = value`. By contract, a node does no side effects before
  returning `Suspend`. Suspension is a return value, so a node is never left
  half-executed.
- **Several nodes suspend in one step.** All payloads are returned together,
  keyed by `task_id`. Resume supplies a value per task, and missing values
  are rejected (`resume_invalid`).

**Dynamic fan-out** (LangGraph's `Send`: run a node once per item of a runtime
list) is **deferred**. The agent runs parallel tool calls inside the tools node
(0004 §4.3). When it is added, the frontier becomes a list of *tasks* (node +
input + deterministic id) instead of node ids. `TaskId` is already
deterministic so that the change stays compatible.

## 3. Reducers

Each state key may declare how concurrent writes and successive writes combine:

| Reducer | Semantics | Conflict |
|---|---|---|
| `replace` (default) | Write replaces the value | Two writes in one step → `update_conflict` (unchanged from 0001) |
| `append` | Value is a list; writes are lists appended in node declaration order | none |
| `upsert_by_id` | Value is a list of objects with `id`. A write with a new id appends; an existing id replaces in place; `{"id": …, "$remove": true}` removes | Two writes to the same id in one step → `update_conflict` |

- Declared on the workflow: `builder.reducer("messages", Reducer::UpsertById)`.
  Undeclared keys use `replace`.
- `upsert_by_id` is what `messages` uses. Appending is O(new messages), and
  transcript repair and HITL edits replace single messages without rewriting
  history. Compare LangChain's remove-all-then-re-add
  ([research](../research/langchain-agents.md) §7.3).
- **Escape hatch:** an update value wrapped as `{"$replace": [...]}` replaces
  the whole list (e.g. summarization). Still subject to conflict detection.
- Reducers run in Rust and never call user code. They behave the same in both
  SDKs, and only the *delta* crosses the language boundary.

## 4. Policies

Status: **implemented**.

| Policy | Scope | Default | Notes |
|---|---|---|---|
| `timeout` | per node attempt | none | Exceeding it → `node_timeout`. The node's cancel token fires and its future is dropped |
| `retry` | per node | none | Retries only when `NodeError.retryable == true`; backoff as 0003 §5; `ctx.attempt` increments |
| `deadline` | per run segment | none | Bounds everything, including backoff sleeps → `deadline_exceeded` |
| `cancel_grace` | per run | 5 s | Time given to in-flight nodes after cancellation |
| `max_steps` | per run | 64 | Unchanged; agents derive it (0004 §2) |

Policies are plain data (serde), so SDKs pass them through unchanged.

### 4.1 API

```rust
builder.policy("call_api", NodePolicy::new()
    .timeout(Duration::from_secs(10))
    .retry(RetryPolicy::new().max_attempts(3)));       // per node

workflow.run_with(input, RunOptions::default()
    .deadline(Duration::from_secs(60))                 // whole segment
    .cancel_token(token.clone())                       // token.cancel() from anywhere
    .cancel_grace(Duration::from_secs(5)));
```

JSON form: `{"timeout_ms": 10000, "retry": {"max_attempts": 3,
"initial_backoff_ms": 500, "backoff_factor": 2.0, "max_backoff_ms": 20000,
"jitter": true}}`.

Validation at `build`: a policy for an unknown node (`unknown_node`), two
policies for one node (`DuplicatePolicy`), `max_attempts` 0, a timeout under
1 ms, or a `backoff_factor` below 1 (`InvalidPolicy`). Policies are not part
of the workflow fingerprint: they change behaviour, not structure.

### 4.2 Retry

- Only errors the node marks retryable (`NodeError::new(..).retryable()`) are
  retried. Timeouts, panics and non-retryable errors fail immediately. A node
  with side effects should not mark its errors retryable (C4).
- `max_attempts` counts the first attempt (default 3).
- Before attempt *n*+1: `min(max_backoff, initial × factor^(n−1))`, with full
  jitter (uniform in `[0, that]`). Jitter is derived from the task id and the
  attempt number rather than a random generator, so it is reproducible and
  needs no dependency.
- Deadline-aware: if the backoff would end at or after the deadline, the node
  fails with `deadline_exceeded` without sleeping (0003 E6).
- Cancellation during a backoff ends the wait immediately.
- Events: each failed attempt that will be retried emits `attempt_failed`
  (`attempt`, `code`, `message`, `retry_in_ms`), then `node_started` with the
  next attempt number. Each `node_started` is closed by exactly one of
  `attempt_failed`, `node_finished`, `node_failed` or `node_cancelled`.
  `node_finished.duration_ms` covers all attempts.

### 4.3 Timeout, deadline and cancellation

- **Node timeout** applies to each attempt. On expiry, the node's cancel token
  fires, its future is dropped, and the step fails with `node_timeout`
  (siblings are cancelled, as with any failure).
- **Deadline** is measured from the start of each `run`/`resume` call. A
  wall-clock instant cannot be carried meaningfully in a snapshot. When it
  passes, in-flight nodes are cancelled at once (no grace: the deadline is a
  hard bound) and the run fails with `deadline_exceeded`. It is also checked
  before each step and when routers run.
- **Cancellation**: `CancelToken::cancel()` (from `RunOptions::cancel_token`,
  or `RunStream::cancel`). In-flight nodes see `ctx.cancel_token()` fire and
  have `cancel_grace` to return; their results are discarded, then they are
  dropped. The run fails with `cancelled`. Between steps it is noticed before
  the next step starts. Dropping the run future still cancels immediately.
- Each node gets a child token that fires on run cancellation, deadline,
  its own timeout, or a sibling's failure. Rust nodes are also dropped, so
  watching the token is optional for them; language bridges rely on it.
- `ctx.remaining_time()` is the time left before the earlier of the run
  deadline and the attempt's timeout.
- A cancelled, timed-out or deadline-exceeded step commits nothing.
- Unfinished nodes get `node_cancelled` with reason `run_cancelled`,
  `deadline_exceeded` or `sibling_failed`.
- In Rust, cancellation surfaces as a `RunError` with code `cancelled`. The
  SDKs map it to their native form (`CancelledError`, `AbortError`; 0003 §2).

## 5. Snapshots, saved task writes and resume

Status: **implemented**.

### 5.1 Atomicity, restated

> The **state commit** of a step is all-or-nothing (unchanged).
> In addition, each **finished task's output is recorded** in the step's
> *pending writes* as soon as it finishes, together with any `ctx.memo()`
> entries the task saved while running (§1). When a failed step is resumed,
> tasks with recorded output are **not executed again**. Tasks that did not
> finish are re-executed with their memo entries available, so finer-grained
> work they already completed (e.g. individual tool calls) is not repeated.

This is LangGraph's best runtime idea. It closes the duplicate-side-effect gap
from 0003 E2: a failed step can be resumed after fixing a bug without
re-running the tools that already succeeded.

**Task memo.** `ctx.memo()` returns the task's `TaskMemo`:

| Method | Meaning |
|---|---|
| `get(key) -> Option<Value>` | A value saved by an earlier attempt or execution of this task |
| `put(key, value)` | Saves at once (not at step commit). A later `put` to the same key replaces it |
| `once(key, async fn) -> Result<T>` | Returns the saved value if there is one; otherwise runs the function and saves its `Ok` result. Errors are not saved |

The memo survives, for the same node in the same pending step:

- **retries** (same `TaskMemo` object across attempts, also after a timed-out
  attempt),
- **resume of a failed step** (memo travels in the failure snapshot),
- **resume of a suspended node** (memo travels in the suspension snapshot, so a
  node that did work before suspending can skip it when invoked again).

It is cleared when the step commits for that node (a finished node's next
execution in a later step starts empty). Values are JSON and must be small;
the memo is not a store for documents.

**Blame rule.** Some failures come from a finished task's output rather than
from a running task: `update_conflict` (both writers), `invalid_value`,
`missing_goto`, and `unknown_route` from a `goto`. That output is **not**
saved, so on resume the node runs again. This is what makes "fix the bug and
resume" work. Router failures keep all outputs: routers are re-evaluated on
resume.

### 5.2 Snapshot format (language-neutral JSON)

```jsonc
{
  "format": "agentbuild.snapshot/1",
  "workflow_fingerprint": "fnv1a64:…",      // graph structure + reducers (not policies)
  "run_id": "…", "step": 7,
  "state": { … },                           // committed state before `step`
  "frontier": ["tools", "notes"],           // nodes `step` (re)executes
  "join_progress": { "combine": ["short"] },
  "suspended": [ { "node": "review", "payload": { … } } ],
  "pending_writes": {                       // keyed by node name; omitted when empty
    "notes": { "output": { "update": { "notes": "…" } } },   // finished: reused
    "tools": { "memo": { "call:c2": { … } } },              // unfinished: re-run with memo
    "review": { "resume": true }                              // resume value already supplied
  },
  "error": { "code": "node_failed", "message": "…" },        // only on failure snapshots
  "event_seq": 42
}
```

- **Plain JSON, no language-specific class names.** LangGraph checkpoints embed
  Python class paths in msgpack, which a TypeScript SDK cannot read
  ([research](../research/langgraph.md)). Our snapshots can be resumed from
  either SDK.
- `output` uses the external tag form: `{"update": {…}}`,
  `{"command": {"update": {…}, "goto": [..]}}`,
  `{"suspend": {"update": {…}, "payload": …}}`.
- `pending_writes` is keyed by node name, because a step has at most one task
  per node. Dynamic fan-out (§2, deferred) will need a new key form and a new
  format version.
- A snapshot is returned on `suspended` and attached to **every failure that
  happens while running** (`RunError::snapshot`), including cancellation,
  deadlines and the step limit. Even with no persistence backend, the app can
  store the snapshot anywhere and resume later. This gives durability *by
  value* from day one.
- The exceptions are `resume_invalid` and `workflow_mismatch`: they are
  rejected before anything runs, and the caller still holds the snapshot it
  passed in, unchanged.
- New fields have serde defaults, so snapshots from earlier builds still load.

### 5.3 Resume

`workflow.resume(snapshot, values)` (or `retry(snapshot)` when no node is
suspended, typically after a failure):

1. Check `format`, then `workflow_fingerprint`. A mismatch gives
   `workflow_mismatch`. (`allow_mismatch` is not offered yet.)
2. Check `values`: exactly one per entry in `suspended`. Check that every
   `pending_writes` entry names a node in the `frontier`.
3. Restore state, join progress and frontier.
4. Re-run the snapshot's step: nodes with a recorded `output` reuse it and do
   not run (no `node_started`; `node_finished` with `reused: true`); suspended
   nodes get `ctx.resume()` (from `values`, or from a recorded `resume`); the
   rest execute with their memo.
5. Continue normally.

Resume input that fails validation is rejected **before** anything runs, so the
snapshot stays usable. Resuming the same snapshot twice runs it twice; preventing
that needs a checkpointer with versions (§5.4).

Choices made while implementing:

- A failure snapshot does not repeat `suspended`: resume values the caller
  already supplied are kept in `pending_writes[node].resume`, so `retry` needs
  no input.
- A failure at a step boundary (cancel, deadline, step limit) has a snapshot
  whose step has not started; resuming simply starts it. For the step limit,
  pass a larger `max_steps`.
- `RunError` is now `#[non_exhaustive]` and carries `snapshot:
  Option<Box<Snapshot>>`.

### 5.4 Persistence (later; designed in [0008](0008-persistence-and-memory.md))

A `Checkpointer` trait (`put(snapshot, expected_version)`, `get(run_id)`,
`list(thread)`) adds automatic saving after every committed step and
compare-and-set versions. A second resume of the same snapshot then fails
instead of forking (0004 A10). Message history is stored as an append-only log
from the start; LangGraph's full-copy-per-checkpoint design costs O(n²) bytes
and needed a later `DeltaChannel` fix. Backends (SQLite, Postgres) come after
the in-memory design is proven.

## 6. Events

Status: **implemented** (core lifecycle events and custom events). Message
deltas and agent events arrive with 0002/0004; `attempt_failed` with §4.

### 6.1 Envelope

```jsonc
{
  "seq": 42,                  // per run, starts at 0, continues across resumes
  "run_id": "…",
  "step": 3, "node": "tools", "task_id": "…",   // omitted when not applicable
  "origin": "internal",       // "internal" (engine) | "user" (ctx.events().emit)
  "type": "node_finished",
  "data": { … },
  "ts_ms": 1791720000000      // Unix time in milliseconds
}
```

Implementation decisions:

- `ts_ms` (an integer) replaces the ISO string originally sketched, to avoid a
  date-time dependency in the core. SDKs can render it.
- `parent_run_id`, `namespace` and `call_id` are added when subgraphs and
  tools exist. Adding fields is backwards compatible.
- `seq` continues across suspend/resume: the snapshot stores the next value
  (`event_seq`, default 0 for older snapshots).

### 6.2 Core event types

| `type` | scope | `data` |
|---|---|---|
| `run_started` | run | `resumed` |
| `step_started` | step | `nodes` |
| `node_started` | node | `attempt` |
| `node_finished` | node | `duration_ms`, `update_keys`, `goto`, `suspended`, `reused` (output saved by an earlier failed attempt of this step; the node did not run) |
| `node_failed` | node | `duration_ms`, `code` (`node_failed` / `node_panicked`), `message` |
| `node_cancelled` | node | `reason` (`sibling_failed`). Every `node_started` gets exactly one of finished / failed / cancelled |
| `step_committed` | step | `next` (names scheduled for the next step) |
| `suspended` | step | `interrupts` |
| `run_finished` | run | `status` (`completed` / `suspended` / `failed`), `error` (code, if failed), `steps` |
| `custom` | node | `name`, `payload` (from `ctx.events().emit`) |

`attempt_failed` comes with retries (§4). Agent-level types are listed in
0004 §8. Message streaming uses `message_delta` / `message_completed` /
`message_aborted` (0002 §8).

Every run that starts emits exactly one `run_finished`, including on failure.
Validation failures before anything runs (`resume_invalid`,
`workflow_mismatch`) emit nothing.

### 6.3 Delivery rules

- One subscriber per run: `event_channel(capacity)` gives a sender for
  `RunOptions::events` and a receiver. `Workflow::stream` wraps this: it spawns
  the run and returns a `RunStream` (`next()` for events, `finish()` for the
  result).
- **No event is dropped** while the subscriber is connected. Emitting never
  blocks a node: if the channel is full, events wait in an in-order backlog.
  The run drains the backlog at each step boundary and before returning, and
  that is where a slow subscriber slows the run down (backpressure), never
  mid-node.
- A subscriber that holds the receiver but never reads will stall the run at
  the next step boundary once the channel is full. Read events concurrently
  (as `RunStream` does), or drop the receiver.
- When *delta* events exist, a slow subscriber gets adjacent deltas
  **coalesced** (fewer, larger deltas, never gaps). Not yet applicable.
- With no subscriber, nothing is built or serialized.
- Observing never changes execution. LangChain switches `invoke` to streaming
  when a streaming callback is attached
  ([research](../research/langchain-core.md)); we must not.
- A disconnected subscriber (receiver dropped) is removed; the run continues
  (C8).

## 7. Routers: what they see

LangGraph evaluates a conditional edge inside the source node's task, so it sees
only that node's own writes. AgentBuild routers keep 0001's rule: they see the
**merged state of the whole step**. This is deliberate. Routing is defined on
the committed view and does not depend on which sibling finished first.

## 8. Edge cases

| # | Case | Behaviour |
|---|---|---|
| C1 | Node returns `Command` to a target not in `may_goto` | `unknown_route`; step not committed |
| C2 | `Suspend` and a failure in the same step | Failure wins (step not committed); snapshot has the failure, with the suspending task's output in `pending_writes` |
| C3 | Node exceeds `timeout` but ignores cancellation (sync Python) | Result discarded; `node_timeout`; `warning: task_abandoned` (0003 §8) |
| C4 | Retry policy on a node that sent an email before failing | Retries only if the node marked the error `retryable`; nodes with side effects should not |
| C5 | `upsert_by_id` write without `id` | `invalid_value` (developer error) |
| C6 | `append` key receives a non-list | `invalid_value` |
| C7 | Resume a snapshot whose `format` is newer than the runtime | `resume_invalid` ("unsupported snapshot format") |
| C8 | Event subscriber disconnects mid-run | Subscriber removed; the run continues |
| C9 | Snapshot taken on failure, then the app changes code but not graph shape | Fingerprint matches (structure only); resume proceeds. Behaviour changes are the app's responsibility |
| C10 | Two nodes suspend; resume supplies only one value | `resume_invalid`; snapshot unchanged |

## 9. Changes to 0001 (when implemented)

1. `Node::run` takes `NodeContext` and returns `NodeOutput`. Breaking for Rust
   node authors; `node_fn` adapters keep the closure form.
2. Run outcome becomes `Completed | Suspended | Failed | Cancelled`;
   `RunError` gains `snapshot`.
3. Atomicity text is extended with saved task writes (§5.1).
4. New error codes: `node_timeout`, `deadline_exceeded`, `invalid_value`,
   `resume_invalid`, `workflow_mismatch`, `internal` (0003 §9).
5. `NodeError` gains `code` and `retryable` (0003 §6.1).
6. The state model gains reducers; the default `replace` behaves exactly as
   today.

Suggested implementation order (each item is small, tested and releasable):
**§1 context → §2 outcomes → §3 reducers → §6 events → §4 policies → §5
snapshots/resume**. The Python bridge (Phase 2) can start after §1–§3.
