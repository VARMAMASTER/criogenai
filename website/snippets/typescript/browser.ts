// The browser guide's samples (content/docs/guides/browser.mdx): the engine
// as WebAssembly (design 0021). Type-checked by scripts/check-snippets.sh.
import { Agent, FakeReply, FunctionModel, Gateway, openai, ready, tool } from "criogen/web";

export async function basics(apiKey: string) {
  // [show]
  // npm install criogen, then in the browser (or a Web Worker):
  // import { Agent, openai, ready, tool } from "criogen/web";

  await ready(); // loads the engine (WebAssembly) once; call it before anything else

  const weather = tool({
    name: "weather",
    description: "Today's forecast for a city.",
    input: { type: "object", properties: { city: { type: "string" } }, required: ["city"] },
    run: ({ city }: { city: string }) => `${city}: 24 °C, sunny`,
  });
  // A key typed by the page's own user: it goes from their tab to the provider only.
  const agent = new Agent({ model: openai("gpt-5-mini", { apiKey }), tools: [weather] });
  const result = await agent.run("Picnic weather in Lisbon?");
  console.log(result.text);
  // [/show]
}

export async function functionModel() {
  // [show function-model]
  // A model written in JavaScript: a model running in the page, your own
  // client, or scripted replies for tests and demos.
  const model = new FunctionModel(async (call) => {
    const result = call.messages.find((m) => m.role === "tool");
    return result ? "It is sunny: go." : FakeReply.toolCall("weather", { city: "Lisbon" });
  });
  // [/show function-model]
  return model;
}

export async function forward(upstreamKey: string) {
  // [show forward]
  // Pass every request on to a real model, tools and all, and watch it.
  const gateway = new Gateway();
  const upstream = openai("gpt-5-mini", { apiKey: upstreamKey });
  const watched = new FunctionModel(async (call) => {
    console.log("the model is asked", call.messages.length, "messages");
    const reply = await gateway.chat(upstream, call.messages, { tools: call.tools });
    return { message: reply.message };
  });
  // [/show forward]
  return watched;
}

export async function locate() {
  // [show locate]
  // The engine file sits next to the script by default; elsewhere, say where:
  await ready({ wasm: new URL("/assets/criogen_bg.wasm", location.href) });
  // [/show locate]
}
