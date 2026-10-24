// The same chat in a browser: `npm run web`, then open http://localhost:4500
// (one conversation per name; reload the page, restart the server: it continues).

import { createServer } from "node:http";
import { readFileSync } from "node:fs";
import { createChat, cleanName } from "./chat.mjs";

const chat = createChat({ file: process.env.CHAT_DB ?? "chat.db" });
const page = readFileSync(new URL("./index.html", import.meta.url));
const port = Number(process.env.PORT ?? 4500);

function readBody(request, limit = 64 * 1024) {
  return new Promise((resolve, reject) => {
    let size = 0;
    const parts = [];
    request.on("data", (chunk) => {
      size += chunk.length;
      if (size > limit) {
        reject(new Error("too large"));
        request.destroy();
      } else parts.push(chunk);
    });
    request.on("end", () => resolve(Buffer.concat(parts).toString("utf8")));
    request.on("error", reject);
  });
}

const json = (response, status, body) => {
  response.writeHead(status, { "content-type": "application/json" });
  response.end(JSON.stringify(body));
};

const server = createServer(async (request, response) => {
  const url = new URL(request.url, "http://localhost");
  try {
    if (request.method === "GET" && url.pathname === "/") {
      response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      return response.end(page);
    }
    if (request.method === "GET" && url.pathname === "/api/messages") {
      return json(response, 200, await chat.recent(cleanName(url.searchParams.get("name")), 50));
    }
    if (request.method === "POST" && url.pathname === "/api/send") {
      const { name, text } = JSON.parse(await readBody(request));
      if (typeof text !== "string" || !text.trim() || text.length > 8000) throw new Error("say something (up to 8000 characters)");
      const person = cleanName(name); // refused with a 400 before anything streams
      response.writeHead(200, { "content-type": "text/plain; charset=utf-8", "cache-control": "no-cache" });
      // The answer streams as plain text; leaving early stops the run.
      await chat.say(person, text.trim(), (piece) => response.write(piece));
      return response.end();
    }
    if (request.method === "POST" && url.pathname === "/api/forget") {
      const { name } = JSON.parse(await readBody(request));
      await chat.forget(cleanName(name));
      return json(response, 200, { ok: true });
    }
    json(response, 404, { error: "not found" });
  } catch (error) {
    if (!response.headersSent) json(response, 400, { error: error.message });
    else response.end(`\n(failed: ${error.message})`);
  }
});

// Only this computer can open it, unless you set HOST yourself.
server.listen(port, process.env.HOST ?? "127.0.0.1", () => console.log(`chat on http://localhost:${port}`));
process.on("SIGINT", async () => {
  await chat.close();
  process.exit(0);
});
