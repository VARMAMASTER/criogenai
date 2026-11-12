// The tutorial's terminal chat (docs: tutorial/support-agent).
// [show]
import * as readline from "node:readline/promises";
import { Storage, loadEnv, type Reply, type Thread } from "criogenai";
import { buildAgent } from "./support_agent.js";

loadEnv(); // OPENAI_API_KEY from .env, if there is one

const terminal = readline.createInterface({ input: process.stdin, output: process.stdout });

/** Asks a staff member about every refund the agent wants to make. */
async function settleApprovals(desk: Thread, reply: Reply): Promise<Reply> {
  while (reply.needsApproval) {
    for (const request of reply.approvals) console.log(`  [staff] ${request.tool}`, request.args);
    const answer = await terminal.question("  [staff] approve? y/n > ");
    reply = answer.trim().toLowerCase() === "y"
      ? await desk.approve()
      : await desk.reject("A staff member declined the refund.");
  }
  return reply;
}

export async function main(customerId: string, ticketId: string): Promise<void> {
  const storage = new Storage("sqlite://support.db");
  const agent = buildAgent("openai:gpt-5-mini", storage);
  const chat = agent.thread(ticketId, { actor: `customer:${customerId}` });
  const desk = agent.thread(ticketId, { actor: "staff:sam" }); // same thread, another person
  try {
    for (;;) {
      const text = (await terminal.question("you > ")).trim();
      if (text === "" || text === "quit") break;
      let reply = await chat.send(text, { state: { customer_id: customerId } });
      reply = await settleApprovals(desk, reply);
      console.log("agent >", reply.text);
    }
  } finally {
    terminal.close();
    await storage.close();
  }
}

// npx tsx support_chat.ts <customer id> <ticket id>
await main(process.argv[2] ?? "c-7", process.argv[3] ?? "ticket-1");
// [/show]

// [show stream]
/** send(), printing the answer as it is written. */
export async function sendStreaming(chat: Thread, text: string, customerId: string): Promise<Reply> {
  const stream = chat.stream(text, { state: { customer_id: customerId } });
  process.stdout.write("agent > ");
  for await (const event of stream) {
    if (event.type === "text") process.stdout.write(event.text);
    else if (event.type === "tool_call_started") process.stdout.write(`[${event.tool}] `);
  }
  process.stdout.write("\n");
  return stream.reply(); // the same saved reply send() returns
}
// [/show stream]
