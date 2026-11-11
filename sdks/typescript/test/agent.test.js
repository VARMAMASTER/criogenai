// The TypeScript SDK (design 0012) with simulated models only: runs, tools,
// typed output, threads, approvals, storage, streaming, cancellation, memory,
// errors. Runs against the source build, or the installed package when
// CRIOGEN_SDK names it (scripts/test-typescript.sh).

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const crypto = require("node:crypto");

// CRIOGEN_ENGINE=wasm runs this suite on the browser's engine (design 0021);
// what needs a server is skipped there, with the reason.
const serverOnly = (why) => (process.env.CRIOGEN_ENGINE === "wasm" ? `server only: ${why}` : false);

const sdk = require(process.env.CRIOGEN_SDK || path.join(__dirname, ".."));
const {
  Agent,
  Embedder,
  Telemetry,
  CriogenError,
  CancelledError,
  Decision,
  FakeModel,
  FakeReply,
  LimitExceededError,
  Memory,
  Storage,
  StorageError,
  ThreadSuspendedError,
  ToolError,
  ToolExecutionError,
  ToolResult,
  ValidationError,
  isCriogenError,
  tool,
} = sdk;

const add = tool({
  name: "add",
  description: "Adds two numbers.",
  input: {
    type: "object",
    properties: { a: { type: "integer" }, b: { type: "integer" } },
    required: ["a", "b"],
    additionalProperties: false,
  },
  run: ({ a, b }) => a + b,
});

const double = tool({
  name: "double",
  description: "Doubles a number.",
  input: { type: "object", properties: { n: { type: "integer" } }, required: ["n"] },
  run: async ({ n }, ctx) => {
    await new Promise((r) => setTimeout(r, 10));
    return { value: n * 2, callId: ctx.callId };
  },
});

const toolResults = (result) => result.messages.filter((m) => m.role === "tool");

function tempDb(name) {
  const file = path.join(os.tmpdir(), `criogen-ts-${name}-${crypto.randomUUID()}.db`);
  return {
    url: `sqlite:///${file}`,
    file,
    // Best effort: on Windows the file stays locked while the storage is
    // open, and Storage has no close() yet.
    remove() {
      for (const suffix of ["", "-wal", "-shm"]) {
        try {
          fs.rmSync(file + suffix, { force: true });
        } catch {}
      }
    },
  };
}

test("sync and async tools run and their results reach the model", async () => {
  const agent = new Agent({
    model: new FakeModel([FakeReply.toolCalls([["add", { a: 2, b: 3 }], ["double", { n: 4 }]]), "5 and 8"]),
    tools: [add, double],
  });
  const result = await agent.run("go");
  assert.equal(result.text, "5 and 8");
  assert.equal(result.toolCalls, 2);
  const results = toolResults(result);
  assert.equal(results[0].status, "success");
  assert.equal(results[0].content[0].value, 5);
  const doubled = results[1].content[0].value;
  assert.equal(doubled.value, 8);
  assert.equal(doubled.callId, results[1].call_id);
});

test("tool errors and exceptions reach the model unless told to fail", async () => {
  class Missing extends Error {}
  const lookup = tool({
    name: "lookup",
    description: "Looks up an order.",
    input: { type: "object", properties: { orderId: { type: "string" } }, required: ["orderId"] },
    run: ({ orderId }) => {
      if (orderId === "missing") throw new ToolError("No such order");
      throw new Missing(orderId);
    },
  });
  const agent = new Agent({
    model: new FakeModel([FakeReply.toolCall("lookup", { orderId: "missing" }), "Sorry, not found."]),
    tools: [lookup],
  });
  const result = await agent.run("where is it");
  assert.equal(result.text, "Sorry, not found.");
  const error = toolResults(result)[0];
  assert.equal(error.status, "error");
  assert.match(JSON.stringify(error.content), /No such order/);

  // An exception the tool did not handle reaches the model too (by default).
  const script = () => new FakeModel([FakeReply.toolCall("lookup", { orderId: "x" }), "The order system failed."]);
  const reported = await new Agent({ model: script(), tools: [lookup] }).run("go");
  assert.equal(reported.text, "The order system failed.");
  const crash = toolResults(reported)[0];
  assert.equal(crash.status, "error");
  assert.equal(crash.error.code, "tool_failed");
  assert.match(JSON.stringify(crash.content), /Error: x/);

  // "returnGeneric": the model only learns that the tool failed.
  const quiet = await new Agent({ model: script(), tools: [lookup], onToolException: "returnGeneric" }).run("go");
  const hidden = toolResults(quiet)[0];
  assert.doesNotMatch(JSON.stringify(hidden.content), /Error: x/);
  assert.equal(hidden.artifact.exception, "Error: x");

  // "fail": the run fails, with the thrown value as the cause.
  const crashing = new Agent({ model: script(), tools: [lookup], onToolException: "fail" });
  await assert.rejects(crashing.run("go"), (e) => {
    assert.ok(e instanceof ToolExecutionError);
    assert.equal(e.code, "tool_failed");
    assert.ok(e.cause instanceof Missing);
    return true;
  });
  assert.throws(() => new Agent({ model: script(), onToolException: "ignore" }), ValidationError);
});

