// MCP servers' tools in TypeScript (design 0022), against a scripted local server only.

const test = require("node:test");
const assert = require("node:assert/strict");
const path = require("node:path");

const sdk = require(process.env.CRIOGEN_SDK || path.join(__dirname, ".."));
const { Agent, FakeModel, FakeReply, ToolError, isCriogenError, mcp } = sdk;
const { serveHttp } = require("./mcp-server.js");

const SERVER = path.join(__dirname, "mcp-server.js");
const TOKEN = "Bearer js-mcp-secret-5521";
// The WebAssembly engine has no processes: stdio is for servers.
const wasm = process.env.CRIOGEN_ENGINE === "wasm";

test("a stdio server gives an agent its tools", { skip: wasm && "stdio needs a server" }, async () => {
  const server = await mcp.stdio(process.execPath, [SERVER, "modern"], { prefix: "fs" });
  try {
    assert.equal(server.info.protocolVersion, "2026-07-28");
    assert.equal(server.instructions, "Use echo.");
    assert.deepEqual(server.tools.map((t) => t.name), ["fs_echo", "fs_files_read", "fs_fail"]);
    // Hints are the server's claims: not trusted by default.
    assert.equal(server.tool("fs_echo").readOnly, false);
    assert.equal(server.tool("fs_fail").destructive, true);
    const agent = new Agent({ model: new FakeModel([FakeReply.toolCall("fs_echo", { text: "hello" }), "done"]), tools: [server] });
    const result = await agent.run("go");
    const message = result.messages.find((m) => m.role === "tool");
    assert.equal(message.content[0].text, "hello");
    assert.deepEqual(await server.call("fs_files_read", { path: "/a" }), { path: "/a", size: 3 });
    await assert.rejects(server.call("fs_fail"), (e) => e instanceof ToolError && e.message === "disk full");
  } finally {
    await server.close();
  }
});

test("one tool of a legacy server", { skip: wasm && "stdio needs a server" }, async () => {
  const server = await mcp.stdio(process.execPath, [SERVER, "legacy"], { trustAnnotations: true });
  try {
    assert.equal(server.info.protocolVersion, "2025-11-25");
    assert.equal(server.tool("echo").readOnly, true);
    const agent = new Agent({ model: new FakeModel([FakeReply.toolCall("echo", { text: "only me" }), "done"]), tools: [server.tool("echo")] });
    assert.equal((await agent.run("go")).toolCalls, 1);
    assert.equal(await server.tool("echo").call({ text: "direct" }), "direct");
  } finally {
    await server.close();
  }
});

test("an http server gets the protocol headers; the token stays private", async () => {
  const { url, seen, close } = await serveHttp("modern");
  try {
    const server = await mcp.http(url, { headers: { Authorization: TOKEN }, exclude: ["fail"] });
    assert.deepEqual(server.tools.map((t) => t.name), ["echo", "files_read"]);
    assert.equal(await server.call("echo", { text: "over http" }), "over http");
    const last = seen[seen.length - 1];
    assert.equal(last["mcp-method"], "tools/call");
    assert.equal(last["mcp-name"], "echo");
    assert.equal(last.authorization, TOKEN);
    await server.close();
  } finally {
    close();
  }
});

test("failures have stable codes", async () => {
  if (!wasm) {
    await assert.rejects(mcp.stdio("criogen-no-such-command-xyz"), (e) => isCriogenError(e, "mcp_connect"));
  } else {
    await assert.rejects(mcp.stdio("node"), (e) => isCriogenError(e, "unsupported"));
  }
  await assert.rejects(
    mcp.http("http://example.com/mcp", { headers: { Authorization: TOKEN } }),
    (e) => isCriogenError(e, "mcp_connect") && !e.message.includes("secret"),
  );
});
