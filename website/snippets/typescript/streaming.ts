import { Agent } from "criogen";

declare const agent: Agent;

// [show]
const stream = agent.thread("user-42").stream("Summarise my last order");
for await (const event of stream) {
  if (event.type === "text") process.stdout.write(event.text);
  else if (event.type === "tool_call_started") console.log(`\n[${event.tool}…]`);
  else if (event.type === "approval_needed") console.log("\nwaiting for approval");
}
const reply = await stream.reply(); // saved, like send()
// Breaking out of the loop cancels the run; it is still saved, and retry() continues it.
// [/show]
export {};
