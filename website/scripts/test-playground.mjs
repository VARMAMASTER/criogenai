// The playground on the built site (out/), in Chromium (design 0021):
//
//   1. every pattern runs its code sample on the engine in the browser, with
//      the scripted model, to an answer, without a page error;
//   2. with a visitor's key, the engine calls the provider from the page:
//      OpenAI and Anthropic are answered here (Playwright), never the
//      network. The agent's tools reach the provider, the key goes only to
//      it (as its header), and it is stored nowhere.
//
// Usage: node scripts/test-playground.mjs   (after `npm run build`)
import { createServer } from "node:http";
import { readFile, readdir, stat } from "node:fs/promises";
import { createRequire } from "node:module";
import { extname, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const site = resolve(fileURLToPath(new URL("..", import.meta.url)));
const out = join(site, "out");
const { chromium } = createRequire(join(site, "..", "sdks", "typescript", "package.json"))("playwright");
const TYPES = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".wasm": "application/wasm", ".json": "application/json", ".txt": "text/plain", ".svg": "image/svg+xml", ".woff2": "font/woff2", ".png": "image/png" };

const server = createServer(async (req, res) => {
  let path = normalize(decodeURIComponent(new URL(req.url, "http://x").pathname)).replace(/^([/\\])+/, "");
  if (path.includes("..")) return res.writeHead(400).end();
  let file = join(out, path);
  try {
    if ((await stat(file)).isDirectory()) file = join(file, "index.html");
    res.writeHead(200, { "content-type": TYPES[extname(file)] ?? "application/octet-stream" }).end(await readFile(file));
  } catch {
    res.writeHead(404).end("not found");
  }
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const base = `http://127.0.0.1:${server.address().port}`;
const ids = (await readdir(join(out, "playground"), { withFileTypes: true })).filter((d) => d.isDirectory() && !d.name.startsWith("__")).map((d) => d.name);

const browser = await chromium.launch();
let failed = 0;
const fail = (what) => {
  failed += 1;
  console.log(`  FAIL ${what}`);
};

async function runPattern(page, id) {
  await page.goto(`${base}/playground/${id}/`, { waitUntil: "networkidle" });
  const four = page.getByRole("button", { name: "4×" });
  if (await four.count()) await four.click();
  await page.getByRole("button", { name: /^Run$/ }).click();
  const started = Date.now();
  while (Date.now() - started < 120_000) {
    if (await page.evaluate(() => !!(document.querySelector(".sb-answer") || document.querySelector(".sb-error")))) break;
    const approve = page.getByRole("button", { name: /^Approve$/ });
    if (await approve.count()) await approve.click();
    await page.waitForTimeout(200);
  }
  return page.evaluate(() => ({
    answer: document.querySelector(".sb-answer p")?.textContent ?? null,
    error: document.querySelector(".sb-error")?.textContent ?? null,
    counts: document.querySelector(".sb-counts")?.textContent ?? "",
    mode: document.querySelector(".sb-mode")?.textContent ?? "",
    time: document.querySelector(".sb-time")?.textContent ?? "",
  }));
}

try {
  console.log(`1. ${ids.length} patterns, scripted model`);
  for (const id of ids) {
    const page = await browser.newPage();
    const errors = [];
    page.on("pageerror", (e) => errors.push(e.message));
    const r = await runPattern(page, id);
    await page.close();
    // Every run says where its time went, Criogen's share included.
    const ok = r.answer && !r.error && !errors.length && r.mode.includes("Criogen engine") && r.time.includes("Criogen engine and this page");
    console.log(`  ${ok ? "ok  " : "FAIL"} ${id}: ${r.counts}${ok ? "" : ` | ${r.error ?? ""} ${errors.join("; ")}`}`);
    if (!ok) failed += 1;
  }

  console.log("2. a visitor's key (providers answered here)");
  for (const provider of ["openai", "anthropic"]) {
    const KEY = provider === "openai" ? "sk-test-playground-0123456789" : "sk-ant-test-playground-0123456789";
    const seen = [];
    const page = await browser.newPage();
    const cors = { "access-control-allow-origin": "*", "access-control-allow-headers": "*", "access-control-allow-methods": "POST" };
    await page.route(provider === "openai" ? "https://api.openai.com/**" : "https://api.anthropic.com/**", async (route) => {
      const request = route.request();
      if (request.method() === "OPTIONS") return route.fulfill({ status: 204, headers: cors });
      const body = JSON.parse(request.postData() ?? "{}");
      const headers = request.headers();
      const tools = (body.tools ?? []).map((t) => t.function?.name ?? t.name);
      const answered = provider === "openai" ? body.messages.some((m) => m.role === "tool") : body.messages.some((m) => Array.isArray(m.content) && m.content.some((b) => b.type === "tool_result"));
      seen.push({ tools, key: provider === "openai" ? headers.authorization === `Bearer ${KEY}` : headers["x-api-key"] === KEY, browser: headers["anthropic-dangerous-direct-browser-access"] });
      const call = !answered && tools.includes("weather");
      const text = "A real model's answer: sunny in Lisbon, go on Saturday.";
      let sse;
      if (provider === "openai") {
        const delta = call ? { role: "assistant", tool_calls: [{ index: 0, id: "call_w", type: "function", function: { name: "weather", arguments: '{"city":"Lisbon"}' } }] } : { role: "assistant", content: text };
        sse = [{ choices: [{ index: 0, delta, finish_reason: null }] }, { choices: [{ index: 0, delta: {}, finish_reason: call ? "tool_calls" : "stop" }], usage: { prompt_tokens: 5, completion_tokens: 5, total_tokens: 10 } }]
          .map((c) => `data: ${JSON.stringify({ id: "x", object: "chat.completion.chunk", model: body.model, ...c })}\n\n`)
          .join("") + "data: [DONE]\n\n";
      } else {
        const block = call ? { type: "tool_use", id: "toolu_1", name: "weather", input: {} } : { type: "text", text: "" };
        const delta = call ? { type: "input_json_delta", partial_json: '{"city":"Lisbon"}' } : { type: "text_delta", text };
        sse = [
          ["message_start", { type: "message_start", message: { id: "msg_1", type: "message", role: "assistant", model: body.model, content: [], stop_reason: null, usage: { input_tokens: 5, output_tokens: 0 } } }],
          ["content_block_start", { type: "content_block_start", index: 0, content_block: block }],
          ["content_block_delta", { type: "content_block_delta", index: 0, delta }],
          ["content_block_stop", { type: "content_block_stop", index: 0 }],
          ["message_delta", { type: "message_delta", delta: { stop_reason: call ? "tool_use" : "end_turn" }, usage: { output_tokens: 5 } }],
          ["message_stop", { type: "message_stop" }],
        ]
          .map(([event, data]) => `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`)
          .join("");
      }
      return route.fulfill({ status: 200, headers: { ...cors, "content-type": "text/event-stream" }, body: sse });
    });
    const errors = [];
    page.on("pageerror", (e) => errors.push(e.message));
    await page.goto(`${base}/playground/react/`, { waitUntil: "networkidle" });
    await page.locator("details.sb-key > summary").click();
    await page.locator("details.sb-key select").selectOption(provider);
    await page.locator("details.sb-key input[type=password]").fill(KEY);
    await page.locator("details.sb-key form button[type=submit]").click();
    await page.getByRole("button", { name: /^Run$/ }).click();
    await page.waitForFunction(() => document.querySelector(".sb-answer") || document.querySelector(".sb-error"), null, { timeout: 60_000 });
    const r = await page.evaluate(() => ({
      answer: document.querySelector(".sb-answer p")?.textContent ?? null,
      error: document.querySelector(".sb-error")?.textContent ?? null,
      mode: document.querySelector(".sb-mode")?.textContent ?? "",
      stored: JSON.stringify(localStorage) + JSON.stringify(sessionStorage) + document.cookie + location.href,
    }));
    await page.close();
    const ok =
      r.answer?.startsWith("A real model's answer") &&
      r.mode.startsWith("Real model") &&
      seen.length === 2 &&
      seen.every((s) => s.key && s.tools.includes("weather") && s.tools.includes("calendar")) &&
      (provider === "openai" || seen.every((s) => s.browser === "true")) &&
      !r.stored.includes(KEY) &&
      !errors.length;
    console.log(`  ${ok ? "ok  " : "FAIL"} ${provider}: ${seen.length} calls, tools forwarded, key only in its header, stored nowhere${ok ? "" : ` | ${JSON.stringify({ r: { ...r, stored: undefined }, seen, errors })}`}`);
    if (!ok) failed += 1;
  }
} catch (error) {
  fail(String(error?.stack ?? error));
} finally {
  await browser.close();
  server.close();
}
console.log(failed ? `playground: ${failed} failed` : "playground: all passed");
process.exit(failed ? 1 : 0);
