// Gateway (design 0018): model calls without an agent, simulated models only.
// Runs against the source build, or the installed package when
// CRIOGEN_SDK names it (scripts/test-typescript.sh).

const test = require("node:test");
const assert = require("node:assert/strict");
const path = require("node:path");

const sdk = require(process.env.CRIOGEN_SDK || path.join(__dirname, ".."));
const { CriogenError, CancelledError, Embedder, FakeModel, Gateway, ModelError, ValidationError } = sdk;

const fast = { initialBackoffSecs: 0.01, jitter: false };

test("chat with text or messages, and instructions", async () => {
  const gw = new Gateway();
  const model = new FakeModel(["Paris.", "Second answer."]);
  const reply = await gw.chat(model, "Capital of France?", { instructions: "Answer in one word." });
  assert.equal(reply.text, "Paris.");
  assert.equal(reply.message.role, "assistant");
  assert.equal(reply.model, "fake:fake");
  assert.equal(reply.stopReason, "stop");
  // The same model object goes on with its script; content may be a string.
  const next = await gw.chat(model, [
    { role: "user", content: "Hi" },
    { role: "assistant", content: [{ type: "text", text: "Hello" }] },
    { role: "user", content: "And now?" },
  ]);
  assert.equal(next.text, "Second answer.");
});

test("retries and fallbacks, as for an agent", async () => {
  const flaky = new FakeModel(["after a retry"], { failFirst: 1 });
  assert.equal((await new Gateway({ retry: fast }).chat(flaky, "hi")).text, "after a retry");

  const gw = new Gateway({ fallbacks: [new FakeModel(["from the backup"], { name: "backup" })], retry: fast });
  const reply = await gw.chat(new FakeModel([], { failFirst: 10, name: "primary" }), "hi");
  assert.equal(reply.text, "from the backup");
  assert.equal(reply.model, "fake:backup");

  const failing = new Gateway({ retry: { ...fast, maxAttempts: 2 } });
  await assert.rejects(failing.chat(new FakeModel([], { failFirst: 10 }), "hi"), (e) => {
    assert.ok(e instanceof ModelError);
    assert.equal(e.code, "model_failed");
    assert.equal(e.details.model_error.kind, "rate_limited");
    return true;
  });
});

test("JSON output is parsed and checked", async () => {
  const weather = {
    type: "object",
    properties: { city: { type: "string" }, temp: { type: "integer" } },
    required: ["city", "temp"],
  };
  const gw = new Gateway();
  const reply = await gw.chat(new FakeModel(['{"city": "Oslo", "temp": 3}']), "Weather?", { output: weather });
  assert.deepEqual(reply.output, { city: "Oslo", temp: 3 });
  await assert.rejects(gw.chat(new FakeModel(['{"city": "Oslo"}']), "Weather?", { output: weather }), (e) => {
    assert.ok(e instanceof CriogenError);
    assert.equal(e.code, "output_invalid");
    return true;
  });
  await assert.rejects(gw.chat(new FakeModel(["not json"]), "Weather?", { output: { type: "object" } }), CriogenError);
});

test("a stream's chunks add up to the reply", async () => {
  const stream = new Gateway().stream(new FakeModel(["Hello there."]), "hi");
  let text = "";
  for await (const chunk of stream) {
    assert.equal(chunk.type, "text");
    text += chunk.text;
  }
  const reply = await stream.reply();
  assert.equal(text, "Hello there.");
  assert.equal(reply.text, "Hello there.");
});

test("an AbortSignal cancels a call", async () => {
  const controller = new AbortController();
  const started = Date.now();
  const call = new Gateway().chat(new FakeModel(["late"], { delayMs: 30000 }), "hi", { signal: controller.signal });
  setTimeout(() => controller.abort(), 50);
  await assert.rejects(call, CancelledError);
  assert.ok(Date.now() - started < 5000);
});

test("embeddings, and bad requests", async () => {
  const gw = new Gateway();
  const vectors = await gw.embed(Embedder.fake(), ["first text", "second text"]);
  assert.equal(vectors.length, 2);
  assert.ok(vectors[0].length > 0 && vectors[0].length === vectors[1].length);
  assert.throws(() => gw.stream("nosuch:model", "hi"), TypeError);
  await assert.rejects(gw.chat(new FakeModel(["x"]), [{ role: "wizard", content: "hi" }]), ValidationError);
});
