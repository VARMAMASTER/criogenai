// The trace viewer's page in a real browser (design 0023): runs listed, a run
// opened with its waterfall and details, the live refresh leaving rows still,
// and a span named like HTML shown as text, never run.
//
// Usage: node test/browser/trace-page.mjs   (after `npm run build:native && npm run build`)
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const sdk = require(join(here, "..", ".."));
const { chromium } = require("playwright");

let failed = 0;
const check = (ok, what) => {
  console.log(`  ${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) failed += 1;
};

const hex = (n, len) => n.toString(16).padStart(len, "0");
const now = BigInt(Date.now()) * 1_000_000n;
const ns = (ms) => (now + BigInt(ms) * 1_000_000n).toString();
const attr = (key, value) => ({ key, value: typeof value === "number" ? { intValue: String(value) } : { stringValue: value } });
const trace = hex(0xabc, 32);
const spans = [
  { traceId: trace, spanId: hex(1, 16), name: "<img src=x onerror=alert(1)>", startTimeUnixNano: ns(0), endTimeUnixNano: ns(900),
    attributes: [attr("criogen.status", "completed"), attr("langfuse.trace.name", "<img src=x onerror=alert(1)>")] },
  { traceId: trace, spanId: hex(2, 16), parentSpanId: hex(1, 16), name: "chat gpt-5-mini", startTimeUnixNano: ns(10), endTimeUnixNano: ns(600),
    attributes: [attr("langfuse.observation.type", "generation"), attr("gen_ai.request.model", "gpt-5-mini"), attr("gen_ai.system", "openai"),
      attr("gen_ai.usage.input_tokens", 1200), attr("gen_ai.usage.output_tokens", 300)] },
  { traceId: trace, spanId: hex(3, 16), parentSpanId: hex(1, 16), name: "weather", startTimeUnixNano: ns(600), endTimeUnixNano: ns(800),
    attributes: [attr("langfuse.observation.type", "tool"), attr("gen_ai.tool.name", "weather"), attr("langfuse.observation.level", "WARNING"),
      attr("langfuse.observation.status_message", "tool_error: <b>down</b>")] },
];

const viewer = await sdk.serveTraces({ port: 0, db: ":memory:", prices: { "gpt-5-mini": { input: 1, output: 2 } } });
const browser = await chromium.launch();
try {
  const posted = await fetch(`${viewer.url}/v1/traces`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ resourceSpans: [{ scopeSpans: [{ spans }] }] }),
  });
  check(posted.ok, "the receiver takes OTLP JSON");

  const page = await browser.newPage();
  const dialogs = [];
  const errors = [];
  page.on("dialog", (d) => { dialogs.push(d.message()); d.dismiss(); });
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto(viewer.url);
  await page.waitForSelector("tbody tr");
  check((await page.locator("tbody tr").count()) === 1, "the run is listed");
  const cell = await page.locator("tbody tr td").nth(1).textContent();
  check(cell.includes("<img src=x onerror=alert(1)>"), "an HTML-like name is shown as text");
  check((await page.locator("tbody img").count()) === 0, "and no element is made from it");
  check((await page.locator("tbody tr").textContent()).includes("1 warning"), "the tool's warning is counted");
  check((await page.locator("tbody tr").textContent()).includes("$0.00180"), "cost from the prices");

  // The live refresh leaves an unchanged table alone.
  const marker = await page.locator("tbody tr").evaluate((tr) => { tr.dataset.mark = "1"; return true; });
  await page.waitForTimeout(2500);
  check(marker && (await page.locator("tbody tr[data-mark='1']").count()) === 1, "rows stay put between refreshes");

  await page.locator("tbody tr").click();
  await page.waitForSelector(".row");
  check((await page.locator(".row").count()) === 3, "the waterfall has every step");
  await page.locator(".row", { hasText: "weather" }).click();
  const aside = await page.locator("aside").textContent();
  check(aside.includes("tool_error: <b>down</b>") && (await page.locator("aside b").count()) === 0, "details show the message as text");
  check((await page.locator(".legend").textContent()).includes("Model calls 590 ms"), "the time split");
  check(dialogs.length === 0 && errors.length === 0, `no dialog, no page error ${JSON.stringify({ dialogs, errors })}`);
} catch (error) {
  failed += 1;
  console.log(`  FAIL ${error?.stack ?? error}`);
} finally {
  await browser.close();
  await viewer.stop();
}
console.log(failed ? `trace page: ${failed} failed` : "trace page: all passed");
process.exit(failed ? 1 : 0);
