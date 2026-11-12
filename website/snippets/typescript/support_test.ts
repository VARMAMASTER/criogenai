// The tutorial's tests (docs: tutorial/support-agent): npx tsx --test support_test.ts
// [show]
import assert from "node:assert/strict";
import { test } from "node:test";
import { FakeModel, FakeReply, Storage } from "criogenai";
import { buildAgent } from "./support_agent.js";

test("a refund waits for a person", async () => {
  const model = new FakeModel([
    FakeReply.toolCall("issue_refund", { orderId: "A-1001", amount: 20, reason: "broken" }),
    "I have refunded 20.",
  ]);
  const agent = buildAgent(model, new Storage()); // in memory: nothing left behind
  const chat = agent.thread("t-1");

  let reply = await chat.send("Please refund 20 on A-1001", { state: { customer_id: "c-7" } });
  assert.ok(reply.needsApproval);
  assert.equal(reply.approvals[0].args.amount, 20);

  reply = await chat.approve();
  assert.equal(reply.text, "I have refunded 20.");
  assert.equal(reply.toolCalls, 1);
});

test("an unknown order goes back to the model", async () => {
  const model = new FakeModel([FakeReply.toolCall("get_order", { orderId: "Z-9" }), "Which order?"]);
  const agent = buildAgent(model, new Storage());
  const result = await agent.run("Where is Z-9?", { state: { customer_id: "c-7" } });
  const toolMessage = result.messages[2] as { status: string; content: { text: string }[] };
  assert.equal(toolMessage.status, "error");
  assert.match(toolMessage.content[0].text, /No order Z-9/);
  assert.equal(result.text, "Which order?");
});
// [/show]
