# 0012: TypeScript SDK (first slice)

Status: **Accepted** (task 6 of the roadmap in `docs/progress.md`; the user
gave full design freedom). The TypeScript side of
[0007](0007-public-api.md) Levels 1, 2, 4 and 5, on the Rust engine: the
same slice as the Python SDK ([0011](0011-python-sdk.md)), so both SDKs
offer the same things under each language's idioms.

## 1. Shape

```
sdks/typescript/
  Cargo.toml        crate `agentbuild-node` (cdylib, not a workspace member)
  src/lib.rs        the bridge: napi-rs 3, napi's Tokio runtime
  lib/*.ts          the TypeScript API, compiled to dist/ (CommonJS + .d.ts)
  native/           the built addon, agentbuild.node (not in git)
  test/*.test.js    node:test, simulated models only
```

- **napi-rs 3** (N-API 6: Node 18+). No `@napi-rs/cli`: cargo builds the
  addon and `scripts/copy-native.js` copies it to `native/agentbuild.node`.
  Prebuilt per-platform packages come with the release workflow later.
- **Rule 3 holds**: the agent loop, validation, retries, storage and
  approvals run in Rust. TypeScript builds the configuration and holds
  the user's tools.
- Not a workspace member: building it needs Node. `scripts/ci-local.sh`
  gains a `typescript` step (skipped, and said so, without Node 18+).

## 2. The API

```ts
import { Agent, tool, ToolError, Storage, Memory, Decision } from "@agentbuild/sdk";

const getOrder = tool({
  name: "get_order",
  description: "Look up an order by id.",
  input: { type: "object", properties: { orderId: { type: "string" } }, required: ["orderId"] },
  run: async ({ orderId }, ctx) => { … },          // throw ToolError(...) for the model to see
});

const agent = new Agent({
  model: "openai:gpt-5-mini",                       // or openai("…", { apiKey }), FakeModel([...])
  instructions: "You are a support agent.",
  tools: [getOrder],
  approve: ["issue_refund"],
  output: resolutionSchema,                         // JSON Schema → result.output
  storage: "sqlite:///support.db",                  // or new Storage({ url, encryptionKeys, audit, retention })
  memory: Memory.byState("user_id", "users"),
});

const result = await agent.run("Where is A-1?", { state: { user_id: "ada" }, signal });
const chat = agent.thread("user-42", { actor: "user:ada" });
let reply = await chat.send("Refund A-1");
if (reply.needsApproval) reply = await chat.approve();   // reject, decide({ [callId]: Decision.edit(...) })
for await (const event of chat.stream("Anything else?")) if (event.type === "text") process.stdout.write(event.text);
```

Names follow TypeScript conventions (`needsApproval`, `toolCalls`,
`checkpointId`); approval requests and stored messages keep the engine's
JSON (`call_id`), the same JSON every language sees.

**Tool input** is JSON Schema in this slice; the engine validates the
model's arguments before `run` is called. Standard Schema (Zod, Valibot)
comes later (0007 decision 3).

**Cancellation**: every awaitable call takes an `AbortSignal`. Aborting
fires the run's `CancelToken`: the run fails with `cancelled`, a thread is
still saved, and `retry` continues it. Leaving a `for await` early, or
`stream.cancel()`, does the same for streams, also while another task is
reading the stream (the stream's token is held apart from the stream:
`ThreadStream::cancel_token`).

## 3. Errors

`AgentBuildError` with `code` (the engine's stable code, 0003), `message`
and `details`; subclasses `ModelError`, `ToolExecutionError` (its `cause`
is what the tool threw), `LimitExceededError`, `ThreadBusyError`,
`ThreadSuspendedError`, `StorageError`, `ValidationError`,
`CancelledError`. `isAgentBuildError(e, code?)` checks without
`instanceof` across copies of the package.

## 4. The bridge

- Async methods are napi `async fn`s on napi's Tokio runtime and return
  Promises; nothing blocks Node's thread except opening storage (the
  `Storage` constructor; SQLite opens a file, Postgres connects).
- **Tools** are JavaScript functions passed as weak thread-safe functions
  (an idle agent does not keep Node running). Every tool is wrapped as
  `(requestJson) => Promise<envelopeJson>`; Rust awaits the Promise. A
  `ToolError` becomes a tool error the model sees; anything else thrown
  fails the run with `tool_failed`, and the TypeScript side attaches the
  thrown value as the error's `cause` (kept by call id).
- **Errors** cross as the error message `{"code","message","details"}`,
  turned into the classes of §3.
- **Values** cross as JSON, as in the Python SDK.

## 5. Tests and packaging (rule 9)

- `sdks/typescript/test`: `node --test` with `FakeModel` (rule 7): tools
  (sync, async, `ToolError`, crashes with `cause`, engine validation),
  typed output, threads with approval across agent instances, SQLite
  storage surviving a new `Agent`, memory, retention, audit, encryption,
  streaming, `AbortSignal` (before and during a call), cancelling a stream
  that is being read, errors by code.
- `scripts/test-typescript.sh`: builds the release addon and `dist/`,
  `npm pack`s the package, installs the tarball into a **new** project,
  runs `examples/consumer.mjs` (public API only) and the test suite
  against the installed package.
- The manual live check gets a TypeScript run against OpenAI and Gemini
  (`examples/live.mjs`), never in CI.

## 6. Not in this slice

Workflows (Level 3), hooks, the gateway, Standard Schema tool input,
semantic memory, prebuilt binaries for other platforms.
