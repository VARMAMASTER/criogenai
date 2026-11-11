// Azure OpenAI and Foundry (design 0006 §17) from TypeScript, against a local
// server that plays both Entra ID and the Azure resource. No network.

const test = require("node:test");
const assert = require("node:assert/strict");
const http = require("node:http");
const path = require("node:path");

const sdk = require(process.env.CRIOGEN_SDK || path.join(__dirname, ".."));
const { Agent, azure } = sdk;
const { modelSpec } = require(process.env.CRIOGEN_SDK ? `${process.env.CRIOGEN_SDK}/dist/models` : "../dist/models");

// The engine in the browser cannot listen; these need a server.
const serverOnly = process.env.CRIOGEN_ENGINE === "wasm" ? "server only: a local Entra ID and Azure server" : false;

const TEXT = "Hi from Azure";
const SECRET = "typescript-client-secret-value";
const seen = [];

const server = http.createServer((req, res) => {
  let raw = "";
  req.on("data", (c) => (raw += c));
  req.on("end", () => {
    seen.push({ path: req.url, headers: req.headers, raw });
    const json = (status, value) => {
      const data = JSON.stringify(value);
      res.writeHead(status, { "content-type": "application/json", "content-length": Buffer.byteLength(data) });
      res.end(data);
    };
    if (req.url.endsWith("/oauth2/v2.0/token")) {
      const form = new URLSearchParams(raw);
      if (form.get("client_secret") !== SECRET) return json(401, { error: "invalid_client", error_description: "bad secret" });
      return json(200, { access_token: "entra-token", expires_in: 3599 });
    }
    if (req.url === "/openai/v1/responses") {
      const body = JSON.parse(raw);
      if (!body.stream) {
        return json(200, { status: "completed", output: [{ type: "message", content: [{ type: "output_text", text: TEXT }] }] });
      }
      res.writeHead(200, { "content-type": "text/event-stream", connection: "close" });
      for (const event of [
        { type: "response.output_item.added", output_index: 0, item: { type: "message", content: [] } },
        { type: "response.output_text.delta", output_index: 0, content_index: 0, delta: TEXT },
        { type: "response.completed", response: { status: "completed" } },
      ]) {
        res.write(`event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`);
      }
      return res.end();
    }
    json(404, { error: { message: "unknown path" } });
  });
});
let origin;
test.before(async () => {
  if (serverOnly) return;
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  origin = `http://127.0.0.1:${server.address().port}`;
});
test.after(() => server.close());

test("an API key goes in the api-key header", { skip: serverOnly }, async () => {
  seen.length = 0;
  const result = await new Agent({ model: azure("my-deployment", { endpoint: origin, apiKey: "azure-key-123" }) }).run("hi");
  assert.equal(result.text, TEXT);
  assert.equal(seen.length, 1);
  assert.equal(seen[0].path, "/openai/v1/responses");
  assert.equal(seen[0].headers["api-key"], "azure-key-123");
  assert.equal(seen[0].headers.authorization, undefined);
});

test("a service principal gets a bearer token", { skip: serverOnly }, async () => {
  seen.length = 0;
  const model = azure("gpt-5-mini", { endpoint: origin, tenantId: "my-tenant", clientId: "app", clientSecret: SECRET, authority: origin });
  assert.equal((await new Agent({ model }).run("hi")).text, TEXT);
  assert.equal(seen[0].path, "/my-tenant/oauth2/v2.0/token");
  assert.equal(seen[1].headers.authorization, "Bearer entra-token");
});

test("a bad secret is an auth error without the secret", { skip: serverOnly }, async () => {
  const model = azure("gpt-5-mini", { endpoint: origin, tenantId: "t", clientId: "c", clientSecret: "wrong-secret-value", authority: origin });
  await assert.rejects(new Agent({ model }).run("hi"), (err) => {
    assert.ok(!String(err.message).includes("wrong-secret-value"), err.message);
    return true;
  });
  assert.ok(!JSON.stringify(model).includes("wrong-secret-value"));
});

test("strings, specs and options", () => {
  assert.deepEqual(modelSpec("azure:gpt-5-mini"), { kind: "azure", model: "gpt-5-mini" });
  const chat = azure("DeepSeek-V4", { endpoint: "my-res", api: "chat", managedIdentity: true }).spec();
  assert.equal(chat.api, "chat");
  assert.equal(chat.managedIdentity, true);
  assert.throws(() => azure("m", { api: "completions" }), /responses/);
});
