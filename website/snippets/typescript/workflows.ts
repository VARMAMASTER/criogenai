import { END, Storage, Workflow, suspend } from "criogen";

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
  .compile({ storage: new Storage({ url: "sqlite://app.db" }) }); // every graph problem, reported at once

// Durable: the pause waits in the database, for days if needed.
const turn = await app.thread("ticket-7").run({ text: "I was charged twice", amount: 40 });
if (turn.suspended) {
  // Later, from any process:
  await app.thread("ticket-7", { actor: "user:boss" }).resume({ approve: "yes" });
}
// [/show]
export {};

// [show parallel]
// Two checks run at the same time; "summary" waits for both.
const checks = new Workflow({ reducers: { findings: "append" } }) // both write "findings": append, not replace
  .node("intake", () => ({ findings: [] }))
  .node("check_allergies", async () => ({ findings: ["no known allergies"] }))
  .node("check_interactions", async () => ({ findings: ["no interactions"] }))
  .node("summary", (state) => ({ report: (state.findings as string[]).join("; ") }))
  .start("intake")
  .edge("intake", "check_allergies")
  .edge("intake", "check_interactions")
  .join(["check_allergies", "check_interactions"], "summary")
  .edge("summary", END)
  .compile();

const checked = await checks.run({ patientRef: "P-1" });
console.log(checked.state.report, checked.steps); // 3 steps: intake, both checks, summary
// [/show parallel]

// [show loop]
import { goto } from "criogen";

const drafting = new Workflow()
  .node("draft", (state) => {
    const round = Number(state.round ?? 0) + 1;
    return { text: `draft ${round}`, round };
  })
  // A node can choose what runs next, among targets declared with .goto().
  .node("review", (state) => (Number(state.round) < 3 ? goto("draft", { feedback: "shorter" }) : goto(END)))
  .start("draft")
  .edge("draft", "review")
  .goto("review", ["draft", END])
  .compile();

// maxSteps bounds any loop; deadlineMs bounds the whole run.
const drafted = await drafting.run({}, { maxSteps: 20, deadlineMs: 30_000 });
console.log(drafted.state.text); // "draft 3"
// [/show loop]

// [show agents]
import { Agent } from "criogen";

const triage = new Agent({ model: "openai:gpt-5-mini", instructions: "Classify the request in one word." });
const writer = new Agent({ model: "openai:gpt-5-mini", instructions: "Write a short, kind reply." });

const desk = new Workflow()
  .addNode("triage", triage) // reads "messages", writes the whole transcript back
  .addNode("writer", writer)
  .start("triage")
  .edge("triage", "writer")
  .edge("writer", END)
  .compile();

const first = { role: "user", content: [{ type: "text", text: "My parcel is late" }] };
const answered = await desk.run({ messages: [first] });
const last = (answered.state.messages as { content: { text?: string }[] }[]).at(-1);
console.log(last?.content[0].text);
// [/show agents]
