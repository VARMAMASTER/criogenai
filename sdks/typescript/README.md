# criogen

**The fast AI agent framework for TypeScript and Node.js**, a LangChain and
LangGraph alternative: agents with tool calling, multi-agent workflows, human
approval, durable threads, long-term memory, an audit trail and encryption at
rest, on [Criogen](https://criogen.vercel.app)'s Rust engine. In
[our benchmark](https://criogen.vercel.app/benchmarks/) (run from Python) an agent turn costs
0.29 ms of framework overhead, against 1.23 ms for LangGraph and 34 ms for CrewAI.

> `npm install criogen` (or `pnpm add criogen`, `yarn add criogen`, `bun add criogen`).
> The engine for your platform installs with it: Linux (x64, ARM64), macOS (Apple
> Silicon, Intel) and Windows (x64); Node 18 or newer.

```ts
import { Agent, tool, ToolError } from "criogen";

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

## Keep your keys in a `.env` file

```ts
import { loadEnv } from "criogen";

loadEnv(); // reads ./.env: OPENAI_API_KEY, ANTHROPIC_API_KEY, GEMINI_API_KEY, OPENROUTER_API_KEY
```

It returns the names it set (never the values) and never replaces a variable that
is already set. A model with no key fails at once and says which variable to set.

**Status:** first slice: agents, tools (JSON Schema input), approvals, typed
output, threads (send, approve, reject, decide, retry, history, rewind,
delete, stream, `AbortSignal`), storage (SQLite, Postgres, encryption,
audit, retention) and memory. Workflows (design 0015), and from design
0017: agents as tools, retries and fallback models, memory by meaning,
tracing, hooks, tool input from Zod or any Standard Schema, and
`Gateway` for model calls without an agent (design 0018), and ready-made
patterns, extendable specialist agents and web search (`patterns`,
`specialists`, `web`; design 0019). Prebuilt
binaries for every platform come with the first release. Tests use
`FakeModel`; no real model is called.

Build from source (needs Rust 1.88+ and Node 18+): `npm install`,
`npm run build:native`, `npm run build`.
