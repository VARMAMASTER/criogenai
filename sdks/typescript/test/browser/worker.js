// A module Web Worker running an agent (design 0021): the engine loads and
// runs off the page's main thread too.
import { Agent, FakeModel, FakeReply, ready, tool } from "/criogen.js";

self.onmessage = async () => {
  try {
    const report = await ready();
    const add = tool({
      name: "add",
      description: "Adds two numbers.",
      input: { type: "object", properties: { a: { type: "number" }, b: { type: "number" } }, required: ["a", "b"] },
      run: ({ a, b }) => a + b,
    });
    const agent = new Agent({ model: new FakeModel([FakeReply.toolCall("add", { a: 20, b: 22 }), "forty-two"]), tools: [add] });
    const result = await agent.run("add them");
    self.postMessage({ ok: report.ok, text: result.text, toolCalls: result.toolCalls });
  } catch (error) {
    self.postMessage({ ok: false, error: String(error?.stack ?? error) });
  }
};
