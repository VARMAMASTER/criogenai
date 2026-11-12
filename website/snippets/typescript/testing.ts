// Docs: guides/testing. Type-checked; every block runs with node --test.
import assert from "node:assert/strict";
import { test } from "node:test";
import { Agent, FakeModel, FakeReply, Storage, tool } from "criogenai";

const getOrder = tool<{ orderId: string }>({
  name: "get_order",
  description: "Look up an order.",
  input: { type: "object", properties: { orderId: { type: "string" } }, required: ["orderId"] },
  run: () => ({ status: "shipped" }),
});

// [show script]
test("the agent looks the order up", async () => {
  const model = new FakeModel([
    FakeReply.toolCall("get_order", { orderId: "A-1001" }), // 1st model call
    "Your order has shipped.", // 2nd model call
  ]);
  const result = await new Agent({ model, tools: [getOrder] }).run("Where is A-1001?");

  assert.equal(result.text, "Your order has shipped.");
  assert.deepEqual([result.modelCalls, result.toolCalls], [2, 1]);
  const toolMessage = result.messages[2]; // user, assistant (the call), tool, assistant
  assert.equal(toolMessage.tool, "get_order");
  assert.equal(toolMessage.status, "success");
});
// [/show script]

// [show by_turn]
test("many conversations at once", async () => {
  // byTurn: the reply depends on the model call's place in its run, not on
  // how many calls came before; one model serves any number of runs.
  const model = new FakeModel([FakeReply.toolCall("get_order", { orderId: "A-1" }), "Shipped."], { byTurn: true });
  const agent = new Agent({ model, tools: [getOrder], storage: new Storage() });
  const replies = await Promise.all(Array.from({ length: 20 }, (_, i) => agent.thread(`t-${i}`).send("Where?")));
  assert.ok(replies.every((r) => r.text === "Shipped."));
});
// [/show by_turn]

// [show failures]
test("a rate limit is retried", async () => {
  // The first two calls fail with rate_limited (transient), the third answers.
  const model = new FakeModel(["Hello."], { failFirst: 2 });
  const agent = new Agent({ model, modelRetry: { maxAttempts: 3, initialBackoffSecs: 0.01 } });
  assert.equal((await agent.run("Hi")).text, "Hello.");
});

test("the fallback answers when the first model is down", async () => {
  const down = new FakeModel(["never"], { failFirst: 100, name: "primary" });
  const backup = new FakeModel(["From the backup."], { name: "backup" });
  const agent = new Agent({ model: down, fallbacks: [backup], modelRetry: { maxAttempts: 1 } });
  const result = await agent.run("Hi");
  assert.equal(result.text, "From the backup.");
  assert.deepEqual(result.answeredBy, { provider: "fake", model: "backup" });
});
// [/show failures]
