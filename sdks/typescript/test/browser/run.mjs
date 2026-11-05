// Runs the browser tests (tests.js) in Chromium, Firefox and WebKit (Safari's
// engine) with Playwright, against the built bundle (dist-web) or, with
// --files <dir>, an installed package's (scripts/test-web.sh). Fails if any
// check fails, any page throws, or a browser is missing (unless --allow-missing).
//
// Usage: node test/browser/run.mjs [--files <dir>] [--browsers chromium,firefox,webkit] [--allow-missing]

import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium, firefox, webkit } from "playwright";
import { KEY, startServers } from "./server.mjs";

const args = process.argv.slice(2);
const option = (name) => {
  const at = args.indexOf(name);
  return at === -1 ? undefined : args[at + 1];
};
const root = resolve(fileURLToPath(new URL("../..", import.meta.url)));
const files = resolve(option("--files") ?? resolve(root, "dist-web"));
const wanted = (option("--browsers") ?? "chromium,firefox,webkit").split(",");
const engines = { chromium, firefox, webkit };

const servers = await startServers({ files });
let failed = 0;
const missing = [];
try {
  for (const name of wanted) {
    let browser;
    try {
      browser = await engines[name].launch();
    } catch (error) {
      missing.push(`${name} (${String(error.message).split("\n")[0]})`);
      continue;
    }
    const page = await browser.newPage();
    // api.anthropic.com, answered here: never the network.
    await page.route("https://api.anthropic.com/**", async (route) => {
      const request = route.request();
      const cors = { "access-control-allow-origin": "*", "access-control-allow-headers": "*", "access-control-allow-methods": "POST" };
      if (request.method() === "OPTIONS") return route.fulfill({ status: 204, headers: cors });
      const headers = request.headers();
      const text = `browser-access=${headers["anthropic-dangerous-direct-browser-access"]} key=${headers["x-api-key"] === KEY ? "ok" : "wrong"}`;
      const body = JSON.parse(request.postData() ?? "{}");
      if (!body.stream) {
        return route.fulfill({ status: 200, headers: { ...cors, "content-type": "application/json" }, body: JSON.stringify({ id: "msg_1", type: "message", role: "assistant", model: body.model, content: [{ type: "text", text }], stop_reason: "end_turn", usage: { input_tokens: 3, output_tokens: 3 } }) });
      }
      const events = [
        ["message_start", { type: "message_start", message: { id: "msg_1", type: "message", role: "assistant", model: body.model, content: [], stop_reason: null, usage: { input_tokens: 3, output_tokens: 0 } } }],
        ["content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } }],
        ["content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text } }],
        ["content_block_stop", { type: "content_block_stop", index: 0 }],
        ["message_delta", { type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: 3 } }],
        ["message_stop", { type: "message_stop" }],
      ];
      const sse = events.map(([event, data]) => `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`).join("");
      return route.fulfill({ status: 200, headers: { ...cors, "content-type": "text/event-stream" }, body: sse });
    });
    const problems = [];
    // WebKit reports a request refused by CORS (one test does that on purpose) as a page error.
    page.on("pageerror", (error) => {
      if (!/due to access control checks/.test(error.message)) problems.push(`page error: ${error.message}`);
    });
    page.on("console", (message) => {
      if (message.type() === "error" && !/Failed to load resource|CORS|Access-Control|blocked by CORS|NetworkError|net::ERR/i.test(message.text())) {
        problems.push(`console error: ${message.text()}`);
      }
    });
    const started = Date.now();
    await page.goto(`${servers.siteUrl}/page.html?provider=${encodeURIComponent(servers.providerUrl)}`);
    await page.waitForFunction(() => window.__done === true, null, { timeout: 180_000 });
    const results = await page.evaluate(() => window.__results);
    const version = browser.version();
    await browser.close();
    const bad = results.filter((r) => !r.ok);
    console.log(`\n${name} ${version}: ${results.length - bad.length}/${results.length} passed in ${((Date.now() - started) / 1000).toFixed(1)} s`);
    for (const r of results) console.log(`  ${r.ok ? "ok  " : "FAIL"} ${r.name} (${r.ms} ms)${r.ok ? "" : `\n       ${r.error.split("\n").slice(0, 4).join("\n       ")}`}`);
    for (const p of problems) console.log(`  FAIL ${p}`);
    failed += bad.length + problems.length;
  }
} finally {
  await servers.close();
}
if (missing.length) {
  console.log(`\nmissing browsers: ${missing.join("; ")} (npx playwright install chromium firefox webkit)`);
  if (!args.includes("--allow-missing")) failed += missing.length;
}
console.log(failed ? `\nbrowser tests: ${failed} failed` : "\nbrowser tests: all passed");
process.exit(failed ? 1 : 0);
