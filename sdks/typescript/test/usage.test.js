// Every result says how long it took, which model answered (a fallback shows),
// and the calls and tokens per model; estimateCost() prices it.

const test = require("node:test");
const assert = require("node:assert/strict");
const path = require("node:path");

const { Agent, FakeModel, FakeReply, tool, estimateCost } = require(process.env.CRIOGEN_SDK || path.join(__dirname, ".."));

const add = tool({
  name: "add",
  description: "Adds two numbers.",
  input: { type: "object", properties: { a: { type: "integer" }, b: { type: "integer" } }, required: ["a", "b"] },
  run: ({ a, b }) => a + b,
});

test("a result has durationMs, answeredBy and the calls and tokens per model", async () => {
  const model = new FakeModel([FakeReply.toolCall("add", { a: 2, b: 3 }), "5"], { name: "main" });
  const result = await new Agent({ model, tools: [add] }).run("2 + 3?");
  assert.ok(result.durationMs >= 0);
  assert.deepEqual(result.answeredBy, { provider: "fake", model: "main" });
  assert.equal(result.models.length, 1);
  assert.equal(result.models[0].calls, 2);
  assert.equal(result.models[0].usage.input_tokens, result.usage.input_tokens);
  assert.equal(result.models[0].usage.output_tokens, result.usage.output_tokens);
});

test("a fallback that answered is named in answeredBy", async () => {
  const agent = new Agent({
    model: new FakeModel(["never"], { failFirst: 100, name: "main" }),
    fallbacks: [new FakeModel(["from the backup"], { name: "backup" })],
    modelRetry: { maxAttempts: 1, initialBackoffSecs: 0.01, jitter: false },
  });
  const result = await agent.run("hi");
  assert.equal(result.text, "from the backup");
  assert.deepEqual(result.answeredBy, { provider: "fake", model: "backup" });
});

test("estimateCost prices each model's tokens, and says null for a model without a price", async () => {
  const result = await new Agent({ model: new FakeModel(["hello"], { name: "main" }) }).run("hi");
  const usage = result.models[0].usage;
  const expected = (usage.input_tokens * 2 + usage.output_tokens * 8) / 1_000_000;
  assert.equal(estimateCost(result, { "fake:main": { input: 2, output: 8 } }), expected);
  assert.equal(estimateCost(result, { main: { input: 2, output: 8 } }), expected);
  assert.equal(estimateCost(result, { other: { input: 1, output: 1 } }), null);
});

test("on a thread, only this call's model calls count", async () => {
  const agent = new Agent({ model: new FakeModel(["one", "two"], { name: "main" }) });
  const thread = agent.thread("usage-thread");
  await thread.send("first");
  const second = await thread.send("second");
  assert.equal(second.models[0].calls, 1);
});
