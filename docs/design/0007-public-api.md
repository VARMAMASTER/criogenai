# 0007: Public API — building agents and workflows

Status: **Proposed** (design phase). This document shows what *users* write.
It is the developer-facing view of [0001](0001-execution-model.md) (workflows),
[0002](0002-messages-and-tools.md) (messages and tools),
[0003](0003-error-model.md) (errors), [0004](0004-agent-loop.md) (agents) and
[0005](0005-core-runtime-extensions.md) (state, streaming, resume). Provider
access goes through the embeddable gateway (0006, pending).

## Principles

1. **Progressive disclosure.** One line for a model call, a few lines for an
   agent, full control for a workflow. Each level reuses the concepts of the
   level before it.
2. **Same concepts, native idioms.** Python uses type hints, Pydantic,
   decorators and `async`/`await`. TypeScript uses Zod, object options,
   Promises and `AbortSignal`. The names match across languages.
3. **No Rust visible.** Users never see Rust types, lifetimes or build steps:
   `pip install criogen` / `npm i criogen` ship prebuilt binaries.
4. **Errors are typed and predictable** (0003). Every failure is a documented
   exception class with a stable `code`.
5. **Validation before running.** Graphs and agents are checked when built, and
   all problems are reported at once.

---

## Level 0: call a model

```python
from criogen import Gateway

gw = Gateway()                                     # keys from env (ANTHROPIC_API_KEY, …)
reply = await gw.chat("anthropic:claude-sonnet-5-5", "Summarize this: …")
print(reply.text)

async for chunk in gw.stream("openai:gpt-…", "Write a haiku"):
    print(chunk.text, end="")
```

```ts
import { Gateway } from "criogen";

const gw = new Gateway();
const reply = await gw.chat("anthropic:claude-sonnet-5-5", "Summarize this: …");
for await (const chunk of gw.stream("openai:gpt-…", "Write a haiku")) process.stdout.write(chunk.text);
```

(Built as [0018](0018-sdk-gateway-client.md), which settles the details;
the `endpoints` map below became model objects.)

A gateway or local server is just another endpoint:
`Gateway(endpoints={"local": OpenAICompatible(base_url="http://localhost:11434/v1")})`,
then `"local:llama-…"`.

## Level 1: an agent with tools

```python
from criogen import Agent, tool, ToolError

@tool
async def get_order(order_id: str) -> dict:
    """Look up an order by id."""
    order = await db.orders.find(order_id)
    if order is None:
        raise ToolError(f"No order {order_id}")     # the model sees this and can recover
    return order

agent = Agent(
    model="anthropic:claude-sonnet-5-5",
    instructions="You are a support agent. Be concise.",
    tools=[get_order],
)

result = await agent.run("Where is order A-1001?")
print(result.output)          # final answer text
print(result.usage)           # tokens across all calls
```

```ts
import { Agent, tool, ToolError } from "criogen";
import { z } from "zod";

const getOrder = tool({
  name: "get_order",
  description: "Look up an order by id.",
  input: z.object({ orderId: z.string() }),
  run: async ({ orderId }) => {
    const order = await db.orders.find(orderId);
    if (!order) throw new ToolError(`No order ${orderId}`);
    return order;
  },
});

const agent = new Agent({ model: "anthropic:claude-sonnet-5-5", instructions: "…", tools: [getOrder] });
const result = await agent.run("Where is order A-1001?");
```

## Level 2: typed output, limits, hooks, context

```python
from pydantic import BaseModel
from criogen import Agent, Limits, Approve, ToolContext

class Resolution(BaseModel):
    category: str
    reply: str
    refund: bool

@tool(destructive=True)
async def issue_refund(order_id: str, amount: float, ctx: ToolContext) -> str:
    return await ctx.context.payments.refund(order_id, amount)   # app deps via ctx

agent = Agent(
    model="anthropic:claude-sonnet-5-5",
    tools=[get_order, issue_refund],
    output=Resolution,                          # result.output is a Resolution
    limits=Limits(model_calls=15, deadline="2m"),
    approve=["issue_refund"],                   # human approval (Level 4; 0011 §7)
)
result = await agent.run("Refund A-1001, it arrived broken", context=AppDeps(payments=…))
```

Custom hooks are plain classes or functions:

```python
from criogen import Hook

class RedactEmails(Hook):
    async def before_model(self, state, ctx):
        return {"messages": [redact(m) for m in state.messages[-1:]]}
```

## Level 3: workflows

A workflow is an explicit graph of nodes. Nodes can be plain functions,
agents, or other workflows.

```python
from criogen import Workflow, State, Messages, END

class Ticket(State):                 # Pydantic-based; fields become state keys
    text: str
    category: str | None = None
    messages: Messages = []          # Messages ⇒ upsert_by_id reducer (0005 §3)
    reply: str | None = None

wf = Workflow(Ticket)

@wf.node
async def classify(t: Ticket) -> dict:
    label = await gw.chat("openai:gpt-…-mini", f"Classify: {t.text}. One word: billing|tech|other")
    return {"category": label.text.strip().lower()}

wf.add_node("billing", billing_agent)              # an Agent as a step (reads/writes `messages`)
wf.add_node("tech", tech_agent)
wf.add_node("fallback", lambda t: {"reply": "A human will contact you."})

wf.start("classify")
wf.route("classify", lambda t: t.category,      # router: returns a label
         {"billing": "billing", "tech": "tech", "other": "fallback"})
for name in ("billing", "tech", "fallback"):
    wf.edge(name, END)

app = wf.compile()                  # validates; raises WorkflowBuildError listing every issue
result = await app.run(Ticket(text="I was charged twice"))
print(result.state.reply, result.steps)
```

