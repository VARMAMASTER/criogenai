// What the TypeScript SDK's bridge costs (design 0014 §2): agent runs with an
// instant simulated model, with and without one JavaScript tool call.
//
//   node benches/typescript_bridge.mjs [quick]   (after building sdks/typescript)

import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const here = path.dirname(fileURLToPath(import.meta.url));
const { Agent, FakeModel, FakeReply, tool } = require(path.join(here, "..", "sdks", "typescript"));

const input = { type: "object", properties: { text: { type: "string" } }, required: ["text"] };
const echoSync = tool({ name: "echo_sync", description: "Echoes.", input, run: ({ text }) => text });
const echoAsync = tool({ name: "echo_async", description: "Echoes.", input, run: async ({ text }) => text });

const show = (us) => (us >= 1000 ? `${(us / 1000).toFixed(2)} ms` : `${us.toFixed(1)} µs`);

async function measure(name, warmup, runs, script, tools) {
  // One scripted model for every run: runs use its replies in order.
  const replies = [];
  for (let i = 0; i < warmup + runs; i++) replies.push(...script);
  const agent = new Agent({ model: new FakeModel(replies), tools });
  for (let i = 0; i < warmup; i++) await agent.run("go");
  const samples = [];
  for (let i = 0; i < runs; i++) {
    const started = process.hrtime.bigint();
    await agent.run("go");
    samples.push(Number(process.hrtime.bigint() - started) / 1000);
  }
  samples.sort((a, b) => a - b);
  const median = samples[Math.floor(samples.length / 2)];
  return [name, runs, median, samples[Math.round((runs - 1) * 0.95)]];
}

const quick = process.argv.includes("quick");
const [warmup, runs] = quick ? [20, 200] : [200, 2000];
const call = FakeReply.toolCall;
const results = [
  await measure("typescript: agent run, no tool", warmup, runs, ["done"], []),
  await measure("typescript: agent run, 1 sync JS tool", warmup, runs, [call("echo_sync", { text: "x" }), "done"], [echoSync]),
  await measure("typescript: agent run, 1 async JS tool", warmup, runs, [call("echo_async", { text: "x" }), "done"], [echoAsync]),
];
console.log(`Node ${process.versions.node}\n`);
console.log("| Scenario | Runs | Median per run | p95 per run |");
console.log("|---|---|---|---|");
for (const [name, n, median, p95] of results) console.log(`| ${name} | ${n} | ${show(median)} | ${show(p95)} |`);
