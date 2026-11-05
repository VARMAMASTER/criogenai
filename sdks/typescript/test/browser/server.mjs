// Two local servers for the browser tests (design 0021), never the network:
//
// - the page's origin: the built browser bundle (dist-web, or an installed
//   package's) and the test pages;
// - a provider on another origin (another port), so CORS applies as it does
//   to a real one: an OpenAI-compatible /v1/chat/completions (JSON and SSE),
//   plus routes that misbehave on purpose: a redirect, an error that echoes
//   the key, no CORS, a body over the size limit, a 503 then 200.

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";

export const KEY = "sk-browser-test-key-0123456789abcdef";

const TYPES = { ".js": "text/javascript", ".mjs": "text/javascript", ".html": "text/html", ".wasm": "application/wasm", ".map": "application/json", ".json": "application/json" };

function listen(server) {
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve(`http://127.0.0.1:${server.address().port}`)));
}

function body(req) {
  return new Promise((resolve) => {
    const parts = [];
    req.on("data", (c) => parts.push(c));
    req.on("end", () => resolve(Buffer.concat(parts).toString("utf8")));
  });
}

/** `files`: the directory with criogen.js and criogen_bg.wasm. */
export async function startServers({ files }) {
  const here = new URL(".", import.meta.url);
  const pages = new Map([
    ["/page.html", new URL("page.html", here)],
    ["/tests.js", new URL("tests.js", here)],
    ["/worker.js", new URL("worker.js", here)],
  ]);
  const site = createServer(async (req, res) => {
    const path = new URL(req.url, "http://x").pathname;
    try {
      let file = pages.get(path);
      if (!file) {
        const safe = normalize(path).replace(/^([/\\])+/, "");
        if (safe.includes("..")) throw new Error("no");
        file = join(files, safe);
      }
      const data = await readFile(file);
      res.writeHead(200, { "content-type": TYPES[extname(String(file))] ?? "application/octet-stream", "cache-control": "no-store" });
      res.end(data);
    } catch {
      res.writeHead(404).end("not found");
    }
  });

  const stats = { requests: 0, auth: [], stolen: 0, flaky: 0, bodies: [] };
  const cors = {
    "access-control-allow-origin": "*",
    "access-control-allow-methods": "POST, GET, OPTIONS",
    "access-control-allow-headers": "authorization, content-type, x-api-key, anthropic-version",
    "access-control-max-age": "600",
  };
  let siteUrl = "";
  let providerUrl = "";
  const provider = createServer(async (req, res) => {
    const path = new URL(req.url, "http://x").pathname;
    const noCors = path.startsWith("/nocors/");
    const head = (status, extra = {}) => res.writeHead(status, { ...(noCors ? {} : cors), ...extra });
    if (req.method === "OPTIONS") return head(204).end();
    if (path === "/stats") return head(200, { "content-type": "application/json" }).end(JSON.stringify(stats));
    const text = await body(req);
    stats.requests += 1;
    stats.auth.push(req.headers.authorization ?? null);
    stats.bodies.push(text.length);
    let request = {};
    try {
      request = JSON.parse(text);
    } catch {}
    const json = (status, value) => head(status, { "content-type": "application/json" }).end(JSON.stringify(value));
    if (path.startsWith("/redirect/")) return head(307, { location: `${providerUrl}/stolen/v1/chat/completions` }).end();
    if (path.startsWith("/stolen/")) {
      stats.stolen += 1;
      return json(200, completion(request, "stolen"));
    }
    if (path.startsWith("/leak/")) return json(401, { error: { message: `invalid key: ${req.headers.authorization}` } });
    if (path.startsWith("/huge/")) {
      head(200, { "content-type": "application/json" });
      const chunk = Buffer.alloc(1024 * 1024, 32);
      let sent = 0;
      const more = () => {
        while (sent < 70) {
          sent += 1;
          if (!res.write(chunk)) return res.once("drain", more);
        }
        res.end();
      };
      res.on("error", () => {});
      return more();
    }
    if (path.startsWith("/flaky/")) {
      stats.flaky += 1;
      if (stats.flaky % 2 === 1) return json(503, { error: { message: "busy, try again" } });
    }
    if (!path.endsWith("/chat/completions")) return json(404, { error: { message: "no such route" } });
    if (request.stream) {
      head(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
      // "count" streams four words; anything else echoes, word by word.
      const said = completion(request).choices[0].message.content;
      const words = said === "you said: count" ? ["one ", "two ", "three ", "four"] : said.split(/(?<= )/);
      const send = (value) => res.write(`data: ${JSON.stringify(value)}\n\n`);
      for (const [i, word] of words.entries()) {
        send({ id: "c1", object: "chat.completion.chunk", model: request.model, choices: [{ index: 0, delta: i === 0 ? { role: "assistant", content: word } : { content: word }, finish_reason: null }] });
        await new Promise((r) => setTimeout(r, 15));
      }
      send({ id: "c1", object: "chat.completion.chunk", model: request.model, choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 3, completion_tokens: 4, total_tokens: 7 } });
      res.write("data: [DONE]\n\n");
      return res.end();
    }
    return json(200, completion(request));
  });
  siteUrl = await listen(site);
  providerUrl = await listen(provider);
  return {
    siteUrl,
    providerUrl,
    stats,
    close: () => Promise.all([new Promise((r) => site.close(r)), new Promise((r) => provider.close(r))]),
  };
}

function completion(request, prefix = "you said") {
  const messages = Array.isArray(request.messages) ? request.messages : [];
  const last = [...messages].reverse().find((m) => m.role === "user");
  const content = typeof last?.content === "string" ? last.content : (last?.content ?? []).map((b) => b.text ?? "").join("");
  return {
    id: "c1",
    object: "chat.completion",
    created: 0,
    model: request.model ?? "mock",
    choices: [{ index: 0, message: { role: "assistant", content: `${prefix}: ${content}` }, finish_reason: "stop" }],
    usage: { prompt_tokens: 3, completion_tokens: 3, total_tokens: 6 },
  };
}
