// Kaggle's Model Proxy (design 0020) from TypeScript, against a local server
// that plays both Kaggle's API and the proxy. No network.

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const http = require("node:http");
const os = require("node:os");
const path = require("node:path");

const sdk = require(process.env.CRIOGEN_SDK || path.join(__dirname, ".."));
const { Agent, kaggle, tool } = sdk;

const API_TOKEN = "KGAT_typescript_test_token";
const seen = [];

const server = http.createServer((req, res) => {
  let raw = "";
  req.on("data", (c) => (raw += c));
  req.on("end", () => {
    const body = raw ? JSON.parse(raw) : {};
    const auth = req.headers.authorization;
    seen.push({ path: req.url, auth, body });
    const send = (status, value) => {
      const data = JSON.stringify(value);
      res.writeHead(status, { "content-type": "application/json", "content-length": Buffer.byteLength(data) });
      res.end(data);
    };
    const host = `http://127.0.0.1:${server.address().port}`;
    if (req.url.endsWith("/CreateDefaultModelProxyToken")) {
      if (auth !== `Bearer ${API_TOKEN}`) return send(401, { message: "bad token" });
      return send(200, { baseUri: `${host}/models`, token: "proxy-1", expiryTime: "2999-01-01T00:00:00Z" });
    }
    if (req.url === "/models/openapi/chat/completions") {
      if (auth !== "Bearer proxy-1") return send(401, { error: { message: "bad proxy token" } });
      const toolResults = body.messages.filter((m) => m.role === "tool");
      const message =
        body.tools && toolResults.length === 0
          ? {
              role: "assistant",
              content: null,
              tool_calls: [{ id: "c1", type: "function", function: { name: "add", arguments: '{"a":2,"b":3}' } }],
            }
          : { role: "assistant", content: `answer from ${body.model}` };
      const finish = message.tool_calls ? "tool_calls" : "stop";
      if (body.stream) {
        // The same answer as a stream, as the real proxy sends it.
        const delta = message.tool_calls
          ? { role: "assistant", tool_calls: message.tool_calls.map((c, index) => ({ ...c, index })) }
          : { role: "assistant", content: message.content };
        res.writeHead(200, { "content-type": "text/event-stream", connection: "close" });
        res.write(`data: ${JSON.stringify({ model: body.model, choices: [{ index: 0, delta }] })}\n\n`);
        const last = {
          model: body.model,
          choices: [{ index: 0, delta: {}, finish_reason: finish }],
          usage: { prompt_tokens: 3, completion_tokens: 2, total_tokens: 5 },
        };
        res.write(`data: ${JSON.stringify(last)}\n\n`);
        return res.end("data: [DONE]\n\n");
      }
      return send(200, {
        id: "x",
        object: "chat.completion",
        model: body.model,
        choices: [{ index: 0, message, finish_reason: message.tool_calls ? "tool_calls" : "stop" }],
        usage: { prompt_tokens: 3, completion_tokens: 2, total_tokens: 5 },
      });
    }
    send(404, { message: "not found" });
  });
});

let base;
test.before(async () => {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  base = `http://127.0.0.1:${server.address().port}`;
});
test.after(() => server.close());

const add = tool({
  name: "add",
  description: "Adds two numbers.",
  input: { type: "object", properties: { a: { type: "integer" }, b: { type: "integer" } }, required: ["a", "b"] },
  run: ({ a, b }) => a + b,
});

test("an agent calls a Kaggle model with tools; one token serves both calls", async () => {
  seen.length = 0;
  const agent = new Agent({ model: kaggle("google/gemini-3.6-flash", { apiToken: API_TOKEN, apiBase: base }), tools: [add] });
  const result = await agent.run("What is 2 + 3?");
  assert.equal(result.text, "answer from google/gemini-3.6-flash");
  assert.equal(seen.filter((s) => s.path.endsWith("/CreateDefaultModelProxyToken")).length, 1);
  assert.ok(seen.at(-1).body.messages.some((m) => m.role === "tool"));
});

test("a pre-minted proxy token works, and JSON never shows a token", async () => {
  const model = kaggle("openai/gpt-oss-120b", { proxyUrl: `${base}/models/openapi`, proxyToken: "proxy-1" });
  assert.ok(!JSON.stringify(model).includes("proxy-1"));
  const result = await new Agent({ model }).run("hi");
  assert.equal(result.text, "answer from openai/gpt-oss-120b");
});

test("without credentials the error says what to set; the token file is found", async () => {
  const saved = { ...process.env };
  try {
    for (const name of ["MODEL_PROXY_URL", "MODEL_PROXY_API_KEY", "KAGGLE_API_TOKEN"]) delete process.env[name];
    const empty = fs.mkdtempSync(path.join(os.tmpdir(), "criogen-kaggle-"));
    process.env.KAGGLE_CONFIG_DIR = empty;
    // Building the agent fails at once, naming what to set.
    assert.throws(() => new Agent({ model: "kaggle:google/gemini-3.6-flash" }), /KAGGLE_API_TOKEN/);
    fs.writeFileSync(path.join(empty, "access_token"), `${API_TOKEN}\n`);
    const result = await new Agent({ model: kaggle("ibm/granite-4.0-h-small", { apiBase: base }) }).run("hi");
    assert.equal(result.text, "answer from ibm/granite-4.0-h-small");
  } finally {
    for (const k of Object.keys(process.env)) if (!(k in saved)) delete process.env[k];
    Object.assign(process.env, saved);
  }
});
