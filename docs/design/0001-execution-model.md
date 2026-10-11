# 0001 — Execution model

Status: **Implemented** in `crates/agentbuild-core` (Phase 1). Pre-1.0; may change.

This is the contract the Python and TypeScript SDKs will build on. Anything
not described here is unspecified.

## Decision

Workflows run as **supersteps** over a directed graph that may contain cycles.

1. The **entry** node is scheduled for step 0.
2. In each step, every scheduled node runs **concurrently** on the Tokio
   runtime against the **same immutable snapshot** of the state.
3. Each node returns an **update** (a partial JSON object). Updates are merged
   in node declaration order. If two nodes in the same step write the same
   key, the step fails with `update_conflict`.
4. Outgoing edges of each node that ran decide what is scheduled next:
   - **static edge** `a → b`: schedule `b`. Multiple static edges fan out.
   - **router** on `a`: an async function sees the *merged* state and returns
     a label; the label's declared target is scheduled. Routers on different
     nodes in a step are evaluated sequentially in declaration order.
   - **join** `[a, b] → c`: schedule `c` once every source has completed
     since `c` last ran, even if sources finish in different steps.
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

### Failure behaviour

| code                  | cause                                          |
|-----------------------|------------------------------------------------|
| `node_failed`         | a node returned `NodeError`                    |
| `node_panicked`       | a Rust node panicked (caught, not propagated)  |
| `router_failed`       | a router returned `NodeError`                  |
| `unknown_route`       | a router returned a label it did not declare   |
| `update_conflict`     | two nodes in one step wrote the same key       |
| `step_limit_exceeded` | `max_steps` reached                            |

Every `RunError` has `code`, `step` (zero-based index of the failing step),
and `state`. `NodeError` is language-neutral: `message`, optional `kind`
(e.g. a Python exception class name), optional JSON `details`.

Within a step, failure is **fail-fast**: the first error observed aborts the
step's other nodes. If several nodes fail, which one is reported depends on
timing. Dropping the `run` future cancels in-flight nodes at their next
`.await`.

## State

State is a JSON object (`serde_json::Map`). Updates replace whole keys; there
is no deep merge and no per-key reducer yet. Writing `null` stores `null`.

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

## Not yet specified

Streaming events, per-node retries and timeouts, explicit cancellation
tokens, reducers, subgraphs, and persistence.
