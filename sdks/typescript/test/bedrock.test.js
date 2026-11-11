// Amazon Bedrock with an API key (design 0006 §18) from TypeScript, against a
// local server. No network.

const test = require("node:test");
const assert = require("node:assert/strict");
const http = require("node:http");
const path = require("node:path");

const sdk = require(process.env.CRIOGEN_SDK || path.join(__dirname, ".."));
const { Agent, bedrock } = sdk;
const { modelSpec } = require(process.env.CRIOGEN_SDK ? `${process.env.CRIOGEN_SDK}/dist/models` : "../dist/models");

const serverOnly = process.env.CRIOGEN_ENGINE === "wasm" ? "server only: a local Bedrock server" : false;

const TEXT = "Hi from Bedrock";
const KEY = "bedrock-typescript-key";
const MESSAGES_STREAM = [
  { type: "message_start", message: { id: "msg_1", type: "message", role: "assistant", model: "m", content: [], usage: { input_tokens: 3, output_tokens: 0 } } },
  { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
  { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: TEXT } },
  { type: "content_block_stop", index: 0 },
  { type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: 2 } },
  { type: "message_stop" },
];
const CHAT_STREAM = [
  { choices: [{ index: 0, delta: { role: "assistant", content: TEXT } }] },
  { choices: [{ index: 0, delta: {}, finish_reason: "stop" }] },
];

const seen = [];
const server = http.createServer((req, res) => {
  req.resume();
  req.on("end", () => {
    seen.push({ path: req.url, headers: req.headers });
    res.writeHead(200, { "content-type": "text/event-stream", connection: "close" });
    if (req.url.endsWith("/messages")) {
      for (const e of MESSAGES_STREAM) res.write(`event: ${e.type}\ndata: ${JSON.stringify(e)}\n\n`);
      return res.end();
    }
    for (const c of CHAT_STREAM) res.write(`data: ${JSON.stringify(c)}\n\n`);
    res.end("data: [DONE]\n\n");
  });
});
let base;
test.before(async () => {
  if (serverOnly) return;
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  base = `http://127.0.0.1:${server.address().port}/v1`;
});
test.after(() => server.close());

test("Claude uses the Messages API with x-api-key", { skip: serverOnly }, async () => {
  seen.length = 0;
  const model = bedrock("us.anthropic.claude-sonnet-5", { region: "us-east-1", apiKey: KEY, baseUrl: base });
  assert.equal((await new Agent({ model }).run("hi")).text, TEXT);
  assert.equal(seen[0].path, "/v1/messages");
  assert.equal(seen[0].headers["x-api-key"], KEY);
});

test("other models use Chat Completions with a bearer key", { skip: serverOnly }, async () => {
  seen.length = 0;
  const model = bedrock("openai.gpt-oss-120b-1:0", { region: "us-east-1", apiKey: KEY, baseUrl: base });
  assert.equal((await new Agent({ model }).run("hi")).text, TEXT);
  assert.equal(seen[0].path, "/v1/chat/completions");
  assert.equal(seen[0].headers.authorization, `Bearer ${KEY}`);
});

test("strings and options", () => {
  assert.deepEqual(modelSpec("bedrock:openai.gpt-oss-120b-1:0"), { kind: "bedrock", model: "openai.gpt-oss-120b-1:0" });
  assert.throws(() => bedrock("m", { api: "converse" }), /auto/);
  assert.ok(!JSON.stringify(bedrock("m", { apiKey: KEY })).includes(KEY));
});
