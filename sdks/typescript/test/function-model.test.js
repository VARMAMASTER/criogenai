// Models written in JavaScript (design 0021 §7): the engine calls a function
// for each model call. Runs on the Node addon and, with CRIOGEN_ENGINE=wasm,
// on the WebAssembly engine.

const test = require("node:test");
const assert = require("node:assert/strict");
const path = require("node:path");

const sdk = require(process.env.CRIOGEN_SDK || path.join(__dirname, ".."));
const { Agent, FakeReply, FunctionModel, Gateway, ModelError, isCriogenError, tool } = sdk;

const add = tool({
  name: "add",
  description: "Adds two numbers.",
  input: { type: "object", properties: { a: { type: "number" }, b: { type: "number" } }, required: ["a", "b"] },
  run: ({ a, b }) => a + b,
});

const textOf = (messages) => JSON.stringify(messages);

test("one JavaScript model answers several agents at once, by what each request says", async () => {
  const seen = [];
  const model = new FunctionModel(async (call) => {
    seen.push(call.messages.length);
    await new Promise((r) => setTimeout(r, 20));
    return /You judge ideas/.test(textOf(call.messages)) ? "the idea scores 7" : "a writer's answer";
  });
  const judge = new Agent({ model, instructions: "You judge ideas." });
  const writer = new Agent({ model, instructions: "You write." });
  const [judged, written] = await Promise.all([judge.run("rate this"), writer.run("write this")]);
  assert.equal(judged.text, "the idea scores 7");
  assert.equal(written.text, "a writer's answer");
  assert.equal(seen.length, 2);
});

test("a JavaScript model sees the tools, calls one, and reads its result", async () => {
  const requests = [];
  const model = new FunctionModel((call) => {
    requests.push(call);
    const result = call.messages.find((m) => m.role === "tool");
    if (!result) return FakeReply.toolCall("add", { a: 2, b: 40 });
    return `the sum is ${result.content[0].value}`;
  }, { name: "scripted" });
  const result = await new Agent({ model, tools: [add] }).run("2 + 40?");
  assert.equal(result.text, "the sum is 42");
  assert.equal(result.toolCalls, 1);
  assert.deepEqual(requests[0].tools.map((t) => t.name), ["add"]);
  assert.equal(result.messages.at(-1).metadata.provider, "function");
  assert.equal(result.messages.at(-1).metadata.model, "scripted");
});

test("a classified error is retried; a throw fails the run with model_failed", async () => {
  let calls = 0;
  const flaky = new FunctionModel(() => {
    calls += 1;
    return calls === 1 ? { error: { kind: "rate_limited", message: "slow down" } } : "second try";
  });
  const result = await new Agent({ model: flaky, modelRetry: { initialBackoffSecs: 0.01, jitter: false } }).run("hi");
  assert.equal(result.text, "second try");
  assert.equal(calls, 2);

  const broken = new FunctionModel(() => {
    throw new TypeError("the local model is not loaded");
  });
  await assert.rejects(new Agent({ model: broken, modelRetry: { maxAttempts: 1 } }).run("hi"), (e) => {
    return e instanceof ModelError && e.code === "model_failed" && /threw: TypeError: the local model is not loaded/.test(e.message);
  });
});

test("typed output and the gateway work with a JavaScript model", async () => {
  const output = { type: "object", properties: { mood: { type: "string" } }, required: ["mood"], additionalProperties: false };
  const typed = new FunctionModel(() => FakeReply.toolCall("final_output", { mood: "sunny" }));
  assert.deepEqual((await new Agent({ model: typed, output }).run("how is it?")).output, { mood: "sunny" });

  const echo = new FunctionModel((call) => `echo: ${call.messages.at(-1).content[0].text}`);
  const reply = await new Gateway().chat(echo, "ping");
  assert.equal(reply.text, "echo: ping");
  assert.equal(reply.model, "function:function");
});

test("a FunctionModel needs a function, and a nonsense answer is an invalid response", async () => {
  assert.throws(() => new FunctionModel("not a function"), TypeError);
  const nonsense = new FunctionModel(() => ({ what: "?" }));
  await assert.rejects(new Agent({ model: nonsense, modelRetry: { maxAttempts: 1 } }).run("hi"), (e) => isCriogenError(e, "model_failed") && /neither text/.test(e.message));
});

test("a FunctionModel can pass an agent's request on to another model through the gateway, tools and all", async () => {
  // The "real" model here is scripted too: it calls the tool once, then answers.
  const upstream = new FunctionModel((call) => {
    assert.deepEqual(call.tools.map((t) => t.name), ["add"]);
    const result = call.messages.find((m) => m.role === "tool");
    return result ? `upstream says ${result.content[0].value}` : FakeReply.toolCall("add", { a: 20, b: 22 });
  }, { name: "upstream" });
  const gateway = new Gateway({ retry: { maxAttempts: 1 } });
  const seen = [];
  const forwarding = new FunctionModel(async (call) => {
    seen.push(call.messages.length);
    const reply = await gateway.chat(upstream, call.messages, { tools: call.tools });
    return { message: reply.message };
  }, { name: "forwarding" });
  const result = await new Agent({ model: forwarding, tools: [add] }).run("20 + 22?");
  assert.equal(result.text, "upstream says 42");
  assert.equal(result.toolCalls, 1);
  assert.equal(seen.length, 2);
});
