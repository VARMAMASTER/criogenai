// [show]
import { Agent, FakeModel, FakeReply, ToolError, tool } from "agentbuild-sdk";

const issueRefund = tool<{ orderId: string; amount: number }>({
  name: "issue_refund",
  description: "Refund an order.",
  destructive: true,
  input: {
    type: "object",
    properties: {
      orderId: { type: "string" },
      amount: { type: "number", minimum: 0, maximum: 500 },
    },
    required: ["orderId", "amount"],
  },
  run: ({ amount }) => {
    // Arguments arrive already checked against the schema.
    if (amount > 200) throw new ToolError("Refunds over 200 need a manager");
    return { refunded: amount };
  },
});

const agent = new Agent<{ category: string; refund: boolean }>({
  model: "openai:gpt-5-mini",
  instructions: "You are a support agent.",
  tools: [issueRefund],
  output: {
    type: "object",
    properties: { category: { enum: ["billing", "shipping", "other"] }, refund: { type: "boolean" } },
    required: ["category", "refund"],
  },
  limits: { modelCalls: 10, toolCalls: 20 },
});

// In tests: scripted replies, no network, the same answer every time.
const testAgent = new Agent({
  model: new FakeModel([FakeReply.toolCall("issue_refund", { orderId: "A-1001", amount: 24 }), "Refunded."]),
  tools: [issueRefund],
});
const result = await testAgent.run("Refund my order");
// [/show]
export {};

// [show subagents]
const researcher = new Agent({ model: "openai:gpt-5-mini", instructions: "Find facts about orders. Be precise." });
const support = new Agent({
  model: "openai:gpt-5-mini",
  instructions: "You are a support agent.",
  // The researcher becomes a tool: it takes { input } and answers.
  tools: [researcher.asTool("research", "Looks up facts about an order")],
});
// [/show subagents]

// [show hooks]
import type { Hook } from "agentbuild-sdk";

const guard: Hook = {
  name: "guard",
  beforeTool(call) {
    if (call.name === "issue_refund" && Number(call.args.amount) > 500) {
      throw new ToolError("Refunds over 500 need a manager."); // the model sees this
    }
    return undefined;
  },
  afterTool(call, result) {
    // The model sees the returned value instead (undefined keeps the result).
    return typeof result === "string" ? result.replace("4111-1111-1111-1111", "[card]") : undefined;
  },
  beforeModel(ctx) {
    if (ctx.modelCalls >= 10) return { end: true, output: "Stopped after 10 model calls." };
    return undefined;
  },
};
const guarded = new Agent({ model: "openai:gpt-5-mini", tools: [issueRefund], hooks: [guard] });
// [/show hooks]

// [show zod]
import { z } from "zod";

const refund = tool({
  name: "issue_refund",
  description: "Refund an order.",
  // The model sees the JSON Schema Zod makes; the engine checks it, then
  // Zod checks the rest (the refinement) and its issues go back to the model.
  input: z
    .object({ orderId: z.string(), amount: z.number().positive(), reason: z.string().default("customer request") })
    .refine((r) => r.amount <= 500, { message: "over 500 needs a manager", path: ["amount"] }),
  // args is { orderId: string; amount: number; reason: string }, with the default applied.
  run: async ({ orderId, amount, reason }) => ({ refunded: amount, orderId, reason }),
});
// [/show zod]
