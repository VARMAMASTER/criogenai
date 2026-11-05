// Bundles app.js against the installed package (esbuild, as an app's bundler
// would), serves it with the engine file next to the bundle, and runs it in
// Chromium. Usage: node run.mjs <project dir with node_modules/criogen> <sdk dir>
import { createServer } from "node:http";
import { copyFileSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { extname, join, resolve } from "node:path";

const [project, sdk] = process.argv.slice(2).map((p) => resolve(p));
const fromSdk = createRequire(join(sdk, "package.json"));
const esbuild = fromSdk("esbuild");
const { chromium } = fromSdk("playwright");

await esbuild.build({
  entryPoints: [join(project, "app.js")],
  bundle: true,
  format: "esm",
  platform: "browser",
  outfile: join(project, "out.js"),
  nodePaths: [join(project, "node_modules")],
  logLevel: "warning",
});
// What a bundler without asset handling needs: the engine next to the bundle.
copyFileSync(join(project, "node_modules", "criogen", "dist-web", "criogen_bg.wasm"), join(project, "criogen_bg.wasm"));

const types = { ".html": "text/html", ".js": "text/javascript", ".wasm": "application/wasm" };
const server = createServer((req, res) => {
  const name = new URL(req.url, "http://x").pathname.replace(/^\/+/, "") || "index.html";
  if (!["index.html", "out.js", "criogen_bg.wasm"].includes(name)) return res.writeHead(404).end();
  res.writeHead(200, { "content-type": types[extname(name)] }).end(readFileSync(join(project, name)));
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const browser = await chromium.launch();
try {
  const page = await browser.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);
  await page.waitForFunction(() => window.__result !== undefined, null, { timeout: 60_000 });
  const result = await page.evaluate(() => window.__result);
  const ok = result.ok && result.text === "the answer is 42" && result.toolCalls === 1 && result.doubled === 42 && result.refused && result.engine.arch === "wasm32" && errors.length === 0;
  console.log(`web consumer: ${ok ? "ok" : "FAILED"} ${JSON.stringify(result)}${errors.length ? ` page errors: ${errors}` : ""}`);
  process.exitCode = ok ? 0 : 1;
} finally {
  await browser.close();
  server.close();
}
