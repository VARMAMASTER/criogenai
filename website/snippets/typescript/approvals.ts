import { tool } from "criogenai";

const issueRefund = tool({
  name: "issue_refund",
  description: "Refund an order.",
  input: { type: "object" },
  run: () => "refunded",
});

// [show]
import { Agent, Decision, Storage } from "criogenai";

const agent = new Agent({
  model: "openai:gpt-5-mini",
  tools: [issueRefund],
  approve: ["issue_refund"], // a person decides before it runs
  storage: new Storage("sqlite://support.db"),
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

declare const support: Agent;
// [show decide]
const ticket = support.thread("ticket-9", { actor: "user:dr-rao" });
const pending = await ticket.send("Refund order A-1001 and email the customer");
const decisions: Record<string, Decision> = {};
for (const request of pending.approvals) {
  if (request.tool === "issue_refund" && Number(request.args.amount) > 100) {
    // Run it with other arguments; the model is told what actually ran.
    decisions[request.call_id] = Decision.edit({ ...request.args, amount: 100 });
  } else if (request.tool === "send_email") {
    // Do not run it; this text is the tool's result.
    decisions[request.call_id] = Decision.respond("Not sent: the customer opted out of email.");
  } else {
    decisions[request.call_id] = Decision.approve();
  }
}
const decided = await ticket.decide(decisions); // one decision per pending call, or resume_invalid
// [/show decide]
