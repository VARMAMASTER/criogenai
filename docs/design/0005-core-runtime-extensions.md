# 0005: Core runtime extensions

Status: **Proposed** (design phase). This document changes the implemented
contract in [0001](0001-execution-model.md); every change is listed in §9.
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

- Declared on the workflow: `builder.state_key("messages", Reducer::UpsertById)`.
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

| Policy | Scope | Default | Notes |
|---|---|---|---|
| `timeout` | per node | none | Exceeding it → `node_timeout`. The node's future is dropped; the cancellation token fires |
| `retry` | per node | none | Retries only when `NodeError.retryable == true`; backoff as 0003 §5; `ctx.attempt` increments |
| `deadline` | per run | none | Bounds everything, including backoff sleeps → `deadline_exceeded` |
| `cancel_grace` | per run | 5 s | Time given to in-flight nodes after cancellation |
| `max_steps` | per run | 64 | Unchanged; agents derive it (0004 §2) |

Policies are plain data (serde), so SDKs pass them through unchanged.

## 5. Snapshots, saved task writes and resume

### 5.1 Atomicity, restated

> The **state commit** of a step is all-or-nothing (unchanged).
> In addition, each **finished task's output is recorded** in the run's
> *pending writes* as soon as it finishes, together with any `ctx.memo`
> entries the task saved while running (§1). When a step is retried or
> resumed, tasks with recorded output are **not executed again**. Tasks that
> did not finish are re-executed with their memo entries available, so
> finer-grained work they already completed (e.g. individual tool calls) is not
> repeated.

This is LangGraph's best runtime idea. It closes the duplicate-side-effect gap
from 0003 E2: a failed step can be resumed after fixing a bug without
re-running the tools that already succeeded.

### 5.2 Snapshot format (language-neutral JSON)

```jsonc
{
  "format": "agentbuild.snapshot/1",
  "workflow_fingerprint": "sha256:…",       // hash of the graph structure + reducers + policies
  "run_id": "…", "step": 7, "created_at": "…",
  "state": { … },
  "frontier": ["tools"],                    // nodes scheduled for the step to (re)execute
  "join_progress": { "combine": ["short"] },
  "pending_writes": { "<task_id>": { "node": "tools", "output": null,   // null = task did not finish
                                     "memo": { "call:c2": { … } } } },
  "suspended": [ { "task_id": "…", "node": "review", "payload": { … } } ],
  "error": null                              // the RunError when the snapshot came from a failure
}
```

- **Plain JSON, no language-specific class names.** LangGraph checkpoints embed
  Python class paths in msgpack, which a TypeScript SDK cannot read
  ([research](../research/langgraph.md)). Our snapshots can be resumed from
  either SDK.
- A snapshot is returned on `suspended` and attached to every `failed` outcome
  (`error.snapshot`). Even with no persistence backend, the app can store the
  snapshot anywhere and resume later. This gives durability *by value* from
  day one.

### 5.3 Resume

`workflow.resume(snapshot, resume_values?, options?)`:

1. Check `format`, then `workflow_fingerprint`. A mismatch gives
   `workflow_mismatch`, unless `allow_mismatch` (the caller accepts the risk).
2. Restore state, join progress and frontier.
3. Re-run the snapshot's step: tasks in `pending_writes` reuse their output;
   suspended tasks get `ctx.resume`; the rest execute.
4. Continue normally.

Resume input that fails validation is rejected **before** anything runs, so the
snapshot stays usable.

### 5.4 Persistence (later; designed in [0008](0008-persistence-and-memory.md))

A `Checkpointer` trait (`put(snapshot, expected_version)`, `get(run_id)`,
`list(thread)`) adds automatic saving after every committed step and
compare-and-set versions. A second resume of the same snapshot then fails
instead of forking (0004 A10). Message history is stored as an append-only log
from the start; LangGraph's full-copy-per-checkpoint design costs O(n²) bytes
and needed a later `DeltaChannel` fix. Backends (SQLite, Postgres) come after
the in-memory design is proven.

## 6. Events

### 6.1 Envelope

```jsonc
{
  "seq": 42,                       // monotonically increasing per run; enables resumable streams
  "run_id": "…", "parent_run_id": null,
  "namespace": ["support", "research"],   // nesting path for subgraphs / sub-agents
  "step": 3, "node": "tools", "task_id": "…", "call_id": "c1",
  "origin": "agent" | "hook:<name>" | "internal" | "user",
  "type": "node_started",
  "data": { … },
  "ts": "2026-10-11T12:00:00.000Z"
}
```

### 6.2 Core event types

`run_started`, `step_started`, `node_started`, `node_finished` (duration,
update keys), `node_failed`, `attempt_failed`, `step_committed`, `suspended`,
`run_finished` (outcome), `custom` (from `ctx.events.emit`). Agent-level types
are listed in 0004 §8. Message streaming uses `message_delta` /
`message_completed` / `message_aborted` (0002 §8).

### 6.3 Delivery rules

- Events are delivered through a bounded channel per subscriber.
- **Lifecycle events are never dropped.** If a subscriber is slow, *delta*
  events are **coalesced** (adjacent text deltas for the same block merged), so
  slow consumers get fewer, larger deltas, never gaps.
- Emitting never blocks node execution. If a subscriber's buffer is full of
  lifecycle events, the run waits at the step boundary (backpressure), not
  mid-node.
- With no subscriber, events cost almost nothing (no serialization).
- Observing never changes execution. LangChain switches `invoke` to streaming
  when a streaming callback is attached
  ([research](../research/langchain-core.md)); we must not.

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
