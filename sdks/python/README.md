# agentbuild-sdk (Python)

```sh
pip install agentbuild-sdk
```

Agents and workflows with tools, human approval, durable threads, long-term memory, an
audit trail and encryption at rest, on [AgentBuild](https://agentbuild.vercel.app)'s
Rust engine (design 0011).

```python
import asyncio
from agentbuild import Agent, tool, ToolError

@tool
async def get_order(order_id: str) -> dict:
    """Look up an order by id."""
    if order_id != "A-1":
        raise ToolError(f"No order {order_id}")
    return {"id": order_id, "status": "shipped"}

agent = Agent("openai:gpt-5-mini", tools=[get_order], storage="sqlite:///support.db")

async def main():
    chat = agent.thread("user-42")
    reply = await chat.send("Where is order A-1?")
    print(reply.text)

asyncio.run(main())
```

**Status:** first slice: agents, tools, approvals, typed output, threads
(send, approve, reject, decide, retry, history, rewind, delete, stream),
storage (SQLite, Postgres, encryption, audit, retention) and memory.
Workflows too (`Workflow`, design 0015), and from design 0017: agents as
tools, retries and fallback models, memory by meaning, tracing and hooks,
and `Gateway` for model calls without an agent (design 0018). Tests use `FakeModel`; no real model is called.

Free to use, also commercially (see `LICENSE`).
