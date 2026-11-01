// Live check of the TypeScript SDK against real models (design 0012 §5).
// Manual only: needs API keys (OPENAI_API_KEY / GEMINI_API_KEY, or KEY=value
// lines in ~/.criogen-keys.env) and costs a few cents. Never run in CI.
//
//   node sdks/typescript/examples/live.mjs [openai] [gemini]

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { Agent, Storage, gemini, openai, tool } = require("..");

const keysFile = process.env.CRIOGEN_KEYS_FILE || path.join(os.homedir(), ".criogen-keys.env");
if (fs.existsSync(keysFile)) {
  for (const line of fs.readFileSync(keysFile, "utf8").split(/\r?\n/)) {
    const at = line.indexOf("=");
    if (at > 0 && !process.env[line.slice(0, at)]) process.env[line.slice(0, at)] = line.slice(at + 1);
  }
}

async function check(name, makeModel) {
  let failures = 0;
  const weatherCalls = [];
  const refunds = [];
  const weather = tool({
    name: "weather",
    description: "Current temperature in a city, in Celsius.",
    input: { type: "object", properties: { city: { type: "string" } }, required: ["city"] },
    run: async ({ city }) => {
      weatherCalls.push(city);
      return { city, celsius: 17 };
    },
  });
  const refund = tool({
    name: "refund",
    description: "Refunds an order.",
    input: { type: "object", properties: { orderId: { type: "string" } }, required: ["orderId"] },
    destructive: true,
    run: ({ orderId }) => {
      refunds.push(orderId);
      return `refunded ${orderId}`;
    },
  });
  const assert = (ok, why) => {
    if (!ok) throw new Error(why);
  };
  const model = makeModel();
  const cases = {
    "tools: async JS tool": async () => {
      const agent = new Agent({ model, tools: [weather], instructions: "Use tools when they help. Be brief." });
      const result = await agent.run("What is the temperature in Paris right now?");
      assert(weatherCalls.length > 0, "the tool was not called");
      assert((result.text ?? "").includes("17"), result.text);
      return result.text.slice(0, 80);
    },
    "threads: memory across turns": async () => {
      const chat = new Agent({ model, storage: new Storage() }).thread("live");
      await chat.send("Hi! My name is Ada and my favourite colour is teal.");
      const text = ((await chat.send("What is my name and favourite colour?")).text ?? "").toLowerCase();
      assert(text.includes("ada") && text.includes("teal"), text);
      return text.slice(0, 80);
    },
    "approval: approve runs the tool": async () => {
      const agent = new Agent({ model, tools: [refund], approve: ["refund"], storage: new Storage({ audit: true }) });
      const chat = agent.thread("refunds", { actor: "user:live" });
      let reply = await chat.send("Please refund order A-1001 now.");
      assert(reply.needsApproval, reply.text);
      assert(refunds.length === 0, "ran before approval");
      reply = await chat.approve();
      assert(refunds.join() === "A-1001", String(refunds));
      return (reply.text ?? "").slice(0, 80);
    },
    "stream: text in pieces": async () => {
      const stream = new Agent({ model }).thread("stream").stream("Count from 1 to 5, separated by spaces.");
      const pieces = [];
      for await (const event of stream) if (event.type === "text") pieces.push(event.text);
      const reply = await stream.reply();
      assert(pieces.join("").trim() === (reply.text ?? "").trim(), "pieces differ from the reply");
      return `${pieces.length} pieces`;
    },
  };
  console.log(`\n=== typescript: ${name} ===`);
  for (const [label, run] of Object.entries(cases)) {
    const started = Date.now();
    try {
      const detail = await run();
      const line = String(detail).replace(/\s+/g, " ");
      console.log(`PASS ${label.padEnd(36)} ${((Date.now() - started) / 1000).toFixed(1)}s  ${line}`);
    } catch (error) {
      failures += 1;
      console.log(`FAIL ${label.padEnd(36)} ${((Date.now() - started) / 1000).toFixed(1)}s  ${error}`);
    }
  }
  console.log(`--- ${name}: ${failures} failed`);
  return failures;
}

const wanted = process.argv.slice(2);
const providers = [];
if (process.env.OPENAI_API_KEY) providers.push(["openai", () => openai(process.env.OPENAI_MODEL || "gpt-5-mini")]);
if (process.env.GEMINI_API_KEY) providers.push(["gemini", () => gemini(process.env.GEMINI_MODEL || "gemini-2.5-flash")]);
const chosen = providers.filter(([name]) => wanted.length === 0 || wanted.includes(name));
if (chosen.length === 0) {
  console.error("no provider: set OPENAI_API_KEY and/or GEMINI_API_KEY");
  process.exit(2);
}
let failed = 0;
for (const [name, make] of chosen) failed += await check(name, make);
process.exit(failed ? 1 : 0);
