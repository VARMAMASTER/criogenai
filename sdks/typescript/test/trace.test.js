// The local trace viewer in TypeScript (design 0023): an agent's runs read back from it.

const test = require("node:test");
const assert = require("node:assert/strict");
const path = require("node:path");

const sdk = require(process.env.CRIOGEN_SDK || path.join(__dirname, ".."));
const { Agent, FakeModel, FakeReply, Telemetry, isCriogenError, serveTraces, tool } = sdk;
// The WebAssembly engine has no server sockets: the viewer runs on servers.
const wasm = process.env.CRIOGEN_ENGINE === "wasm";

test("runs sent with Telemetry.local() show up in the viewer", { skip: wasm && "needs a server" }, async () => {
  const viewer = await serveTraces({ port: 0, db: ":memory:", prices: { fake: { input: 1, output: 2 } } });
  try {
    assert.match(viewer.url, /^http:\/\/127\.0\.0\.1:\d+$/);
    const telemetry = Telemetry.local({ url: viewer.url });
    const weather = tool({
      name: "weather",
      description: "Today's forecast.",
      input: { type: "object", properties: { city: { type: "string" } }, required: ["city"] },
      run: ({ city }) => `${city}: sunny`,
    });
    const agent = new Agent({ model: new FakeModel([FakeReply.toolCall("weather", { city: "Lisbon" }), "Sunny."]), tools: [weather], telemetry });
    await agent.run("Weather in Lisbon?");
    await telemetry.flush();
    const { runs } = await (await fetch(`${viewer.url}/api/runs`)).json();
    assert.equal(runs.length, 1);
    assert.equal(runs[0].model_calls, 2);
    assert.equal(runs[0].tool_calls, 1);
    assert.notEqual(runs[0].cost, null);
    const detail = await (await fetch(`${viewer.url}/api/runs/${runs[0].id}`)).json();
    const kinds = new Set(detail.spans.map((s) => s.kind));
    assert.ok(kinds.has("run") && kinds.has("model") && kinds.has("tool"));
    await telemetry.shutdown();
  } finally {
    await viewer.stop();
  }
});

test("another address needs a token", { skip: wasm && "needs a server" }, async () => {
  await assert.rejects(serveTraces({ port: 0, host: "0.0.0.0", db: ":memory:" }), (e) => isCriogenError(e) && /token/.test(e.message));
});