test("an agent as a tool of another", async () => {
  const researcher = new Agent({ model: new FakeModel(["The order shipped on Monday."]), instructions: "Find facts." });
  const writer = new Agent({
    model: new FakeModel([FakeReply.toolCall("research", { input: "When did A-1 ship?" }), "It shipped on Monday."]),
    tools: [researcher.asTool("research", "Looks things up")],
  });
  const result = await writer.run("When did my order ship?");
  assert.equal(result.text, "It shipped on Monday.");
  assert.match(JSON.stringify(toolResults(result)[0].content), /Monday/);
});

test("fallback models and retry settings", async () => {
  // The main model keeps failing with a transient error: after its retries the fallback answers.
  const agent = new Agent({
    model: new FakeModel(["never"], { failFirst: 100 }),
    fallbacks: [new FakeModel(["from the fallback"])],
    modelRetry: { maxAttempts: 2, initialBackoffSecs: 0.01, jitter: false },
    maxRetryAfterSecs: 5,
  });
  assert.equal((await agent.run("hi")).text, "from the fallback");
  assert.throws(() => new Agent({ model: new FakeModel([]), fallbackOn: ["no_such_kind"] }), ValidationError);
  // A transient error once, then the answer: retried by the agent itself.
  assert.equal((await new Agent({ model: new FakeModel(["fine now"], { failFirst: 1 }) }).run("hi")).text, "fine now");
});

test("semantic memory finds facts by meaning and falls back to keywords", async () => {
  const storage = new Storage();
  const memory = Memory.shared("facts").semantic(Embedder.fake({ synonyms: { automobile: "car" } }));
  const agent = new Agent({
    model: new FakeModel([
      FakeReply.toolCall("remember", { key: "vehicle", value: "The customer drives a red car" }),
      "Noted.",
      FakeReply.toolCall("search_memory", { query: "automobile" }),
      "Found it.",
    ]),
    storage,
    memory,
  });
  assert.equal((await agent.run("Remember my car")).text, "Noted.");
  const found = await agent.run("Which automobile do I have?");
  assert.match(JSON.stringify(toolResults(found)[0].content), /red car/); // no shared word: found by meaning
  const broken = new Agent({
    model: new FakeModel([FakeReply.toolCall("search_memory", { query: "red" }), "ok"]),
    storage,
    memory: Memory.shared("facts").semantic(Embedder.fake({ failing: true })),
  });
  assert.match(JSON.stringify(toolResults(await broken.run("search"))[0].content), /red car/);
});

test("Langfuse with one key is an error, not the environment", { skip: serverOnly("telemetry export") }, () => {
  assert.throws(() => Telemetry.langfuse({ publicKey: "pk-lf-1" }), (e) => e instanceof ValidationError && /secretKey|secret_key/.test(e.message));
  assert.throws(() => Telemetry.langfuse({ secretKey: "sk-lf-1" }), (e) => e instanceof ValidationError && /publicKey|public_key/.test(e.message));
});