```ts
import { Workflow, messages, END } from "criogen";
import { z } from "zod";

const Ticket = z.object({
  text: z.string(),
  category: z.string().optional(),
  messages: messages(),                    // upsert_by_id reducer
  reply: z.string().optional(),
});

const wf = new Workflow(Ticket)
  .node("classify", async (t) => ({ category: await classify(t.text) }))
  .addNode("billing", billingAgent)
  .addNode("tech", techAgent)
  .addNode("fallback", () => ({ reply: "A human will contact you." }))
  .start("classify")
  .route("classify", (t) => t.category ?? "other", { billing: "billing", tech: "tech", other: "fallback" })
  .edge("billing", END).edge("tech", END).edge("fallback", END);

const app = wf.compile();
const result = await app.run({ text: "I was charged twice" });
```

Parallel branches and joins:

```python
wf.edge("start", "search_web")
wf.edge("start", "search_docs")             # both run in the same step, concurrently
wf.join(["search_web", "search_docs"], "write_answer")   # waits for both, even if lengths differ
```

Sync Python functions are accepted anywhere a node or router is expected; the
SDK adapts them. Blocking work should be `async`, or it will hold up its superstep.

## Level 4: conversations, approvals, streaming (threads)

One parameter, `storage=`, turns on conversations, approvals that survive
restarts, history and memory. All interactive features are methods on a
**thread** ([0008](0008-persistence-and-memory.md) §2).

```python
agent = Agent(model=…, tools=[get_order, issue_refund],
              hooks=[Approve(tools=["issue_refund"])],
              storage="sqlite://support.db")          # or postgres://…

chat = agent.thread("user-42")
reply = await chat.send("Order A-1001 arrived broken, please refund")

if reply.needs_approval:
    for req in reply.approvals:
        print(req.tool, req.args)            # issue_refund {'order_id': 'A-1001', 'amount': 49.0}
    reply = await chat.approve()             # or chat.reject("…"), chat.edit(req.id, args={…})

print(reply.text)

async for event in chat.stream("Thanks! Anything else I should know?"):
    if event.type == "message_delta":
        print(event.text, end="")

await chat.history()          # checkpoints, newest first
await chat.rewind(cp_id)      # go back; chat.fork(cp_id) to branch
```

```ts
const agent = new Agent({ model, tools, hooks: [approve({ tools: ["issue_refund"] })],
                          storage: "sqlite://./support.db" });
const chat = agent.thread("user-42");
let reply = await chat.send("Order A-1001 arrived broken, please refund");
if (reply.needsApproval) reply = await chat.approve();
for await (const e of chat.stream("Anything else?")) if (e.type === "message_delta") process.stdout.write(e.text);
```

Approval requests are stored. A different process (an admin dashboard,
a webhook) can call `agent.thread("user-42").approve()` hours later.

Without `storage=`, threads still work in memory for the life of the process.

Workflows get the same API: `app = wf.compile(storage=…)`, then
`app.thread(id).send(…)`, and `thread.resume(value)` for custom pauses.

## Level 5: long-term memory

```python
agent = Agent(
    model=…, storage="postgres://…",
    memory=Memory(namespace=lambda ctx: ("users", ctx.context.user_id),
                  recall=5, tools=True),     # recalls before each call; gives remember/forget tools
)
await agent.storage.memory.put(("users", "42"), "diet", {"value": "vegetarian"})
```

## Errors as users see them

```python
from criogen.errors import ModelError, RateLimitError, LimitExceededError, ToolExecutionError

try:
    result = await agent.run(question)
except RateLimitError as e:          # after automatic retries (0003 §5)
    retry_later(e.retry_after)
except LimitExceededError as e:      # e.limit == "model_calls"
    ...
except ToolExecutionError as e:      # a bug in a tool; e.__cause__ is the original exception
    log.exception(e)
    resume_after_fix(e.snapshot)     # finished tool calls are not re-run (0005 §5)
```

```ts
try { await agent.run(q); }
catch (e) {
  if (isCriogenError(e, "limit_exceeded")) { /* e.details.limit */ }
  else throw e;
}
```

## What runs where

| User writes | Runs in | Implemented by |
|---|---|---|
| `@tool` functions, nodes, routers, hooks | Python / Node (user's interpreter) | bridge calls into them (architecture §5) |
| Agent loop, routing, limits, retries, timeouts | Rust | 0004, 0005 |
| Graph validation, scheduling, state merging | Rust | 0001, 0005 |
| Tool-argument and output validation | Rust (JSON Schema) + Pydantic/Zod | 0002 §5.1 |
| Model calls, streaming, error classification | Rust | `criogen-providers` (0006) |
| Checkpoints, threads, memory store (SQLite/Postgres) | Rust | 0008 |

## Resolved API decisions (2026-10-11)

1. **Naming:** `node` (not `step`, which already means a superstep in errors
   and traces), `start` for the entry node, `route` for a router, plus `edge`
   and `join`. The core keeps `entry` internally.
2. **Decorator and builder:** both, one model. `@wf.node` is shorthand for
   `wf.add_node(name, fn)`; TS uses chained builder methods.
3. **State in TS:** any [Standard Schema](https://standardschema.dev) library
   (Zod is documented by default; Valibot and ArkType also work), or plain
   TypeScript types via a generic (`new Workflow<Ticket>()`) without runtime
   validation.
4. **Sync API in Python:** `run_sync`, `send_sync`, `stream_sync`, as thin
   wrappers for scripts and notebooks.
