# 0001 — Execution model

Status: **Implemented** in `crates/agentbuild-core`. Pre-1.0; may change.
Extended by [0005](0005-core-runtime-extensions.md) §1–§3 (node context,
node outcomes, reducers, suspend/resume), §4 (policies), §5 (saved task
writes, the task memo, snapshots on failure) and §6 (events), which are
implemented and described below.

This is the contract the Python and TypeScript SDKs will build on. Anything
not described here is unspecified.

## Decision

Workflows run as **supersteps** over a directed graph that may contain cycles.

1. The **entry** node is scheduled for step 0.
2. In each step, every scheduled node runs **concurrently** on the Tokio
   runtime against the **same immutable snapshot** of the state.
3. Each node receives a **`NodeContext`** (`run_id`, `step`, `node`,
   `task_id` = `"{run_id}:{step}:{node}"`, `attempt`, `resume`, `memo`) and returns a
   **`NodeOutput`**:
   - `Update`: a partial JSON object;
   - `Command { update, goto }`: also schedules `goto` targets, which must
     be declared with `may_goto` (see below);
   - `Suspend { update, payload }`: pauses the run (see Suspension).

   Updates are merged in node declaration order using each key's
   **reducer** (see State).
4. Outgoing edges of each node that ran decide what is scheduled next:
   - **static edge** `a → b`: schedule `b`. Multiple static edges fan out.
   - **router** on `a`: an async function sees the *merged* state and returns
     a label; the label's declared target is scheduled. Routers on different
     nodes in a step are evaluated sequentially in declaration order.
   - **join** `[a, b] → c`: schedule `c` once every source has completed
     since `c` last ran, even if sources finish in different steps.
   - **goto** from a `Command`: targets are scheduled **in addition** to static
     edges. Targets not declared with `may_goto` fail the step with
     `unknown_route`. A node with `may_goto` targets but no static edges or
     joins must name a target every time; otherwise the step fails with
     `missing_goto`.
   - Targets may be `END`, which schedules nothing.
5. A node scheduled by several predecessors in one step runs **once**.
6. The run finishes when nothing is scheduled. It fails with
   `step_limit_exceeded` if it reaches `max_steps` (default 64).

### Atomicity

A step is **all-or-nothing**. If any node, router, or merge in a step fails,
none of that step's updates or join progress is committed, and the error
carries the state as of the last completed step. Supersteps are therefore
the natural checkpoint boundary for future persistence work (Phase 5); no
persistence exists yet.

### Suspension

When a node returns `Suspend`, the step commits normally: all nodes' updates,
including the suspending node's own. Then:

- The suspending node's outgoing edges are **not** followed, and it does not
  count toward joins. It has not completed.
- The run returns `RunOutput { status: Suspended { interrupts, snapshot } }`.
  This is a normal return, not an error.
- The **snapshot** is plain JSON: format `agentbuild.snapshot/1`, workflow
  fingerprint, `run_id`, next step index, state, frontier (the scheduled nodes
  *plus* the suspended ones), join progress and interrupts.
- `Workflow::resume(snapshot, {node: value})` validates the input first
  (`resume_invalid` and `workflow_mismatch` leave the snapshot usable). It then
  runs the next step with the frontier. Suspended nodes receive
  `ctx.resume()`; no other node re-runs.
- `max_steps` counts steps from the start of the run, across resumes.

The **workflow fingerprint** is an FNV-1a hash of the graph structure (nodes,
edges, routes, `may_goto`, joins and reducers), not of node implementations.

### Saved task writes, the task memo, and snapshots on failure

