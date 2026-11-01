// Package smoke test (design 0012 §5): run against the installed tarball by
// scripts/test-typescript.sh, using only the public API (as an ES module).
// Simulated models only.

import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import fs from "node:fs";
import { END, Agent, FakeModel, FakeReply, Storage, ToolError, ValidationError, Workflow, patterns, specialists, suspend, tool } from "criogen";

const require = createRequire(import.meta.url);
const resolved = require.resolve("criogen");
if (!resolved.includes("node_modules")) throw new Error(`not the installed package: ${resolved}`);

const lookup = tool({
  name: "lookup",
  description: "Looks up an order.",
  input: { type: "object", properties: { orderId: { type: "string" } }, required: ["orderId"] },
  run: async ({ orderId }) => {
    if (orderId !== "A-1") throw new ToolError(`No order ${orderId}`);
    return { id: orderId, status: "shipped" };
  },
});

const file = path.join(os.tmpdir(), `criogen-consumer-${process.pid}.db`);
const storage = new Storage({ url: `sqlite:///${file}`, audit: true });
const agent = new Agent({
  model: new FakeModel([FakeReply.toolCall("lookup", { orderId: "A-1" }), "It shipped."]),
  tools: [lookup],
  approve: ["lookup"],
  storage,
});
const chat = agent.thread("consumer", { actor: "user:test" });
const reply = await chat.send("Where is A-1?");
if (!reply.needsApproval) throw new Error("expected an approval");
const stream = chat.streamApprove();
const kinds = [];
for await (const event of stream) kinds.push(event.type);
if (!kinds.includes("tool_call_finished")) throw new Error(`events: ${kinds}`);
if ((await stream.reply()).text !== "It shipped.") throw new Error("wrong reply");
if (!(await storage.verifyAudit()).ok) throw new Error("audit chain broken");
try {
  await chat.decide({ nope: { type: "approve" } });
  throw new Error("an invalid decision was accepted");
} catch (error) {
  if (!(error instanceof ValidationError) || error.code !== "resume_invalid") throw error;
}
// Best effort: Windows keeps the file locked while the storage is open.
for (const suffix of ["", "-wal", "-shm"]) {
  try {
    fs.rmSync(file + suffix, { force: true });
  } catch {}
}
// A workflow that pauses for a person, then resumes (design 0015).
const app = new Workflow()
  .node("ask", (s, ctx) => (ctx.resume === undefined ? suspend({ refund: s.amount }) : { ok: ctx.resume }))
  .start("ask")
  .edge("ask", END)
  .compile();
const paused = await app.run({ amount: 40 });
if (!paused.suspended || paused.interrupts[0].payload.refund !== 40) throw new Error("expected a pause");
if ((await app.resume(paused.snapshot, { ask: true })).state.ok !== true) throw new Error("resume failed");
// Patterns and specialists (design 0019): a one-line pipeline and a review loop.
const piped = await patterns.quick("researcher > writer", { model: new FakeModel(["notes", "article"]) }).run("topic");
if (piped.trace.map((t) => t.step).join() !== "researcher,writer" || piped.stopReason !== "done") throw new Error("pipeline failed");
const reviewed = await patterns
  .reviewAndCritique(new Agent({ model: new FakeModel(["draft"]) }), new Agent({ model: new FakeModel(["APPROVED"]) }))
  .run("write");
if (reviewed.stopReason !== "approved" || !specialists.available().includes("researcher")) throw new Error("review failed");
console.log("typescript package test: ok");
