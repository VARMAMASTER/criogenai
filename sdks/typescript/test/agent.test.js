// The TypeScript SDK (design 0012) with simulated models only: runs, tools,
// typed output, threads, approvals, storage, streaming, cancellation, memory,
// errors. Runs against the source build, or the installed package when
// AGENTBUILD_SDK names it (scripts/test-typescript.sh).

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const crypto = require("node:crypto");

const sdk = require(process.env.AGENTBUILD_SDK || path.join(__dirname, ".."));
const {
  Agent,
  AgentBuildError,
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
  ValidationError,
  isAgentBuildError,
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
  const file = path.join(os.tmpdir(), `agentbuild-ts-${name}-${crypto.randomUUID()}.db`);
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

test("tool errors reach the model; crashes fail the run with the thrown value as cause", async () => {
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

  const crashing = new Agent({ model: new FakeModel([FakeReply.toolCall("lookup", { orderId: "x" })]), tools: [lookup] });
  await assert.rejects(crashing.run("go"), (e) => {
    assert.ok(e instanceof ToolExecutionError);
    assert.equal(e.code, "tool_failed");
    assert.ok(e.cause instanceof Missing);
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

test("SQLite threads survive a new agent; memory, retention and audit", async () => {
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

test("encrypted storage keeps text unreadable and refuses to open without its keys", async () => {
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
  await assert.rejects(agent.run("no user"), (e) => isAgentBuildError(e, "invalid_state"));
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
  assert.throws(() => new Agent({ model: new FakeModel(), tools: [add, add] }), (e) => e instanceof AgentBuildError);

  const { modelSpec } = require(process.env.AGENTBUILD_SDK ? `${process.env.AGENTBUILD_SDK}/dist/models` : "../dist/models");
  process.env.OPENAI_API_KEY = "sk-test-not-real";
  try {
    assert.deepEqual(modelSpec("openai:gpt-5-mini"), {
      kind: "openai",
      model: "gpt-5-mini",
      apiKey: "sk-test-not-real",
      baseUrl: undefined,
      name: undefined,
    });
    assert.ok(!JSON.stringify(sdk.openai("gpt-5-mini")).includes("sk-test"));
  } finally {
    delete process.env.OPENAI_API_KEY;
  }
});

test("every catalog code raises its family class", () => {
  // schemas/error-codes.json (design 0013): the scripts pass its path.
  const file = process.env.AGENTBUILD_ERROR_CATALOG || path.join(__dirname, "..", "..", "..", "schemas", "error-codes.json");
  const catalog = JSON.parse(fs.readFileSync(file, "utf8"));
  const { errorFor } = require(process.env.AGENTBUILD_SDK ? `${process.env.AGENTBUILD_SDK}/dist/errors` : "../dist/errors");
  const classes = {
    model: sdk.ModelError,
    tool: sdk.ToolExecutionError,
    limit: sdk.LimitExceededError,
    busy: sdk.ThreadBusyError,
    suspended: sdk.ThreadSuspendedError,
    storage: sdk.StorageError,
    validation: sdk.ValidationError,
    cancelled: sdk.CancelledError,
    other: sdk.AgentBuildError,
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
