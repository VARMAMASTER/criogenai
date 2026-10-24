import { END, Storage, Workflow, suspend } from "agentbuild-sdk";

// [show]
const app = new Workflow()
  .node("classify", (state) => ({
    category: String(state.text).includes("charge") ? "billing" : "other",
  }))
  .node("approve", (state, ctx) =>
    // Pause for a person; continue when someone answers.
    ctx.resume === undefined ? suspend({ refund: state.amount }) : { approved: ctx.resume === "yes" },
  )
  .node("answer", () => ({ reply: "A human will follow up." }))
  .start("classify")
  .route("classify", (state) => String(state.category), { billing: "approve", other: "answer" })
  .edge("approve", END)
  .edge("answer", END)
  .compile({ storage: new Storage({ url: "sqlite:///app.db" }) }); // every graph problem, reported at once

// Durable: the pause waits in the database, for days if needed.
const turn = await app.thread("ticket-7").run({ text: "I was charged twice", amount: 40 });
if (turn.suspended) {
  // Later, from any process:
  await app.thread("ticket-7", { actor: "user:boss" }).resume({ approve: "yes" });
}
// [/show]
export {};
