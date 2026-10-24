// [show]
import { Agent, ToolError, tool } from "agentbuild-sdk";

const getOrder = tool({
  name: "get_order",
  description: "Look up an order by id.",
  input: {
    type: "object",
    properties: { orderId: { type: "string" } },
    required: ["orderId"],
  },
  run: async ({ orderId }) => {
    if (orderId !== "A-1001") throw new ToolError(`No order ${orderId}`); // the model sees this
    return { status: "shipped", eta: "tomorrow" };
  },
});

const agent = new Agent({
  model: "openai:gpt-5-mini", // key from OPENAI_API_KEY
  instructions: "You are a support agent. Be brief.",
  tools: [getOrder],
});

const result = await agent.run("Where is order A-1001?");
console.log(result.text);
// [/show]
