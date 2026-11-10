// Named providers (design 0006 §4.1): the SDK's key table matches the
// engine's, every name works as a string and as a factory, and
// anthropicCompatible speaks the Messages API. A local server only.

const test = require("node:test");
const assert = require("node:assert/strict");
const http = require("node:http");
const path = require("node:path");

const root = process.env.CRIOGEN_SDK || path.join(__dirname, "..");
const sdk = require(root);
const { KEYS, modelSpec } = require(path.join(root, "dist", "models.js"));
const { native } = require(path.join(root, "dist", "native.js"));
const { Agent, anthropicCompatible, deepseek, envStatus, keyName } = sdk;

const PRESETS = JSON.parse(native.presets());
const TEXT = "Hi from the server";

const OPENAI_STREAM = [
  [null, { choices: [{ index: 0, delta: { role: "assistant", content: TEXT } }] }],
  [null, { choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 3, completion_tokens: 2 } }],
];
// The OpenAI Responses API (design 0006 §16): what `openai` speaks.
const RESPONSES_STREAM = [
  ["response.created", { type: "response.created", response: { id: "resp_1", model: "m" } }],
  ["response.output_item.added", { type: "response.output_item.added", output_index: 0, item: { type: "message", content: [] } }],
  ["response.output_text.delta", { type: "response.output_text.delta", output_index: 0, content_index: 0, delta: TEXT }],
  ["response.completed", { type: "response.completed", response: { status: "completed", usage: { input_tokens: 3, output_tokens: 2 } } }],
];
const ANTHROPIC_STREAM = [
  ["message_start", { type: "message_start", message: { id: "msg_1", type: "message", role: "assistant", model: "m", content: [], usage: { input_tokens: 3, output_tokens: 0 } } }],
  ["content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } }],
  ["content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: TEXT } }],
  ["content_block_stop", { type: "content_block_stop", index: 0 }],
  ["message_delta", { type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: 2 } }],
  ["message_stop", { type: "message_stop" }],
];

const seen = [];
const server = http.createServer((req, res) => {
  let raw = "";
  req.on("data", (c) => (raw += c));
  req.on("end", () => {
    seen.push({ path: req.url, auth: req.headers.authorization, xApiKey: req.headers["x-api-key"], body: JSON.parse(raw) });
    const anthropic = req.url.endsWith("/messages");
    const responses = req.url.endsWith("/responses");
    res.writeHead(200, { "content-type": "text/event-stream" });
    const script = anthropic ? ANTHROPIC_STREAM : responses ? RESPONSES_STREAM : OPENAI_STREAM;
    for (const [event, data] of script) {
      res.write(`${event ? `event: ${event}\n` : ""}data: ${JSON.stringify(data)}\n\n`);
    }
    res.end(anthropic || responses ? "" : "data: [DONE]\n\n");
  });
});
let base;
test.before(async () => {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  base = `http://127.0.0.1:${server.address().port}/v1`;
});
test.after(() => server.close());

const saved = { ...process.env };
test.afterEach(() => {
  for (const name of Object.values(KEYS)) {
    if (saved[name] === undefined) delete process.env[name];
    else process.env[name] = saved[name];
  }
});

test("the key table matches the engine", () => {
  const engine = Object.fromEntries(PRESETS.filter((p) => p.keyEnv).map((p) => [p.name, p.keyEnv]));
  assert.deepEqual(KEYS, engine);
  assert.deepEqual(PRESETS.filter((p) => !p.keyEnv).map((p) => p.name).sort(), ["lmstudio", "ollama"]);
});

test("every named provider works as a string", () => {
  for (const p of PRESETS) {
    if (p.keyEnv) process.env[p.keyEnv] = "sk-test-key";
    const spec = modelSpec(`${p.name}:some/model`);
    assert.equal(spec.kind, p.name);
    assert.equal(spec.model, "some/model");
    if (p.keyEnv) assert.equal(spec.apiKey, "sk-test-key");
  }
});

test("every named provider answers through its factory", async () => {
  for (const p of PRESETS) {
    const factory = sdk[p.name];
    assert.equal(typeof factory, "function", p.name);
    seen.length = 0;
    const result = await new Agent({ model: factory("m", { apiKey: "sk-test-key", baseUrl: base }) }).run("hi");
    assert.equal(result.text, TEXT, p.name);
    assert.equal(seen.length, 1, p.name);
    if (p.anthropic) {
      assert.equal(seen[0].path, "/v1/messages");
      assert.equal(seen[0].xApiKey, "sk-test-key");
    } else {
      // `openai` speaks the Responses API; the others Chat Completions.
      assert.equal(seen[0].path, p.name === "openai" ? "/v1/responses" : "/v1/chat/completions", p.name);
      assert.equal(seen[0].auth, "Bearer sk-test-key", p.name);
    }
  }
});

test("a missing key names the variable; an unknown provider lists the known ones", () => {
  delete process.env.DEEPSEEK_API_KEY;
  assert.throws(() => new Agent({ model: "deepseek:deepseek-chat" }), /DEEPSEEK_API_KEY/);
  assert.throws(() => new Agent({ model: "azure:gpt-5" }), /provider one of/);
});

test("status and key names cover the new providers", () => {
  for (const name of Object.values(KEYS)) delete process.env[name];
  process.env.GROQ_API_KEY = "g";
  const status = envStatus();
  assert.equal(status.groq, true);
  assert.equal(status.deepseek, false);
  assert.equal(keyName("qwen"), "DASHSCOPE_API_KEY");
});

test("anthropicCompatible speaks the Messages API", async () => {
  seen.length = 0;
  const result = await new Agent({ model: anthropicCompatible("glm-5.2", { baseUrl: base, apiKey: "sk-test-key" }) }).run("hi");
  assert.equal(result.text, TEXT);
  assert.equal(seen[0].path, "/v1/messages");
  assert.equal(seen[0].xApiKey, "sk-test-key");
  assert.equal(seen[0].auth, undefined);
  assert.equal(seen[0].body.model, "glm-5.2");
});

test("a model shows no key when printed", () => {
  const model = deepseek("deepseek-chat", { apiKey: "sk-secret-123456" });
  assert.doesNotMatch(JSON.stringify(model), /sk-secret/);
});
