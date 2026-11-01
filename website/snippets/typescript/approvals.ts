import { tool } from "criogen";

const issueRefund = tool({
  name: "issue_refund",
  description: "Refund an order.",
  input: { type: "object" },
  run: () => "refunded",
});

// [show]
import { Agent, Decision, Storage } from "criogen";

const agent = new Agent({
  model: "openai:gpt-5-mini",
  tools: [issueRefund],
  approve: ["issue_refund"], // a person decides before it runs
  storage: new Storage("sqlite:///support.db"),
});

const chat = agent.thread("ticket-7", { actor: "user:ada" });
let reply = await chat.send("Order A-1001 arrived broken, please refund it");
if (reply.needsApproval) {
  for (const request of reply.approvals) console.log(request.tool, request.args);
}

// Hours later, in another process: the approval waited in the database.
reply = await agent.thread("ticket-7", { actor: "user:dr-rao" }).approve();
// Or: await chat.reject("Refund policy says no")
//     await chat.decide({ [callId]: Decision.edit({ orderId: "A-1001", amount: 12 }) })
// [/show]
void Decision.approve();
