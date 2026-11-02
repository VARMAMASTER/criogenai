// `redact`: nothing that leaves for a provider contains the redacted text,
// neither from the user's input nor from a tool result. A local server plays
// an OpenAI-compatible provider and records every request body.

const test = require("node:test");
const assert = require("node:assert/strict");
const http = require("node:http");
const path = require("node:path");

const { Agent, openaiCompatible, tool } = require(process.env.CRIOGEN_SDK || path.join(__dirname, ".."));

const bodies = [];
const server = http.createServer((req, res) => {
  let raw = "";
  req.on("data", (c) => (raw += c));
  req.on("end", () => {
    bodies.push(raw);
    const body = JSON.parse(raw);
    const toolDone = body.messages.some((m) => m.role === "tool");
    const message = toolDone
      ? { role: "assistant", content: "The record is on file." }
      : { role: "assistant", content: null, tool_calls: [{ id: "c1", type: "function", function: { name: "get_record", arguments: "{}" } }] };
    const finish = toolDone ? "stop" : "tool_calls";
    if (body.stream) {
      const delta = message.tool_calls
        ? { role: "assistant", tool_calls: message.tool_calls.map((c, index) => ({ ...c, index })) }
        : { role: "assistant", content: message.content };
      res.writeHead(200, { "content-type": "text/event-stream", connection: "close" });
      res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta }] })}\n\n`);
      res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: finish }] })}\n\n`);
      return res.end("data: [DONE]\n\n");
    }
    const data = JSON.stringify({ choices: [{ index: 0, message, finish_reason: finish }], usage: { prompt_tokens: 1, completion_tokens: 1 } });
    res.writeHead(200, { "content-type": "application/json", "content-length": Buffer.byteLength(data) });
    res.end(data);
  });
});

let base;
test.before(async () => {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  base = `http://127.0.0.1:${server.address().port}/v1`;
});
test.after(() => server.close());

const getRecord = tool({
  name: "get_record",
  description: "The patient's record.",
  input: { type: "object", properties: {}, additionalProperties: false },
  run: () => ({ name: "Asha Rao", mrn: "MRN-884213", note: "Seen on Monday. Call +91 98450 12345." }),
});

// Removes record numbers, phone numbers and the one known name.
const redact = (text) =>
  text
    .replace(/MRN-\d+/g, "[MRN]")
    .replace(/\+?\d[\d ]{8,}\d/g, "[PHONE]")
    .replace(/Asha Rao/g, "[NAME]");

test("redacted text never reaches the provider, from input or tool results", async () => {
  bodies.length = 0;
  const agent = new Agent({ model: openaiCompatible("m", { baseUrl: base }), tools: [getRecord], redact });
  const result = await agent.run("Summarize the record for Asha Rao; her phone is +91 98450 12345.");
  assert.equal(result.text, "The record is on file.");
  assert.equal(bodies.length, 2);
  for (const body of bodies) {
    for (const secret of ["Asha Rao", "MRN-884213", "98450 12345"]) assert.ok(!body.includes(secret), `leaked ${secret}`);
  }
  assert.match(bodies[0], /\[NAME\]/);
  assert.match(bodies[1], /\[MRN\]/); // the tool result, redacted before the second call
  assert.match(bodies[1], /Seen on Monday/); // the rest of the text is kept
});

test("an async redactor works, and a thread saves the redacted transcript", async () => {
  bodies.length = 0;
  const agent = new Agent({
    model: openaiCompatible("m", { baseUrl: base }),
    tools: [getRecord],
    redact: async (text) => redact(text),
  });
  await agent.thread("redact-thread").send("Patient Asha Rao, MRN-884213.");
  const history = JSON.stringify(await agent.thread("redact-thread").history());
  assert.ok(!history.includes("MRN-884213") && !history.includes("Asha Rao"));
  assert.ok(bodies.every((b) => !b.includes("Asha Rao")));
});
