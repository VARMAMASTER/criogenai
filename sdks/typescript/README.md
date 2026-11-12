# criogenai

**The fast AI agent framework for TypeScript and Node.js.** Build agents that call
tools, wait for a person's approval, remember users, and survive crashes, on a Rust
engine that adds almost nothing to each step. A LangChain and LangGraph alternative
with one package and no dependency tree.

```sh
npm install criogenai        # or: pnpm add criogenai · yarn add criogenai · bun add criogenai
```

Node 18+ · Linux (x64, ARM64) · macOS (Apple Silicon, Intel) · Windows (x64) ·
serverless (Vercel, AWS Lambda) · free to use, also commercially

[Website](https://criogenai.com) ·
[Docs](https://criogenai.com/docs/) ·
[Quickstart](https://criogenai.com/docs/quickstart/) ·
[Benchmarks](https://criogenai.com/benchmarks/) ·
[PyPI package](https://pypi.org/project/criogenai/)

## Why CriogenAI

The scheduling, tool-call checks, retries and saving run in compiled Rust; the
engine for your platform installs with the package, with no build step. Your agents,
tools and workflows stay plain TypeScript: async functions, JSON Schema or Zod.

Measured on one machine (AMD Ryzen 5 4600H, Windows 11, 2026-10-21). The comparison
runs from Python, where all three frameworks exist, against the same simulated model,
so only framework cost is counted:

| | CriogenAI | LangGraph | CrewAI |
|---|---|---|---|
| Framework overhead per agent turn | **0.29 ms** | 1.23 ms | 33.9 ms |
| 1,000 agent runs at once | **333 ms** | 2.85 s | 49 s |
| Reliability tests passed with default settings | **5 of 5** | 2 of 5 | 4 of 5 |

With one agent and a real model, the model's own time dominates and the frameworks
are close. The difference shows with many steps, many tool calls and many users at
once. The [benchmark page](https://criogenai.com/benchmarks/) has every
scenario and a kit to rerun them.

## Quickstart

```ts
import { Agent, tool, ToolError } from "criogenai";

const getOrder = tool({
  name: "get_order",
  description: "Look up an order by id.",
  input: { type: "object", properties: { orderId: { type: "string" } }, required: ["orderId"] },
  run: async ({ orderId }) => {
    if (orderId !== "A-1") throw new ToolError(`No order ${orderId}`); // the model sees this
    return { id: orderId, status: "shipped" };
  },
});

const agent = new Agent({ model: "openai:gpt-5-mini", tools: [getOrder], storage: "sqlite://support.db" });
const chat = agent.thread("user-42"); // a saved conversation
const reply = await chat.send("Where is order A-1?");
console.log(reply.text);
```

Keep keys in a `.env` file: `import { loadEnv } from "criogenai"; loadEnv();` reads
`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY` and `OPENROUTER_API_KEY`. It
never prints values and never overrides a variable that is already set.

## What you get

- **Tools with checked input.** JSON Schema, Zod or any Standard Schema. Every argument
  is checked before your function runs, and the problems go back to the model.
- **Typed output.** `new Agent<MyType>({ output: schema })` and `result.output` is typed.
- **Human approval.** `approve: ["issue_refund"]` pauses before the call. The request
  waits in the database; anyone can approve, reject or edit it later, from another
  process.
- **Durable threads.** Every step is saved. After a crash, `thread.retry()` continues
  where it stopped. Threads also have `history`, `rewind` and `delete`.
- **Streaming.** `thread.stream(...)` yields text, tool and approval events; there is a
  Next.js route and React hook example for streaming to the browser. `AbortSignal`
  works on every call.
- **Memory.** `Memory.byState("user_id", "users")` recalls facts per user before each
  model call. Search by keyword, or by meaning with an embedder (OpenAI or Ollama).
- **Workflows.** Graphs of plain functions and agents with routes, joins, loops and
  pauses (`suspend`) that wait for a person, for days if needed.
- **Multi-agent.** `agent.asTool(...)` turns an agent into another agent's tool.
  `patterns` has 12 ready-made designs (sequential, parallel, review loops and more),
  and `specialists` has agent recipes you can extend.
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

```ts
import { Agent, Storage } from "criogenai";

const agent = new Agent({
  model: "openai:gpt-5-mini",
  tools: [issueRefund],
  approve: ["issue_refund"], // a person decides before it runs
  storage: new Storage("sqlite://support.db"),
});

const chat = agent.thread("ticket-7", { actor: "user:ada" });
let reply = await chat.send("Order A-1001 arrived broken, please refund it");
if (reply.needsApproval) {
  for (const request of reply.approvals) console.log(request.tool, request.args);
}

// Hours later, in another process: the approval waited in the database.
reply = await agent.thread("ticket-7", { actor: "user:manager" }).approve();
```

## Tool input from Zod

```ts
import { z } from "zod";
import { tool } from "criogenai";

const refund = tool({
  name: "issue_refund",
  description: "Refund an order.",
  input: z
    .object({ orderId: z.string(), amount: z.number().positive() })
    .refine((r) => r.amount <= 500, { message: "over 500 needs a manager", path: ["amount"] }),
  run: async ({ orderId, amount }) => ({ refunded: amount, orderId }),
});
```

## Streaming

```ts
const stream = agent.thread("user-42").stream("Summarise my last order");
for await (const event of stream) {
  if (event.type === "text") process.stdout.write(event.text);
  else if (event.type === "tool_call_started") console.log(`\n[${event.tool}…]`);
}
const reply = await stream.reply(); // saved, like send()
```

## Workflows

```ts
import { END, Storage, Workflow, suspend } from "criogenai";

const app = new Workflow()
  .node("classify", (state) => ({ category: String(state.text).includes("charge") ? "billing" : "other" }))
  .node("approve", (state, ctx) =>
    ctx.resume === undefined ? suspend({ refund: state.amount }) : { approved: ctx.resume === "yes" },
  )
  .node("answer", () => ({ reply: "A human will follow up." }))
  .start("classify")
  .route("classify", (state) => String(state.category), { billing: "approve", other: "answer" })
  .edge("approve", END)
  .edge("answer", END)
  .compile({ storage: new Storage({ url: "sqlite://app.db" }) });

const turn = await app.thread("ticket-7").run({ text: "I was charged twice", amount: 40 });
```

## Models

```ts
import { Agent, anthropic, groq, ollama, openaiCompatible } from "criogenai";

new Agent({ model: "openai:gpt-5-mini" }); // OPENAI_API_KEY
new Agent({ model: anthropic("claude-sonnet-5-5") }); // ANTHROPIC_API_KEY
new Agent({ model: "deepseek:deepseek-chat" }); // DEEPSEEK_API_KEY
new Agent({ model: groq("llama-3.3-70b-versatile") }); // GROQ_API_KEY
new Agent({ model: ollama("llama3.2") }); // self-hosted
new Agent({ model: openaiCompatible("qwen3", { baseUrl: "http://gpu-box:8000/v1" }) }); // vLLM, LM Studio

// Rate limits, overload and outages go to the next model.
new Agent({ model: "openai:gpt-5-mini", fallbacks: [ollama("llama3.2")], modelRetry: { maxAttempts: 4 } });
```

## Testing without a model

```ts
import { Agent, FakeModel, FakeReply } from "criogenai";

const testAgent = new Agent({
  model: new FakeModel([FakeReply.toolCall("get_order", { orderId: "A-1" }), "It has shipped."]),
  tools: [getOrder],
});
const result = await testAgent.run("Where is A-1?"); // result.text === "It has shipped."
```

Scripted replies, no network, the same answer every time.

## In the browser

The same engine, compiled to WebAssembly, behind the same API: agents run in
a page or a Web Worker.

```ts
import { Agent, openai, ready } from "criogenai/web";

await ready(); // loads the engine once (about 550 KB compressed)
const agent = new Agent({ model: openai("gpt-5-mini", { apiKey: theUsersOwnKey }), tools: [weather] });
const result = await agent.run("Picnic weather in Lisbon?");
```

Storage is in the tab's memory; databases, encryption at rest, telemetry
export and web tools need a server (they fail with `unsupported`). A key in
a page is visible to whoever uses it: in a product, point models at your own
server or a CriogenAI gateway. `FunctionModel` plugs in a model written in
JavaScript, such as one running in the page.
[Guide](https://criogenai.com/docs/guides/browser/).

## Also on PyPI

The same engine and the same features for Python: `pip install criogenai`
([PyPI](https://pypi.org/project/criogenai/)).

## Build from source

Needs Rust 1.88+ and Node 18+: `npm install`, `npm run build:native`, `npm run build`.

## Licence

Open source under MIT or Apache-2.0, at your option (see `LICENSE-MIT` and `LICENSE-APACHE`). Source: https://github.com/VARMAMASTER/criogenai
