// Google Vertex AI (design 0006 §19) from TypeScript, against a local server.
// No network.

const test = require("node:test");
const assert = require("node:assert/strict");
const http = require("node:http");
const path = require("node:path");

const sdk = require(process.env.CRIOGEN_SDK || path.join(__dirname, ".."));
const { Agent, vertex } = sdk;
const { modelSpec } = require(process.env.CRIOGEN_SDK ? `${process.env.CRIOGEN_SDK}/dist/models` : "../dist/models");

const serverOnly = process.env.CRIOGEN_ENGINE === "wasm" ? "server only: a local Vertex server" : false;

const TEXT = "Hi from Vertex";
const CLAUDE_STREAM = [
  { type: "message_start", message: { id: "m", type: "message", role: "assistant", model: "c", content: [], usage: { input_tokens: 1, output_tokens: 0 } } },
  { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
  { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: TEXT } },
  { type: "content_block_stop", index: 0 },
  { type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: 1 } },
  { type: "message_stop" },
];

const seen = [];
const server = http.createServer((req, res) => {
  let raw = "";
  req.on("data", (c) => (raw += c));
  req.on("end", () => {
    seen.push({ path: req.url, auth: req.headers.authorization, body: JSON.parse(raw || "{}") });
    res.writeHead(200, { "content-type": "text/event-stream", connection: "close" });
    if (req.url.includes(":streamRawPredict")) {
      for (const e of CLAUDE_STREAM) res.write(`data: ${JSON.stringify(e)}\n\n`);
      return res.end();
    }
    res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { role: "assistant", content: TEXT } }] })}\n\n`);
    res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: "stop" }] })}\n\n`);
    res.end("data: [DONE]\n\n");
  });
});
let host;
test.before(async () => {
  if (serverOnly) return;
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  host = `http://127.0.0.1:${server.address().port}`;
});
test.after(() => server.close());

test("Claude goes to streamRawPredict with the version in the body", { skip: serverOnly }, async () => {
  seen.length = 0;
  const model = vertex("claude-sonnet-5", { project: "my-project", location: "global", token: "ya29.t", host });
  assert.equal((await new Agent({ model }).run("hi")).text, TEXT);
  assert.equal(seen[0].path, "/v1/projects/my-project/locations/global/publishers/anthropic/models/claude-sonnet-5:streamRawPredict");
  assert.equal(seen[0].auth, "Bearer ya29.t");
  assert.equal(seen[0].body.anthropic_version, "vertex-2023-10-16");
  assert.equal(seen[0].body.model, undefined);
});

test("Gemini goes to the OpenAI-compatible endpoint", { skip: serverOnly }, async () => {
  seen.length = 0;
  const model = vertex("google/gemini-3.6-flash", { project: "my-project", location: "europe-west4", token: "t", host });
  assert.equal((await new Agent({ model }).run("hi")).text, TEXT);
  assert.equal(seen[0].path, "/v1/projects/my-project/locations/europe-west4/endpoints/openapi/chat/completions");
});

test("strings and options", () => {
  assert.deepEqual(modelSpec("vertex:claude-sonnet-5"), { kind: "vertex", model: "claude-sonnet-5" });
  assert.throws(() => vertex("m", { api: "gemini" }), /auto/);
  assert.ok(!JSON.stringify(vertex("m", { token: "ya29.secret" })).includes("ya29.secret"));
});
