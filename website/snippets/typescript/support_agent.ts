// The tutorial's agent (docs: tutorial/support-agent). Type-checked; the
// tutorial's test (support_test.ts) runs it with a FakeModel.

// [show tools]
import { ToolError, tool } from "criogen";

// A stand-in for your order database.
const ORDERS: Record<string, { status: string; total: number }> = {
  "A-1001": { status: "delivered", total: 48 },
  "A-1002": { status: "in transit", total: 120 },
};

export const getOrder = tool<{ orderId: string }>({
  name: "get_order",
  description: "Look up an order by its id (like A-1001): its status and total.",
  readOnly: true,
  input: {
    type: "object",
    properties: { orderId: { type: "string" } },
    required: ["orderId"],
  },
  run: ({ orderId }) => {
    const order = ORDERS[orderId];
    // The model sees this message and can ask the customer again.
    if (!order) throw new ToolError(`No order ${orderId}. Ask the customer to check the id.`);
    return { orderId, ...order };
  },
});

export const issueRefund = tool<{ orderId: string; amount: number; reason: string }>({
  name: "issue_refund",
  description: "Refund part or all of an order.",
  destructive: true,
  input: {
    type: "object",
    properties: {
      orderId: { type: "string" },
      amount: { type: "number", exclusiveMinimum: 0 },
      reason: { type: "string" },
    },
    required: ["orderId", "amount", "reason"],
  },
  run: ({ orderId, amount, reason }) => {
    const order = ORDERS[orderId];
    if (!order) throw new ToolError(`No order ${orderId}`);
    if (amount > order.total) throw new ToolError(`A refund must be at most ${order.total}`);
    return { orderId, refunded: amount, reason };
  },
});
// [/show tools]

// [show agent]
import { Agent, Memory, Storage, type Model } from "criogen";

const INSTRUCTIONS = `You are the support assistant of an online shop.
Look an order up before you answer a question about it.
To refund, call issue_refund: a staff member approves it before it runs.
Save lasting facts about the customer (preferences, past problems) with remember.`;

export function buildAgent(model: Model, storage: Storage): Agent {
  return new Agent({
    model,
    instructions: INSTRUCTIONS,
    tools: [getOrder, issueRefund],
    approve: ["issue_refund"], // pause before every refund
    storage, // conversations survive restarts
    memory: Memory.byState("customer_id", "customers"), // facts per customer
    limits: { modelCalls: 12, toolCalls: 20 },
  });
}
// [/show agent]
