// Package smoke test (design 0012 §5): run against the installed tarball by
// scripts/test-typescript.sh, using only the public API (as an ES module).
// Simulated models only.

import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import fs from "node:fs";
import { Agent, FakeModel, FakeReply, Storage, ToolError, ValidationError, tool } from "@agentbuild/sdk";

const require = createRequire(import.meta.url);
const resolved = require.resolve("@agentbuild/sdk");
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

const file = path.join(os.tmpdir(), `agentbuild-consumer-${process.pid}.db`);
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
console.log("typescript package test: ok");
