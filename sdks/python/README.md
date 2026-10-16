# agentbuild (Python)

Agents with tools, human approval, durable threads, long-term memory, an
audit trail and encryption at rest, on [AgentBuild](https://github.com/VARMAMASTER/agentbuild)'s
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
Python workflows, hooks and the gateway are planned. Tests use
`FakeModel`; no real model is called.

Build from source: `pip install maturin && maturin develop` in this
directory (needs Rust 1.88+).
