# criogen

**The fast AI agent framework for Python.** Build agents that call tools, wait for a
person's approval, remember users, and survive crashes, on a Rust engine that adds
almost nothing to each step. A LangChain and LangGraph alternative with one package
and no dependency tree.

```sh
pip install criogen
```

Python 3.9+ · Linux (x64, ARM64) · macOS (Apple Silicon, Intel) · Windows (x64) ·
free to use, also commercially

[Website](https://criogen.vercel.app) ·
[Docs](https://criogen.vercel.app/docs/) ·
[Quickstart](https://criogen.vercel.app/docs/quickstart/) ·
[Benchmarks](https://criogen.vercel.app/benchmarks/) ·
[npm package](https://www.npmjs.com/package/criogen)

## Why Criogen

The scheduling, tool-call checks, retries and saving run in compiled Rust. Your
agents, tools and workflows stay plain Python: `async def` functions, type hints,
Pydantic models.

Measured on one machine (AMD Ryzen 5 4600H, Windows 11, 2026-10-21), all three
frameworks from Python against the same simulated model, so only framework cost is
counted:

| | Criogen | LangGraph | CrewAI |
|---|---|---|---|
| Framework overhead per agent turn | **0.29 ms** | 1.23 ms | 33.9 ms |
| 1,000 agent runs at once | **333 ms** | 2.85 s | 49 s |
| Packages installed | **1** | 42 | 138 |
| Install size | **17.9 MB** | 42.9 MB | 742.7 MB |
| Import time | **148 ms** | 1.70 s | 9.30 s |
| Reliability tests passed with default settings | **5 of 5** | 2 of 5 | 4 of 5 |

With one agent and a real model, the model's own time dominates and the frameworks
are close. The difference shows with many steps, many tool calls and many users at
once. The [benchmark page](https://criogen.vercel.app/benchmarks/) has every
scenario and a kit to rerun them on your machine.

## Quickstart

```python
import asyncio
from criogen import Agent, tool, ToolError

@tool
async def get_order(order_id: str) -> dict:
    """Look up an order by id."""
    if order_id != "A-1":
        raise ToolError(f"No order {order_id}")   # the model sees this and can recover
    return {"id": order_id, "status": "shipped"}

agent = Agent("openai:gpt-5-mini", tools=[get_order], storage="sqlite://support.db")

async def main():
    chat = agent.thread("user-42")                # a saved conversation
    reply = await chat.send("Where is order A-1?")
    print(reply.text)

asyncio.run(main())
```

Keep keys in a `.env` file: `from criogen import load_env; load_env()` reads
`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY` and `OPENROUTER_API_KEY`. It
never prints values and never overrides a variable that is already set.

## What you get

- **Tools from type hints.** `@tool` builds the JSON Schema from the signature and
  checks every argument before your function runs. Sync tools run on a worker thread,
  async tools on your event loop.
- **Typed output.** `output=MyPydanticModel` and `result.output` is that model.
- **Human approval.** `approve=["issue_refund"]` pauses before the call. The request
  waits in the database; anyone can approve, reject or edit it later, from another
  process.
- **Durable threads.** Every step is saved. After a crash, `thread.retry()` continues
  where it stopped. Threads also have `history`, `rewind` and `delete`.
- **Streaming.** `thread.stream(...)` yields text, tool and approval events. Leaving the
  loop cancels the run and still saves it.
- **Memory.** `Memory.by_state("user_id", "users")` recalls facts per user before each
  model call. Search by keyword, or by meaning with an embedder (OpenAI or Ollama).
- **Workflows.** Graphs of plain functions and agents with routes, joins, loops and
  pauses (`suspend`) that wait for a person, for days if needed.
- **Multi-agent.** `agent.as_tool(...)` turns an agent into another agent's tool.
  `criogen.patterns` has 12 ready-made designs (sequential, parallel, review loops and
  more), and `criogen.specialists` has agent recipes you can extend.
- **Resilience.** Retries with `Retry-After`, fallback models, limits on model calls,
  tool calls and tokens, and tool timeouts.
- **Storage.** SQLite or Postgres, encryption at rest (AES-256-GCM), retention rules,
  and a tamper-evident audit trail that records who did what, without the content.
- **Observability.** Hooks around model and tool calls, OpenTelemetry export and a
  Langfuse preset. Content is left out unless you turn it on.
- **Any model.** OpenAI, Anthropic, Gemini, DeepSeek, Mistral, xAI, Groq, Cerebras,
  Together, Fireworks, Qwen, Kimi (Moonshot), GLM (Z.ai) and OpenRouter by name, any
  OpenAI- or Anthropic-compatible server by URL, and self-hosted Ollama, vLLM or
  LM Studio, so data can stay on your own servers. `Gateway` makes plain chat, stream
  and embedding calls with the same retries and fallbacks.

## Approval before a tool runs

```python
from criogen import Agent, Storage

agent = Agent(
    "openai:gpt-5-mini",
    tools=[issue_refund],
    approve=["issue_refund"],               # a person decides before it runs
    storage=Storage("sqlite://support.db"),
)

chat = agent.thread("ticket-7", actor="user:ada")
reply = await chat.send("Order A-1001 arrived broken, please refund it")
if reply.needs_approval:
    for request in reply.approvals:
        print(request.tool, request.args)

# Hours later, in another process: the approval waited in the database.
reply = await agent.thread("ticket-7", actor="user:manager").approve()
```

## Typed output and limits

```python
from typing import Literal
from pydantic import BaseModel
from criogen import Agent

class Resolution(BaseModel):
    category: Literal["billing", "shipping", "other"]
    refund: bool

agent = Agent(
    "openai:gpt-5-mini",
    instructions="You are a support agent.",
    tools=[issue_refund],
    output=Resolution,                       # result.output is a Resolution
    limits={"model_calls": 10, "tool_calls": 20},
)
```

## Streaming

```python
stream = agent.thread("user-42").stream("Summarise my last order")
async for event in stream:
    if event.type == "text":
        print(event.text, end="")
    elif event.type == "tool_call_started":
        print(f"\n[{event.tool}…]")
reply = await stream.reply()                 # saved, like send()
```

## Workflows

```python
from criogen import END, NodeContext, Storage, Workflow, suspend

wf = Workflow()

@wf.node
def classify(state: dict) -> dict:
    return {"category": "billing" if "charge" in state["text"] else "other"}

@wf.node
def approve(state: dict, ctx: NodeContext) -> object:
    if ctx.resume is None:
        return suspend({"refund": state["amount"]})   # pause for a person
    return {"approved": ctx.resume == "yes"}

@wf.node
def answer(state: dict) -> dict:
    return {"reply": "A human will follow up."}

wf.start("classify")
wf.route("classify", lambda s: s["category"], {"billing": "approve", "other": "answer"})
wf.edge("approve", END).edge("answer", END)
app = wf.compile(storage=Storage("sqlite://app.db"))

turn = await app.thread("ticket-7").run({"text": "I was charged twice", "amount": 40})
```

## Models

```python
from criogen import Agent, Anthropic, Gemini, Groq, Ollama, OpenAICompatible, Retry

Agent("openai:gpt-5-mini")                                   # OPENAI_API_KEY
Agent(Anthropic("claude-sonnet-5-5"))                        # ANTHROPIC_API_KEY
Agent(Gemini("gemini-2.5-flash"))                            # GEMINI_API_KEY
Agent("deepseek:deepseek-chat")                              # DEEPSEEK_API_KEY
Agent(Groq("llama-3.3-70b-versatile"))                       # GROQ_API_KEY
Agent(Ollama("llama3.2"))                                    # self-hosted
Agent(OpenAICompatible("qwen3", base_url="http://gpu-box:8000/v1"))   # vLLM, LM Studio

# Rate limits, overload and outages go to the next model.
Agent("openai:gpt-5-mini", fallbacks=[Ollama("llama3.2")], model_retry=Retry(max_attempts=4))
```

## Testing without a model

```python
from criogen import Agent, FakeModel, FakeReply

agent = Agent(
    FakeModel([FakeReply.tool_call("get_order", {"order_id": "A-1"}), "It has shipped."]),
    tools=[get_order],
)
assert agent.run_sync("Where is A-1?").text == "It has shipped."
```

Scripted replies, no network, the same answer every time.

## Also on npm

The same engine and the same features for TypeScript and Node.js:
`npm install criogen` ([npm](https://www.npmjs.com/package/criogen)).

## Licence

Free to use, also commercially, under the Criogen Free Licence (see `LICENSE`).
