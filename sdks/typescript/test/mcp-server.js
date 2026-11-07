// A scripted MCP server for the tests (design 0022): `node mcp-server.js
// modern|legacy` speaks stdio; `serveHttp(mode)` serves Streamable HTTP on a
// local port. Never the network.

const http = require("node:http");
const readline = require("node:readline");

const VERSION = "2026-07-28";
const TOOLS = [
  { name: "echo", description: "Says it back.", inputSchema: { type: "object", properties: { text: { type: "string" } }, required: ["text"] }, annotations: { readOnlyHint: true } },
  { name: "files.read", description: "Reads a file.", inputSchema: { type: "object", properties: { path: { type: "string" } } } },
  { name: "fail", description: "Always fails.", inputSchema: { type: "object" }, annotations: { destructiveHint: true } },
];

function fake(mode) {
  const legacy = mode === "legacy";
  let initialized = false;
  return (msg) => {
    const { method, id } = msg;
    if (id === undefined) {
      if (method === "notifications/initialized") initialized = true;
      return null;
    }
    const params = msg.params || {};
    const ok = (result) => ({ jsonrpc: "2.0", id, result });
    const err = (code, message) => ({ jsonrpc: "2.0", id, error: { code, message } });
    if (method === "server/discover") {
      if (legacy) return err(-32601, "Method not found");
      return ok({
        supportedVersions: [VERSION],
        capabilities: { tools: {} },
        _meta: { "io.modelcontextprotocol/serverInfo": { name: "js-fake", version: "1" } },
        instructions: "Use echo.",
      });
    }
    if (method === "initialize" && legacy) {
      return ok({ protocolVersion: params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: "js-legacy", version: "1" } });
    }
    if (!legacy && (params._meta || {})["io.modelcontextprotocol/protocolVersion"] !== VERSION) return err(-32600, "missing _meta");
    if (legacy && !initialized) return err(-32002, "not initialized");
    if (method === "tools/list") return ok({ tools: TOOLS });
    if (method === "tools/call") {
      const args = params.arguments || {};
      if (params.name === "echo") return ok({ content: [{ type: "text", text: args.text ?? "" }] });
      if (params.name === "files.read") return ok({ content: [], structuredContent: { path: args.path, size: 3 } });
      if (params.name === "fail") return ok({ content: [{ type: "text", text: "disk full" }], isError: true });
      return err(-32602, `unknown tool ${params.name}`);
    }
    return err(-32601, "Method not found");
  };
}

function serveHttp(mode) {
  const handle = fake(mode);
  const seen = [];
  const server = http.createServer((req, res) => {
    let raw = "";
    req.on("data", (c) => (raw += c));
    req.on("end", () => {
      seen.push(req.headers);
      const reply = handle(JSON.parse(raw));
      const data = Buffer.from(reply === null ? "" : JSON.stringify(reply));
      res.writeHead(reply === null ? 202 : 200, { "content-type": "application/json", "content-length": data.length });
      res.end(data);
    });
  });
  return new Promise((resolve) =>
    server.listen(0, "127.0.0.1", () => resolve({ url: `http://127.0.0.1:${server.address().port}/mcp`, seen, close: () => server.close() })),
  );
}

module.exports = { serveHttp };

if (require.main === module) {
  const handle = fake(process.argv[2] || "modern");
  readline.createInterface({ input: process.stdin }).on("line", (line) => {
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      return;
    }
    const reply = handle(msg);
    if (reply !== null) process.stdout.write(JSON.stringify(reply) + "\n");
  });
}