test("telemetry exports runs without their content", { skip: serverOnly("telemetry export") }, async () => {
  const http = require("node:http");
  const received = [];
  const server = http.createServer((req, res) => {
    let body = "";
    req.on("data", (chunk) => (body += chunk));
    req.on("end", () => {
      received.push(body);
      res.writeHead(200).end();
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  try {
    const telemetry = Telemetry.otlp(`http://127.0.0.1:${server.address().port}/v1/traces`, { serviceName: "test" });
    const agent = new Agent({ model: new FakeModel(["fine"]), telemetry });
    await agent.run("a private question");
    await telemetry.flush();
    const body = received.join("");
    assert.match(body, /resourceSpans/);
    assert.doesNotMatch(body, /a private question/); // content is left out by default
    assert.ok(telemetry.stats().exported > 0);
    await telemetry.shutdown();
  } finally {
    server.close();
  }
});

test("hooks change what they may, in order", async () => {
  const seen = [];
  const lookup = tool({
    name: "lookup",
    description: "Looks up an order.",
    input: { type: "object", properties: { orderId: { type: "string" } }, required: ["orderId"] },
    run: ({ orderId }) => {
      seen.push(orderId);
      return `order ${orderId}: shipped, card 4111-1111`;
    },
  });
  const weather = tool({
    name: "weather",
    description: "The weather.",
    input: { type: "object", properties: { city: { type: "string" } } },
    run: () => {
      throw new Error("skipped by the hook");
    },
  });
  class Guard {
    onStart(ctx) {
      return { state: { runs: (ctx.hookState?.runs ?? 0) + 1 } };
    }
    async beforeTool(call) {
      if (call.name === "weather") return new ToolResult("sunny (cached)");
      return { ...call, args: { orderId: call.args.orderId.toUpperCase() } };
    }
    afterTool(call, result) {
      return typeof result === "string" ? result.replace("4111-1111", "[card]") : undefined;
    }
    afterModel(ctx) {
      const last = ctx.messages[ctx.messages.length - 1];
      if (JSON.stringify(last.content).includes("secret")) {
        return { message: { ...last, content: [{ type: "text", text: "[redacted]" }] } };
      }
    }
  }
  const ended = [];
  const audit = { name: "audit", onEnd: async (ctx, outcome) => ended.push([outcome, ctx.modelCalls]) };
  const agent = new Agent({
    model: new FakeModel([
      FakeReply.toolCalls([
        ["lookup", { orderId: "a-1" }],
        ["weather", { city: "Paris" }],
      ]),
      "The secret answer",
    ]),
    tools: [lookup, weather],
    hooks: [new Guard(), audit],
  });
  const result = await agent.run("where is it?");
  assert.deepEqual(seen, ["A-1"]); // beforeTool changed the argument
  const results = toolResults(result).map((m) => JSON.stringify(m.content));
  assert.match(results[0], /\[card\]/); // afterTool redacted
  assert.doesNotMatch(results[0], /4111/);
  assert.match(results[1], /sunny \(cached\)/); // skipped the tool
  assert.equal(result.text, "[redacted]"); // afterModel replaced the reply
  assert.deepEqual(ended, [["completed", 2]]);
});

test("a hook can end the run, and what it throws fails the run as the cause", async () => {
  const budget = {
    beforeModel(ctx) {
      if (ctx.modelCalls >= 1) return { end: true, output: "stopped by the budget hook" };
    },
  };
  const model = new FakeModel([FakeReply.toolCall("noop", {}), "never"], { byTurn: true });
  const result = await new Agent({ model, hooks: [budget] }).run("go");
  assert.equal(result.output, "stopped by the budget hook");

  class MissingSetting extends Error {}
  const broken = {
    name: "broken",
    beforeModel() {
      throw new MissingSetting("no budget configured");
    },
  };
  await assert.rejects(new Agent({ model: new FakeModel(["x"]), hooks: [broken] }).run("go"), (e) => {
    assert.ok(e instanceof CriogenError);
    assert.equal(e.code, "hook_failed");
    assert.ok(e.cause instanceof MissingSetting);
    return true;
  });
  assert.throws(() => new Agent({ model: new FakeModel(["x"]), hooks: [{}] }), TypeError);

  // In a tool hook it is the tool's crash: with "fail", the run fails with it as the cause.
  class CannotRedact extends Error {}
  const leaky = {
    afterTool() {
      throw new CannotRedact("cannot redact");
    },
  };
  const ping = tool({ name: "ping", description: "Pings.", input: { type: "object", properties: {} }, run: () => "pong" });
  const crashing = new Agent({
    model: new FakeModel([FakeReply.toolCall("ping", {}), "done"]),
    tools: [ping],
    hooks: [leaky],
    onToolException: "fail",
  });
  await assert.rejects(crashing.run("go"), (e) => {
    assert.ok(e instanceof ToolExecutionError);
    assert.ok(e.cause instanceof CannotRedact);
    return true;
  });
});

test("the engine validates arguments before the tool runs", async () => {
  const calls = [];
  const charge = tool({
    name: "charge",
    description: "Charges.",
    input: { type: "object", properties: { amount: { type: "integer" } }, required: ["amount"] },
    run: ({ amount }) => {
      calls.push(amount);
      return "ok";
    },
  });
  const agent = new Agent({
    model: new FakeModel([FakeReply.toolCall("charge", { amount: "lots" }), "I could not."]),
    tools: [charge],
  });
  const result = await agent.run("charge");
  assert.deepEqual(calls, []);
  assert.match(JSON.stringify(toolResults(result)[0].content), /Invalid arguments/);
});

test("tool input from Zod: the model sees its JSON Schema and its issues; the tool gets its output", async () => {
  const { z } = require("zod");
  const seen = [];
  const order = tool({
    name: "order",
    description: "Orders an item.",
    input: z
      .object({ sku: z.string().regex(/^SKU-\d+$/), qty: z.number().int().min(1).default(1) })
      .refine((o) => o.qty <= 5, { message: "at most 5 per order", path: ["qty"] }),
    run: (args) => {
      seen.push(args);
      return "ordered";
    },
  });
  // The model is offered the JSON Schema Zod makes.
  assert.deepEqual(order.jsonSchema.required, ["sku"]);
  assert.equal(order.jsonSchema.properties.sku.pattern, "^SKU-\\d+$");
  assert.equal(order.jsonSchema.$schema, undefined);

  const model = new FakeModel([
    FakeReply.toolCall("order", { sku: "SKU-1", qty: 9 }), // passes JSON Schema, fails the refinement
    FakeReply.toolCall("order", { sku: "SKU-1" }),
    "Ordered one.",
  ]);
  const result = await new Agent({ model, tools: [order] }).run("order SKU-1");
  assert.equal(result.text, "Ordered one.");
  const [rejected, accepted] = toolResults(result);
  assert.equal(rejected.error.code, "invalid_arguments");
  assert.match(rejected.error.message, /qty: at most 5 per order/);
  assert.equal(accepted.status, "success");
  assert.deepEqual(seen, [{ sku: "SKU-1", qty: 1 }]); // the default, applied by Zod

  // The engine still checks the JSON Schema first (the pattern).
  const bad = await new Agent({
    model: new FakeModel([FakeReply.toolCall("order", { sku: "nope" }), "ok"]),
    tools: [order],
  }).run("x");
  assert.match(toolResults(bad)[0].error.message, /sku/);
  assert.equal(seen.length, 1);

  // A Standard Schema without a JSON Schema needs one given.
  const bare = { "~standard": { version: 1, vendor: "bare", validate: (value) => ({ value }) } };
  assert.throws(() => tool({ name: "bare", description: "", input: bare, run: () => null }), /jsonSchema/);
  const given = tool({ name: "bare", description: "", input: bare, jsonSchema: { type: "object" }, run: () => null });
  assert.deepEqual(given.spec().input, { type: "object" });
});

test("typed output with a JSON Schema", async () => {
  const output = {
    type: "object",
    properties: { category: { type: "string" }, refund: { type: "boolean" } },
    required: ["category", "refund"],
    additionalProperties: false,
  };
  const agent = new Agent({
    model: new FakeModel([FakeReply.toolCall("final_output", { category: "billing", refund: true })]),
    output,
  });
  assert.deepEqual((await agent.run("charged twice")).output, { category: "billing", refund: true });
});

test("threads remember, and approvals continue from another agent instance", async () => {
  const storage = new Storage();
  const refunds = [];
  const refund = tool({
    name: "refund",
    description: "Refunds an order.",
    input: { type: "object", properties: { orderId: { type: "string" } }, required: ["orderId"] },
    destructive: true,
    run: ({ orderId }) => {
      refunds.push(orderId);
      return "refunded";
    },
  });
  const make = (replies) => new Agent({ model: new FakeModel(replies), tools: [refund], approve: ["refund"], storage });

  const chat = make([FakeReply.toolCall("refund", { orderId: "A-1" })]).thread("t-1", { actor: "user:ada" });
  let reply = await chat.send("refund A-1");
  assert.ok(reply.needsApproval);
  assert.equal(reply.approvals[0].tool, "refund");
  assert.deepEqual(reply.approvals[0].args, { orderId: "A-1" });
  assert.deepEqual(refunds, []);
  await assert.rejects(chat.send("hello?"), ThreadSuspendedError);

  const second = make(["Refunded."]);
  reply = await second.thread("t-1", { actor: "user:boss" }).approve();
  assert.equal(reply.text, "Refunded.");
  assert.deepEqual(refunds, ["A-1"]);
  const roles = (await second.thread("t-1").messages()).map((m) => m.role);
  assert.deepEqual(roles, ["user", "assistant", "tool", "assistant"]);
  assert.equal((await chat.history())[0].status, "completed");
  assert.equal(await chat.delete(), true);
  assert.equal(await chat.delete(), false);
});

test("reject, decide with an edit, invalid decisions, rewind", async () => {
  const agent = new Agent({
    model: new FakeModel([
      FakeReply.toolCall("add", { a: 1, b: 1 }),
      "Not added.",
      FakeReply.toolCall("add", { a: 1, b: 1 }),
      "Added with edits.",
    ]),
    tools: [add],
    approve: ["add"],
  });
  const chat = agent.thread("t");
  await chat.send("add");
  assert.equal((await chat.reject("no")).text, "Not added.");
  const pending = await chat.send("add please");
  const reply = await chat.decide({ [pending.approvals[0].call_id]: Decision.edit({ a: 2, b: 3 }) });
  assert.equal(reply.text, "Added with edits.");
  assert.match(JSON.stringify(reply.messages.at(-2).content), /5/);
  await assert.rejects(chat.decide({ nope: Decision.approve() }), (e) => e instanceof ValidationError && e.code === "resume_invalid");
  const oldest = (await chat.history(100)).at(-1).id;
  assert.equal((await chat.rewind(oldest)).source, "fork");
});

test("SQLite threads survive a new agent; memory, retention and audit", { skip: serverOnly("SQLite") }, async () => {
  const db = tempDb("sqlite");
  try {
    const first = new Agent({ model: new FakeModel(["Hello Ada"]), storage: new Storage({ url: db.url, audit: true }) });
    assert.equal((await first.thread("t", { actor: "user:ada" }).send("I am Ada")).text, "Hello Ada");
    const storage = new Storage({ url: db.url, audit: true, retention: { keepCheckpoints: 1 } });
    const second = new Agent({ model: new FakeModel(["You are Ada"]), storage });
    assert.equal((await second.thread("t").send("Who am I?")).messages.length, 4);

    await storage.remember("users/ada", "diet", "vegetarian, no peanuts");
    assert.equal((await storage.search("users", "peanuts"))[0].key, "diet");
    assert.equal((await storage.recall("users/ada", "diet")).value, "vegetarian, no peanuts");
    assert.equal(await storage.forgetAll("users/ada"), 1);
    assert.ok((await storage.prune()).checkpointsDeleted > 0);
    const records = await storage.auditRecords({ threadId: "t" });
    const actions = records.map((r) => r.action);
    assert.ok(actions.includes("run.finished") && actions.includes("model.called"));
    assert.equal(records[0].actor, "user:ada");
    assert.equal((await storage.verifyAudit()).ok, true);
    await storage.close();
    await first.storage.close();
    await assert.rejects(storage.recall("users/ada", "diet"), (e) => e instanceof StorageError && e.code === "storage_unavailable");
    // Closed, the file can go (on Windows other programs, such as the
    // virus scanner, may hold it for a moment).
    for (const suffix of ["", "-wal", "-shm"]) {
      fs.rmSync(db.file + suffix, { force: true, maxRetries: 50, retryDelay: 20 });
    }
  } finally {
    db.remove();
  }
});

test("encrypted storage keeps text unreadable and refuses to open without its keys", { skip: serverOnly("encryption at rest") }, async () => {
  const db = tempDb("enc");
  try {
    const keys = "k1:" + "ab".repeat(32);
    const agent = new Agent({ model: new FakeModel(["noted"]), storage: new Storage({ url: db.url, encryptionKeys: keys }) });
    await agent.thread("t").send("my secret is pelican");
    assert.ok(!fs.readFileSync(db.file).includes("pelican"));
    assert.throws(() => new Storage(db.url), (e) => e instanceof StorageError && e.code === "encryption_key_missing");
  } finally {
    db.remove();
  }
});

test("pending approvals are read from storage", async () => {
  const agent = new Agent({
    model: new FakeModel([FakeReply.toolCall("add", { a: 1, b: 2 }), "Three."]),
    tools: [add],
    approve: ["add"],
  });
  const chat = agent.thread("p");
  assert.deepEqual(await chat.pendingApprovals(), []);
  const reply = await chat.send("add");
  assert.ok(reply.needsApproval);
  const waiting = await chat.pendingApprovals();
  assert.equal(waiting.length, 1);
  assert.equal(waiting[0].tool, "add");
  assert.deepEqual(waiting[0].args, { a: 1, b: 2 });
  assert.equal(waiting[0].call_id, reply.approvals[0].call_id);
  assert.equal((await chat.approve()).text, "Three.");
  assert.deepEqual(await chat.pendingApprovals(), []);
});

test("other stream events carry no content", async () => {
  const agent = new Agent({ model: new FakeModel(["SECRET-ANSWER"]) });
  const stream = agent.thread("o").stream("SECRET-QUESTION");
  const others = [];
  for await (const event of stream) if (event.type === "other") others.push(event);
  await stream.reply();
  assert.ok(others.length > 0, "a run has engine events");
  for (const event of others) {
    for (const key of Object.keys(event)) assert.ok(["type", "event", "node", "step"].includes(key), key);
    assert.match(event.event, /^[a-z_]+$/);
    assert.ok(!JSON.stringify(event).includes("SECRET"));
  }
});

test("streaming shows text and approvals, then the saved reply", async () => {
  const agent = new Agent({
    model: new FakeModel(["Hello there", FakeReply.toolCall("add", { a: 1, b: 2 }), "Three."]),
    tools: [add],
    approve: ["add"],
  });
  const chat = agent.thread("s");
  let stream = chat.stream("hi");
  let text = "";
  for await (const event of stream) if (event.type === "text") text += event.text;
  assert.equal(text, "Hello there");
  assert.equal((await stream.reply()).text, "Hello there");

  stream = chat.stream("add");
  const types = [];
  for await (const event of stream) types.push(event.type);
  assert.ok(types.includes("approval_needed"));
  assert.ok((await stream.reply()).needsApproval);

  stream = chat.streamApprove();
  const events = [];
  for await (const event of stream) events.push(event);
  assert.ok(events.some((e) => e.type === "tool_call_finished" && e.status === "success"));
  assert.equal((await stream.reply()).text, "Three.");
});

test("an AbortSignal cancels the run, and the thread is saved for retry", async () => {
  const agent = new Agent({ model: new FakeModel(["slow answer"], { delayMs: 5000 }) });
  const chat = agent.thread("c");
  const started = Date.now();
  await assert.rejects(chat.send("hi", { signal: AbortSignal.timeout(300) }), CancelledError);
  assert.ok(Date.now() - started < 2000);
  assert.equal((await chat.history())[0].status, "failed");
  // Aborted before the call starts.
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(agent.run("x", { signal: controller.signal }), CancelledError);
});

test("cancel reaches a stream that is being read", async () => {
  const agent = new Agent({ model: new FakeModel(["never"], { delayMs: 5000 }) });
  const stream = agent.thread("x").stream("hi");
  const reading = (async () => {
    for await (const _ of stream);
  })();
  await new Promise((r) => setTimeout(r, 200));
  const started = Date.now();
  stream.cancel();
  await reading;
  await assert.rejects(stream.reply(), CancelledError);
  assert.ok(Date.now() - started < 2000);
});

test("memory by state recalls per user", async () => {
  const storage = new Storage();
  const agent = new Agent({
    model: new FakeModel([FakeReply.toolCall("remember", { key: "diet", value: "vegan" }), "Noted.", "Eat lentils."]),
    storage,
    memory: Memory.byState("user_id", "users"),
  });
  assert.equal((await agent.run("I am vegan", { state: { user_id: "ada" } })).text, "Noted.");
  assert.equal((await storage.search("users/ada"))[0].value, "vegan");
  assert.equal((await agent.run("What should I eat?", { state: { user_id: "ada" } })).text, "Eat lentils.");
  await assert.rejects(agent.run("no user"), (e) => isCriogenError(e, "invalid_state"));
});

test("limits, construction errors and model names", async () => {
  const looping = new Agent({
    model: new FakeModel(Array(5).fill(FakeReply.toolCall("add", { a: 1, b: 1 }))),
    tools: [add],
    limits: { modelCalls: 2 },
  });
  await assert.rejects(looping.run("loop"), (e) => e instanceof LimitExceededError && e.code === "limit_exceeded");
  assert.throws(() => new Agent({ model: new FakeModel(), tools: [() => 1] }), TypeError);
  assert.throws(() => new Agent({ model: "nope" }), TypeError);
  assert.throws(() => tool({ name: "bad name", description: "", input: {}, run: () => 1 }), TypeError);
  // Two tools with one name: the engine refuses the agent.
  assert.throws(() => new Agent({ model: new FakeModel(), tools: [add, add] }), (e) => e instanceof CriogenError);

  const { modelSpec } = require(process.env.CRIOGEN_SDK ? `${process.env.CRIOGEN_SDK}/dist/models` : "../dist/models");
  process.env.OPENAI_API_KEY = "sk-test-not-real";
  try {
    assert.deepEqual(modelSpec("openai:gpt-5-mini"), {
      kind: "openai",
      model: "gpt-5-mini",
      apiKey: "sk-test-not-real",
      baseUrl: undefined,
      name: undefined,
      allowInsecureHttp: undefined,
    });
    assert.ok(!JSON.stringify(sdk.openai("gpt-5-mini")).includes("sk-test"));
  } finally {
    delete process.env.OPENAI_API_KEY;
  }
});

test("every catalog code raises its family class", () => {
  // schemas/error-codes.json (design 0013): the scripts pass its path.
  const file = process.env.CRIOGEN_ERROR_CATALOG || path.join(__dirname, "..", "..", "..", "schemas", "error-codes.json");
  const catalog = JSON.parse(fs.readFileSync(file, "utf8"));
  const { errorFor } = require(process.env.CRIOGEN_SDK ? `${process.env.CRIOGEN_SDK}/dist/errors` : "../dist/errors");
  const classes = {
    model: sdk.ModelError,
    tool: sdk.ToolExecutionError,
    limit: sdk.LimitExceededError,
    busy: sdk.ThreadBusyError,
    suspended: sdk.ThreadSuspendedError,
    storage: sdk.StorageError,
    validation: sdk.ValidationError,
    cancelled: sdk.CancelledError,
    other: sdk.CriogenError,
  };
  let checked = 0;
  for (const entry of catalog.codes) {
    if (entry.family === null) continue;
    const error = errorFor(entry.code, "x");
    assert.equal(error.constructor, classes[entry.family], `${entry.code}: ${error.constructor.name}, wanted ${entry.family}`);
    assert.equal(error.code, entry.code);
    checked += 1;
  }
  assert.ok(checked > 20);
});

test("concurrent runs get their own hook exception", async () => {
  // Two runs of one agent fail in the same hook at the same moment: each
  // must get the exception thrown for it, never the other's (security audit).
  class Mine extends Error {}
  const flaky = {
    name: "flaky",
    async beforeModel(ctx) {
      const text = ctx.messages[ctx.messages.length - 1].content[0].text;
      await new Promise((resolve) => setTimeout(resolve, text === "slow" ? 200 : 20));
      throw new Mine(`for ${text}`);
    },
  };
  const agent = new Agent({ model: new FakeModel(["never"]), hooks: [flaky] });
  const [slow, fast] = await Promise.allSettled([agent.run("slow"), agent.run("fast")]);
  assert.ok(slow.reason.cause instanceof Mine);
  assert.equal(slow.reason.cause.message, "for slow");
  assert.equal(fast.reason.cause.message, "for fast");
});

test("credentials are not sent over plain http to another machine", () => {
  const { openaiCompatible } = sdk;
  assert.throws(
    () => new Agent({ model: openaiCompatible("m", { baseUrl: "http://gpu-box:8000/v1", apiKey: "a-secret-key-1234" }) }),
    (error) => {
      assert.ok(error instanceof CriogenError);
      assert.match(error.message, /plain http/);
      assert.doesNotMatch(error.message, /a-secret-key-1234/);
      return true;
    },
  );
  // Explicitly allowed for a trusted network; this machine and https need nothing.
  new Agent({ model: openaiCompatible("m", { baseUrl: "http://gpu-box:8000/v1", apiKey: "k-12345678", allowInsecureHttp: true }) });
  new Agent({ model: openaiCompatible("m", { baseUrl: "http://localhost:11434/v1", apiKey: "k-12345678" }) });
  new Agent({ model: openaiCompatible("m", { baseUrl: "https://gpu-box/v1", apiKey: "k-12345678" }) });
});
