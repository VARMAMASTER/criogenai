# 0015: Workflows in the SDKs

Status: **Accepted** (task 13 of the second roadmap in `docs/progress.md`;
the user gave full design freedom). Level 3 of
[0007](0007-public-api.md) in Python and TypeScript, on the Rust engine
([0001](0001-execution-model.md), [0005](0005-core-runtime-extensions.md))
and its threads ([0008](0008-persistence-and-memory.md) §18).

## 1. The API

```python
from agentbuild import Workflow, END, suspend, goto

wf = Workflow(reducers={"log": "append"})

@wf.node
async def classify(state):                    # (state) or (state, ctx)
    return {"category": "billing" if "charge" in state["text"] else "other"}

@wf.node
def approve(state, ctx):
    if ctx.resume is None:
        return suspend({"refund": state["amount"]})   # pause for a person
    return {"approved": ctx.resume == "yes"}

wf.add_node("billing", billing_agent)        # an Agent: reads and writes `messages`
wf.start("classify")
wf.route("classify", lambda s: s["category"], {"billing": "approve", "other": END})
wf.edge("approve", "billing")
wf.edge("billing", END)

app = wf.compile(storage=Storage("sqlite:///app.db"))   # validated in Rust
result = await app.run({"text": "I was charged twice", "amount": 40})
thread = app.thread("ticket-7", actor="user:ada")
turn = await thread.run({...})
if turn.suspended:
    turn = await thread.resume({"approve": "yes"})
```

```ts
const wf = new Workflow({ reducers: { log: "append" } })
  .node("classify", async (state) => ({ category: "billing" }))
  .node("approve", (state, ctx) => (ctx.resume === undefined ? suspend({ refund: state.amount }) : { approved: ctx.resume === "yes" }))
  .addNode("billing", billingAgent)
  .start("classify")
  .route("classify", (s) => s.category, { billing: "approve", other: END })
  .edge("approve", "billing")
  .edge("billing", END);
const app = wf.compile({ storage });
const turn = await app.thread("ticket-7").run({ text: "…" }, { signal });
```

| Python | TypeScript | Rust |
|---|---|---|
| `@wf.node`, `wf.add_node(name, fn_or_agent)` | `.node(name, fn)`, `.addNode(name, fnOrAgent)` | `add_node` (a bridged node; `Agent::as_node`) |
| `wf.start(name)` | `.start(name)` | `set_entry` |
| `wf.edge(a, b)` | `.edge(a, b)` | `add_edge` |
| `wf.route(node, fn, {label: target})` | `.route(node, fn, {...})` | `add_router` (a bridged router) |
| `wf.join([a, b], c)` | `.join([a, b], c)` | `add_join` |
| `wf.goto(node, [targets])` + `return goto(targets, update)` | `.goto(node, [...])` + `goto(...)` | `may_goto` + `NodeOutput::goto` |
| `Workflow(reducers={key: "append" \| "upsert_by_id" \| "replace"})` | `new Workflow({ reducers })` | `reducer` |
| `return suspend(payload, update=None)` | `suspend(payload, update?)` | `NodeOutput::suspend` |
| `wf.compile(storage=…)` | `wf.compile({ storage })` | `build` (all issues at once) |
| `app.run(input, max_steps=, deadline=)` | `app.run(input, { maxSteps, deadlineMs, signal })` | `Workflow::run_with` |
| `app.resume(snapshot, {node: value})` | `app.resume(snapshot, values)` | `resume_with` (no storage) |
| `app.thread(id, actor=)`: `run`, `resume`, `retry`, `interrupts`, `history`, `state`, `rewind`, `delete` | same, camelCase | `WorkflowThread` |

A node returns a dict/object (the update), `None`/`undefined` (no
update), `suspend(...)` or `goto(...)`. `ctx` has `run_id`, `step`,
`node`, `attempt` and `resume` (the value given to `resume`, else
`None`/`undefined`). Results: `state`, `steps` (count), `suspended`,
`interrupts` (`[{node, payload}]`), and `snapshot` (JSON, for
`app.resume` without storage).

## 2. Rules

- **Engine semantics stay in Rust** (rule 3): scheduling, joins, reducers,
  step atomicity, validation, suspend/resume, retries of failed runs.
  The SDK only builds the graph description and runs user functions.
- A node or router that raises fails the run with `node_failed` /
  `router_failed`; the exception is the error's `__cause__` / `cause`
  (kept by run id and node, as tools keep theirs by call id).
- State values cross as JSON. Nodes running in the same step run
  concurrently in Rust; Python sync functions run on worker threads, async
  ones on the caller's event loop (the tools' rules, 0011 §4).
- `compile` reports every graph problem at once (`ValidationError` with
  `details.issues`), as the Rust builder does.

## 3. Tests

Python and TypeScript, simulated models only: a linear workflow, a router,
parallel nodes with a join and an `append` reducer, `goto`, a node that
pauses and resumes (plain and on a thread, from a second handle), a
failing node with its `cause` and `retry` on a thread, an agent as a
node, compile errors listing several issues, cancellation. Package tests
and consumers extended.

## 4. Not in this slice

Typed state classes (Pydantic / Zod schemas for the state), streaming a
workflow's events, node policies (timeouts, retries per node) and
workflows as nodes of other workflows.