Details in [0005 §5](0005-core-runtime-extensions.md#5-snapshots-saved-task-writes-and-resume).

- The state commit stays all-or-nothing. In addition, each node's output is
  **saved as soon as the node finishes**. If the step then fails, the
  `RunError` carries a `snapshot` (JSON, same format as for suspension) with
  those outputs in `pending_writes`.
- `Workflow::retry(snapshot)` re-runs the failed step: nodes with a saved
  output are **not run again** (`node_finished` with `reused: true`); the
  others run.
- `ctx.memo()` saves finer-grained results immediately (`get`, `put`,
  `once(key, f)`). They survive retries, timeouts, failure snapshots and
  suspension, so work such as an already-sent email is not repeated. A
  node's memo starts empty in each new step.
- **Blame rule:** an output that itself caused the failure
  (`update_conflict`, `invalid_value`, `missing_goto`, `unknown_route` from a
  goto) is not saved, so its node runs again after the fix.
- Output a node returns after the run was cancelled is not saved (it may be
  a partial result); its memo entries are.
- Every failure while running has a snapshot, including `cancelled`,
  `deadline_exceeded` and `step_limit_exceeded` (retry with a larger
  `max_steps`). Rejected resume input (`resume_invalid`,
  `workflow_mismatch`) has none; the caller still holds its snapshot.

### Failure behaviour

| code                  | cause                                          |
|-----------------------|------------------------------------------------|
| `node_failed`         | a node returned `NodeError`                    |
| `node_panicked`       | a Rust node panicked (caught, not propagated)  |
| `router_failed`       | a router returned `NodeError`                  |
| `unknown_route`       | a router returned a label it did not declare   |
| `update_conflict`     | two nodes in one step wrote the same key       |
| `step_limit_exceeded` | `max_steps` reached                            |
| `invalid_value`       | a write a reducer cannot apply (e.g. non-list to `append`) |
| `missing_goto`        | a goto-only node returned no target            |
| `resume_invalid`      | bad resume input or snapshot format            |
| `workflow_mismatch`   | snapshot from a workflow with another fingerprint |
| `node_timeout`        | a node attempt exceeded its policy's timeout   |
| `deadline_exceeded`   | the run's deadline passed                      |
| `storage_failed`      | the run's persister could not save progress (0008 §12) |
| `cancelled`           | the run's cancel token fired                   |

Every `RunError` has `code`, `step` (zero-based index of the failing step),
and `state`. `NodeError` is language-neutral: `message`, optional `kind`
(e.g. a Python exception class name), optional JSON `details`.

Within a step, failure is **fail-fast**: the first error observed aborts the
step's other nodes. If several nodes fail, which one is reported depends on
timing. Dropping the `run` future cancels in-flight nodes at their next
`.await`.

### Events

A run can report what happens as it happens: `RunOptions::events(sender)`
with `event_channel(capacity)`, or `Workflow::stream(input)`, which runs in
the background and returns a `RunStream`. The envelope, the event types and
the delivery rules are in [0005 §6](0005-core-runtime-extensions.md#6-events).
In short:

- Every event has a `seq` that starts at 0 and continues across resumes
  (stored in the snapshot as `event_seq`).
- Every `node_started` is followed by exactly one `node_finished`,
  `node_failed` or `node_cancelled`. Every run that starts ends with one
  `run_finished`, including failed and suspended runs.
- Nodes emit their own events with `ctx.events().emit(name, payload)`.
- No event is lost while the subscriber is connected. Emitting never blocks
  a node; a slow subscriber slows the run only at step boundaries.
- Observing a run never changes its result.

### Policies: timeouts, retries, deadlines, cancellation

Details in [0005 §4](0005-core-runtime-extensions.md#4-policies). In short:

- `builder.policy(node, NodePolicy::new().timeout(..).retry(..))`. The
  timeout applies to **each attempt** (`node_timeout`). Retries happen only for
  errors marked `NodeError::retryable()`, with exponential backoff and full
  jitter. Timeouts and panics are never retried.
- `RunOptions::deadline(d)` bounds a whole `run`/`resume` call, including
  backoff (`deadline_exceeded`). A backoff that would cross the deadline fails
  at once.
- `RunOptions::cancel_token(t)` plus `t.cancel()`, or `RunStream::cancel()`:
  in-flight nodes see `ctx.cancel_token()` fire and get `cancel_grace`
  (default 5 s) to return. Their results are discarded, then they are
  dropped. The run fails with `cancelled`.
- In every case the interrupted step commits nothing; earlier steps stay
  committed.

## State

State is a JSON object (`serde_json::Map`). Each key has a **reducer**, declared
with `WorkflowBuilder::reducer(key, Reducer)`:

| Reducer | Behaviour | Conflict |
|---|---|---|
| `Replace` (default) | Write replaces the value; `null` stores `null` | Two writers in one step: `update_conflict` |
| `Append` | Lists appended in node declaration order | none |
| `UpsertById` | Items with a string `id`: new id appends, existing id replaces in place, `{"id", "$remove": true}` removes | Same id written twice in one step: `update_conflict` (key `k[id=x]`) |

For `Append` and `UpsertById` keys, writing `{"$replace": [...]}` replaces
the whole list. Merges happen on a copy, so a failed merge commits nothing.

**Why JSON:** state must cross into Python and JavaScript. A dynamic JSON
object gives one representation all three languages agree on, at the cost of
Rust-side static typing and a copy at each language boundary. Typed state can
be layered on top (e.g. Pydantic models or Zod schemas validated at the SDK
edge) without changing the core.

**Known costs:** each step clones the state once to apply updates. Nodes
receive an `Arc<State>`, so fan-out does not clone per node. These costs are
unmeasured; benchmarks come before optimization.

## Validation

`WorkflowBuilder::build` reports **all** issues at once: empty graph, missing
or unknown entry, empty/reserved (`__` prefix)/duplicate names, references to
unknown nodes, dead ends (no outgoing edge, router, or join), unreachable
nodes, multiple routers on a node, a router mixed with static edges, empty or
duplicate route labels, empty joins, and join targets that are also scheduled
another way.

Several rules are deliberately strict (no router + static edges on one node,
join targets reachable only through their join). Relaxing a rule later is
backwards compatible; tightening one is not.

## Alternatives considered

- **Pure DAG with topological execution** (Airflow-style). Simple, with
  obvious dependency semantics, but cannot express agent loops
  (model → tool → model), which Phase 4 needs.
- **Free-running actor/event model** (each node fires as soon as an input
  arrives). Maximally concurrent, but ordering and state merges become
  timing-dependent, which hurts reproducibility, testing, and checkpointing.
- **Supersteps (chosen)**, the bulk-synchronous model also used by Pregel and
  LangGraph. Supports cycles, gives deterministic merges and a clean
  checkpoint boundary. Cost: a fast node in a step waits for the slowest
  before its successors start. Joins let unequal branches proceed
  independently.

## Language bridge seam

`Node` and `Router` are object-safe traits that return boxed `Send` futures.
Bindings will implement them by calling into Python (PyO3) or Node.js
(NAPI-RS threadsafe functions). Routers are async for this reason: a
JavaScript function cannot be called synchronously from a Tokio worker thread.

## Not yet implemented

Designed in 0005–0008 but not yet built: message-delta events, persistence
backends (checkpointers with versions, so the same snapshot cannot be resumed
twice), dynamic fan-out (`Send`) and subgraphs.
