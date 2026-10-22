# @agentbuild/sdk

Agents with tools, human approval, durable threads, long-term memory, an
audit trail and encryption at rest, on [AgentBuild](https://agentbuild.vercel.app)'s
Rust engine (design 0012).

```ts
import { Agent, tool, ToolError } from "@agentbuild/sdk";

const getOrder = tool({
  name: "get_order",
  description: "Look up an order by id.",
  input: { type: "object", properties: { orderId: { type: "string" } }, required: ["orderId"] },
  run: async ({ orderId }) => {
    if (orderId !== "A-1") throw new ToolError(`No order ${orderId}`);
    return { id: orderId, status: "shipped" };
  },
});

const agent = new Agent({ model: "openai:gpt-5-mini", tools: [getOrder], storage: "sqlite:///support.db" });
const chat = agent.thread("user-42");
const reply = await chat.send("Where is order A-1?");
console.log(reply.text);
```

**Status:** first slice: agents, tools (JSON Schema input), approvals, typed
output, threads (send, approve, reject, decide, retry, history, rewind,
delete, stream, `AbortSignal`), storage (SQLite, Postgres, encryption,
audit, retention) and memory. Workflows (design 0015), and from design
0017: agents as tools, retries and fallback models, memory by meaning,
tracing, hooks, and tool input from Zod or any Standard Schema. Prebuilt
binaries for every platform come with the first release. Tests use
`FakeModel`; no real model is called.

Build from source (needs Rust 1.88+ and Node 18+): `npm install`,
`npm run build:native`, `npm run build`.
